//! `nes-web --mcp`: a stdio MCP server that lets an AI agent play Pokémon PvP on the arena.
//!
//! It holds ONE token-authenticated WebSocket to the arena (the same `/ws` protocol the browser
//! uses) and exposes the gameplay as MCP tools. Config via env: `NES_TOKEN` (the player's agent
//! token) + `NES_URL` (default http://localhost:3000). The arena stays the source of truth — this
//! adds no game logic, it only bridges tool calls <-> WebSocket messages.
//!
//! STDIO GOTCHA: stdout is the JSON-RPC channel, so every log here goes to STDERR.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;

/// Live arena state, fed by the WebSocket event stream.
#[derive(Default)]
struct St {
    phase: String, // idle | queued | matched | in_battle | ended
    room: Option<String>,
    seat: Option<u64>,
    opponent: Option<String>,
    you_dex: Option<u64>,
    opp_dex: Option<u64>,
    you: Option<Value>, // {species,hp,max_hp,status}
    opp: Option<Value>,
    moves: Vec<Value>, // [{slot,id,pp}]
    my_turn: bool,
    result: Option<String>,
    connected: bool,
}

#[derive(Clone)]
pub struct Arena {
    state: Arc<Mutex<St>>,
    notify: Arc<Notify>,
    tx: mpsc::UnboundedSender<String>, // JSON messages to send over the WS
    base: String,
    names: Arc<HashMap<u64, String>>, // internal species index -> NAME
    tool_router: ToolRouter<Arena>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct MoveArgs {
    /// move slot 0-3, taken from the moves list shown by wait_turn / get_state
    slot: u8,
}

fn text(s: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s)])
}

#[tool_router]
impl Arena {
    fn new(base: String, tx: mpsc::UnboundedSender<String>, names: HashMap<u64, String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(St { phase: "idle".into(), ..Default::default() })),
            notify: Arc::new(Notify::new()),
            tx,
            base,
            names: Arc::new(names),
            tool_router: Self::tool_router(),
        }
    }

    /// Await until `pred(state)` holds, or `secs` elapse. Lost-wakeup-safe (registers before checking).
    async fn wait_until<F: Fn(&St) -> bool>(&self, pred: F, secs: u64) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if pred(&*self.state.lock().await) {
                return true;
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => return pred(&*self.state.lock().await),
            }
        }
    }

    fn name_of(&self, idx: u64) -> String {
        self.names.get(&idx).cloned().unwrap_or_else(|| format!("#{idx}"))
    }

    async fn state_text(&self) -> String {
        let s = self.state.lock().await;
        let mut lines = vec![format!("phase: {}", s.phase)];
        if let Some(r) = &s.room {
            lines.push(format!(
                "room: {} (seat {}, vs {})",
                r,
                s.seat.unwrap_or(0),
                s.opponent.clone().unwrap_or_default()
            ));
        }
        let mon = |label: &str, m: &Option<Value>, dex: Option<u64>| -> String {
            match m {
                Some(v) => {
                    let sp = v.get("species").and_then(|x| x.as_u64()).unwrap_or(0);
                    let hp = v.get("hp").and_then(|x| x.as_i64()).unwrap_or(0);
                    let mh = v.get("max_hp").and_then(|x| x.as_i64()).unwrap_or(0);
                    let dexs = dex.map(|d| format!(" (dex {d})")).unwrap_or_default();
                    format!("{label}: {}{dexs}  HP {hp}/{mh}", self.name_of(sp))
                }
                None => format!("{label}: (unknown)"),
            }
        };
        if s.phase == "in_battle" || s.you.is_some() {
            lines.push(mon("YOU", &s.you, s.you_dex));
            lines.push(mon("FOE", &s.opp, s.opp_dex));
            lines.push(if s.my_turn {
                "It is YOUR turn — pick a move with make_move.".into()
            } else {
                "Waiting for the turn...".into()
            });
            if !s.moves.is_empty() {
                let ms: Vec<String> = s
                    .moves
                    .iter()
                    .map(|m| {
                        format!(
                            "slot {} (move id {}, pp {})",
                            m.get("slot").and_then(|x| x.as_u64()).unwrap_or(0),
                            m.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
                            m.get("pp").and_then(|x| x.as_u64()).unwrap_or(0)
                        )
                    })
                    .collect();
                lines.push(format!("moves: {}", ms.join("; ")));
            }
        }
        if s.phase == "ended" {
            let r = s.result.clone().unwrap_or_default();
            let label = if r == "won" { "YOU WON" } else if r == "lost" { "you lost" } else { "battle ended" };
            lines.push(format!("result: {label}"));
        }
        lines.join("\n")
    }

    #[tool(description = "Queue for a Pokémon PvP match and wait until matched. Returns your Pokémon and the opponent.")]
    async fn find_match(&self) -> Result<CallToolResult, McpError> {
        {
            let s = self.state.lock().await;
            if s.phase == "in_battle" || s.phase == "matched" {
                drop(s);
                return Ok(text(format!("Already in a match.\n{}", self.state_text().await)));
            }
        }
        let _ = self.tx.send(json!({"type":"find_match"}).to_string());
        let ok = self.wait_until(|s| s.phase == "matched" || s.phase == "in_battle", 45).await;
        Ok(text(if ok {
            format!("Matched!\n{}", self.state_text().await)
        } else {
            "Still searching (no opponent yet). Call status or find_match again.".into()
        }))
    }

    #[tool(description = "Block until it's your turn to move OR the battle ends. Returns the battle state + move options, or the final result.")]
    async fn wait_turn(&self) -> Result<CallToolResult, McpError> {
        let ok = self.wait_until(|s| (s.my_turn && s.phase == "in_battle") || s.phase == "ended", 60).await;
        if !ok {
            return Ok(text(format!("Timed out waiting for your turn.\n{}", self.state_text().await)));
        }
        let ended = self.state.lock().await.phase == "ended";
        let head = if ended { "BATTLE OVER." } else { "YOUR TURN." };
        Ok(text(format!("{head}\n{}", self.state_text().await)))
    }

    #[tool(description = "Use one of your Pokémon's moves by slot (0-3). Call wait_turn first to see the options.")]
    async fn make_move(&self, Parameters(MoveArgs { slot }): Parameters<MoveArgs>) -> Result<CallToolResult, McpError> {
        {
            let mut s = self.state.lock().await;
            if s.phase == "ended" {
                return Ok(text("The battle is over.".into()));
            }
            s.my_turn = false;
        }
        let _ = self.tx.send(json!({"type":"commit_move","slot":slot}).to_string());
        Ok(text(format!("Move (slot {slot}) submitted. Call wait_turn for the next turn or the result.")))
    }

    #[tool(description = "Current battle state as text (your Pokémon + HP + moves, the foe, whose turn).")]
    async fn get_state(&self) -> Result<CallToolResult, McpError> {
        Ok(text(self.state_text().await))
    }

    #[tool(description = "Quick status: idle / queued / matched / in_battle / ended, plus the room id.")]
    async fn status(&self) -> Result<CallToolResult, McpError> {
        let s = self.state.lock().await;
        Ok(text(
            json!({"phase":s.phase,"room":s.room,"seat":s.seat,"opponent":s.opponent,"result":s.result,"connected":s.connected})
                .to_string(),
        ))
    }

    #[tool(description = "A spectator URL a human can open to watch your current battle live.")]
    async fn watch_link(&self) -> Result<CallToolResult, McpError> {
        let s = self.state.lock().await;
        Ok(text(match &s.room {
            Some(r) => format!("{}/room?id={}", self.base, r),
            None => "No active battle yet — call find_match first.".into(),
        }))
    }

    #[tool(description = "The arena leaderboard (top trainers by wins) for today / weekly / monthly.")]
    async fn ranking(&self) -> Result<CallToolResult, McpError> {
        let d: Value = match reqwest::get(format!("{}/api/ranking", self.base)).await {
            Ok(r) => r.json().await.unwrap_or(Value::Null),
            Err(_) => return Ok(text("Could not fetch ranking.".into())),
        };
        let fmt = |k: &str| -> String {
            d.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .take(5)
                        .enumerate()
                        .map(|(i, e)| {
                            format!(
                                "{}. {} ({})",
                                i + 1,
                                e.get("name").and_then(|x| x.as_str()).unwrap_or("?"),
                                e.get("wins").and_then(|x| x.as_i64()).unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("  ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(none)".into())
        };
        Ok(text(format!("TODAY: {}\nWEEKLY: {}\nMONTHLY: {}", fmt("today"), fmt("weekly"), fmt("monthly"))))
    }
}

#[tool_handler]
impl ServerHandler for Arena {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive] -> mutate a default instead of a struct literal.
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::V_2024_11_05;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info.instructions = Some(
            "Play Pokémon PvP: call find_match, then loop wait_turn -> make_move until the battle \
             is over. get_state shows your mon/HP/moves; watch_link gives a spectator URL; ranking \
             shows the leaderboard."
                .into(),
        );
        info
    }
}

fn apply_event(s: &mut St, m: &Value) {
    match m.get("type").and_then(|t| t.as_str()) {
        Some("queued") => s.phase = "queued".into(),
        Some("matched") => {
            s.phase = "matched".into();
            s.room = m.get("room_id").and_then(|x| x.as_str()).map(String::from);
            s.seat = m.get("seat").and_then(|x| x.as_u64());
            s.opponent = m.get("opponent").and_then(|x| x.as_str()).map(String::from);
            s.result = None;
        }
        Some("slot_result") => {
            s.you_dex = m.get("you_dex").and_then(|x| x.as_u64());
            s.opp_dex = m.get("opp_dex").and_then(|x| x.as_u64());
        }
        Some("battle_state") => {
            s.phase = "in_battle".into();
            s.you = m.get("you").cloned();
            s.opp = m.get("opp").cloned();
        }
        Some("your_turn") => {
            s.my_turn = true;
            s.phase = "in_battle".into();
            s.moves = m.get("moves").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        }
        Some("winner") => {
            s.phase = "ended".into();
            s.result = Some(if m.get("you_won").and_then(|x| x.as_bool()).unwrap_or(false) {
                "won".into()
            } else {
                "lost".into()
            });
            s.my_turn = false;
        }
        Some("room_closed") => {
            if s.phase != "ended" {
                s.phase = "ended".into();
                if s.result.is_none() {
                    s.result = Some("ended".into());
                }
            }
            s.my_turn = false;
        }
        _ => {}
    }
}

/// Entry point for `nes-web --mcp`.
pub async fn run() -> anyhow::Result<()> {
    let base = std::env::var("NES_URL")
        .unwrap_or_else(|_| "http://localhost:3000".into())
        .trim_end_matches('/')
        .to_string();
    let token = std::env::var("NES_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!("[nes-mcp] WARNING: NES_TOKEN is empty; the arena will reject the WebSocket.");
    }

    // species index -> name, for readable battle text (best-effort)
    let mut names = HashMap::new();
    if let Ok(r) = reqwest::get(format!("{base}/api/species")).await {
        if let Ok(list) = r.json::<Vec<Value>>().await {
            for sp in list {
                if let (Some(i), Some(n)) =
                    (sp.get("index").and_then(|x| x.as_u64()), sp.get("name").and_then(|x| x.as_str()))
                {
                    names.insert(i, n.to_string());
                }
            }
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let arena = Arena::new(base.clone(), tx, names);
    let state = arena.state.clone();
    let notify = arena.notify.clone();

    // Persistent token-authed WebSocket: pump rx -> ws (send) and ws -> state (recv). Reconnect on drop.
    let ws_url = base.replacen("http", "ws", 1) + "/ws?token=" + &token;
    tokio::spawn(async move {
        loop {
            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws, _)) => {
                    eprintln!("[nes-mcp] ws connected");
                    state.lock().await.connected = true;
                    let (mut wtx, mut wrx) = ws.split();
                    loop {
                        tokio::select! {
                            out = rx.recv() => match out {
                                Some(s) => { if wtx.send(Message::Text(s.into())).await.is_err() { break; } }
                                None => return,
                            },
                            inc = wrx.next() => match inc {
                                Some(Ok(Message::Text(t))) => {
                                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                        let mut g = state.lock().await;
                                        apply_event(&mut g, &v);
                                        drop(g);
                                        notify.notify_waiters();
                                    }
                                }
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                        }
                    }
                    state.lock().await.connected = false;
                    eprintln!("[nes-mcp] ws closed; reconnecting in 1s");
                }
                Err(e) => eprintln!("[nes-mcp] ws connect error: {e}; retry in 1s"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    eprintln!("[nes-mcp] MCP server ready on stdio; arena = {base}");
    let service = arena.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

use rmcp::transport::stdio;
