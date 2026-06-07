//! Scalable mode: a COORDINATOR process that owns auth/lobby/matchmaking + a pool of EPHEMERAL
//! emulator WORKER processes (each = this same binary with `--worker`, its own emulator, sharing
//! the DB).
//!
//! Lifecycle: one worker per battle. When two players are paired the coordinator **spawns a fresh
//! worker on demand**, waits for it to boot, tells it to run the battle (`POST /internal/assign`),
//! and notifies both players. `GET /room?id=<uuid>` then **redirects** to that worker — WebRTC video
//! + the battle WS go browser↔worker directly (localhost shares cookies across ports, so the
//! session still authenticates against the shared DB). When the battle ends the coordinator **kills
//! the worker process** to free its emulator/CPU/RAM. Concurrency = number of live workers, capped
//! by `MAX_WORKERS` (or `--workers N`); **unbounded if neither is set**. Backwards-compatible: with
//! no `--coordinator`, the binary is the solo arena.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::rooms::{self, UserId};
use crate::signaling::AppState;

struct Worker {
    port: u16,
    child: Child,
    public_id: String, // "" until /internal/assign succeeds; then the battle's UUID
}

pub struct WorkerPool {
    http: reqwest::Client,
    secret: String,
    workers: Mutex<Vec<Worker>>,
    base_port: u16,
    /// Max concurrent workers/battles. `usize::MAX` = unbounded (the default).
    max: usize,
    // Spawn config for new worker processes.
    exe: PathBuf,
    rom: String,
    core: String,
    db_url: String,
    coord_origin: String,
}

impl WorkerPool {
    pub fn new(
        base_port: u16,
        max: usize,
        rom: String,
        core: String,
        db_url: String,
        coord_origin: String,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            secret: rooms::gen_uuid(),
            workers: Mutex::new(Vec::new()),
            base_port,
            max,
            exe: std::env::current_exe().unwrap_or_default(),
            rom,
            core,
            db_url,
            coord_origin,
        }
    }

    /// Port of the worker currently running `public_id` (used by the /room redirect).
    pub fn worker_for(&self, public_id: &str) -> Option<u16> {
        self.workers
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.public_id == public_id)
            .map(|w| w.port)
    }

    fn count(&self) -> usize {
        self.workers.lock().unwrap().len()
    }

    /// Spawn a fresh worker process; returns its port (added to the pool, public_id still "").
    /// Called only from the single matchmaker loop, so port allocation is race-free.
    fn spawn_proc(&self) -> Option<u16> {
        let port = {
            let ws = self.workers.lock().unwrap();
            let used: HashSet<u16> = ws.iter().map(|w| w.port).collect();
            let mut p = self.base_port + 1;
            while used.contains(&p) {
                p += 1;
            }
            p
        };
        let child = Command::new(&self.exe)
            .arg(&self.rom)
            .arg(&self.core)
            .arg("--worker")
            .arg("--port")
            .arg(port.to_string())
            .env("DATABASE_URL", &self.db_url)
            .env("INTERNAL_SECRET", &self.secret)
            .env("COORDINATOR_ORIGIN", &self.coord_origin)
            .env("RUST_LOG", "nes_web=warn,webrtc=off")
            .spawn()
            .ok()?;
        self.workers.lock().unwrap().push(Worker { port, child, public_id: String::new() });
        tracing::info!("coordinator: spawned worker on :{port} ({} live)", self.count());
        Some(port)
    }

    /// Poll the worker until it answers /internal/status (emulator booted + ROM loaded).
    async fn wait_ready(&self, port: u16) -> bool {
        let url = format!("http://127.0.0.1:{port}/internal/status?secret={}", self.secret);
        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Ok(r) = self.http.get(&url).send().await {
                if r.status().is_success() {
                    return true;
                }
            }
        }
        false
    }

    fn set_public(&self, port: u16, public_id: String) {
        if let Some(w) = self.workers.lock().unwrap().iter_mut().find(|w| w.port == port) {
            w.public_id = public_id;
        }
    }

    /// Kill a worker process and drop it from the pool (frees its emulator/CPU/RAM + its port).
    fn kill(&self, port: u16) {
        let mut ws = self.workers.lock().unwrap();
        if let Some(i) = ws.iter().position(|w| w.port == port) {
            let mut w = ws.remove(i);
            let _ = w.child.kill();
        }
    }

    fn kill_all(&self) {
        for w in self.workers.lock().unwrap().iter_mut() {
            let _ = w.child.kill();
        }
    }
}

/// Start the coordinator: no emulator, just matchmaking + the (lazily-spawned) worker pool + serve.
pub async fn run_coordinator(state: AppState, port: u16) -> anyhow::Result<()> {
    let pool = state.pool.clone().expect("coordinator requires a pool");

    tokio::spawn(reaper(pool.clone()));
    tokio::spawn(matchmaker(state.clone(), pool.clone()));

    // Best-effort: kill all workers when the coordinator is Ctrl-C'd.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            pool.kill_all();
            std::process::exit(0);
        });
    }

    let cap = if pool.max == usize::MAX { "∞".to_string() } else { pool.max.to_string() };
    let app = crate::signaling::router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  COORDINATOR on http://localhost:{port}  (max workers: {cap}, spawned on demand)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Pair queued players; spawn a fresh worker per match (up to the cap) and hand it the battle.
async fn matchmaker(state: AppState, pool: Arc<WorkerPool>) {
    let game = state.game.clone();
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        loop {
            if game.queue.lock().await.len() < 2 {
                break;
            }
            if pool.count() >= pool.max {
                break; // at capacity — players wait until a battle ends and frees a slot
            }
            let pair = {
                let mut q = game.queue.lock().await;
                if q.len() >= 2 {
                    Some((q.pop_front().unwrap(), q.pop_front().unwrap()))
                } else {
                    None
                }
            };
            let (a, b) = match pair {
                Some((a, b)) if a != b => (a, b),
                _ => continue,
            };
            // Spawn a dedicated worker for this match, boot it, then assign the battle.
            let port = match pool.spawn_proc() {
                Some(p) => p,
                None => {
                    requeue(&game, a, b).await;
                    break;
                }
            };
            if !pool.wait_ready(port).await {
                tracing::error!("coordinator: worker :{port} never became ready; killing");
                pool.kill(port);
                requeue(&game, a, b).await;
                break;
            }
            match assign_to_worker(&pool, port, a, b).await {
                Some(public_id) => {
                    pool.set_public(port, public_id.clone());
                    let ua = rooms::username_of(&game.db, a).await;
                    let ub = rooms::username_of(&game.db, b).await;
                    game.ws.send_to(a, json!({"type":"matched","room_id":public_id,"seat":1,"opponent":ub})).await;
                    game.ws.send_to(b, json!({"type":"matched","room_id":public_id,"seat":2,"opponent":ua})).await;
                    tracing::info!("coordinator: {a} vs {b} -> worker :{port} room {public_id}");
                }
                None => {
                    pool.kill(port);
                    requeue(&game, a, b).await;
                    break;
                }
            }
        }
    }
}

async fn requeue(game: &Arc<rooms::GameState>, a: UserId, b: UserId) {
    let mut q = game.queue.lock().await;
    q.push_front(b);
    q.push_front(a);
}

async fn assign_to_worker(pool: &WorkerPool, port: u16, a: UserId, b: UserId) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/internal/assign");
    let resp = pool
        .http
        .post(&url)
        .json(&json!({ "p1": a, "p2": b, "secret": pool.secret }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("public_id").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Reaper: once a worker's battle has ended (or the worker died), KILL the process so its emulator
/// stops running and the slot frees for the next match.
async fn reaper(pool: Arc<WorkerPool>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        // Only consider workers that actually started a battle (public_id assigned).
        let running: Vec<u16> = pool
            .workers
            .lock()
            .unwrap()
            .iter()
            .filter(|w| !w.public_id.is_empty())
            .map(|w| w.port)
            .collect();
        for p in running {
            let url = format!("http://127.0.0.1:{p}/internal/status?secret={}", pool.secret);
            match pool.http.get(&url).send().await {
                Ok(r) => {
                    let busy = r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v.get("busy").and_then(|x| x.as_bool()))
                        .unwrap_or(true);
                    if !busy {
                        pool.kill(p);
                        tracing::info!("coordinator: battle ended -> killed worker :{p} ({} live)", pool.count());
                    }
                }
                Err(_) => {
                    // Unreachable (crashed) — reap it.
                    pool.kill(p);
                    tracing::warn!("coordinator: worker :{p} unreachable -> reaped ({} live)", pool.count());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Worker-side internal endpoints (secret-gated; mounted only in --worker mode)
// ---------------------------------------------------------------------------

fn internal_ok(secret: &str) -> bool {
    std::env::var("INTERNAL_SECRET").map(|s| !s.is_empty() && s == secret).unwrap_or(false)
}

#[derive(Deserialize)]
pub struct AssignReq {
    pub p1: UserId,
    pub p2: UserId,
    #[serde(default)]
    pub secret: String,
}

/// POST /internal/assign {p1, p2, secret} -> create + start the battle here; return its UUID.
pub async fn assign_handler(State(st): State<AppState>, Json(req): Json<AssignReq>) -> Response {
    if !internal_ok(&req.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match rooms::assign_battle(&st.game, req.p1, req.p2).await {
        Some(public_id) => Json(json!({ "public_id": public_id })).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "assign failed").into_response(),
    }
}

#[derive(Deserialize)]
pub struct SecretQ {
    #[serde(default)]
    pub secret: String,
}

/// GET /internal/status?secret=... -> {busy} (true while a battle is running on this worker).
pub async fn status_handler(State(st): State<AppState>, Query(q): Query<SecretQ>) -> Response {
    if !internal_ok(&q.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let busy = st.game.active_room.lock().await.is_some();
    Json(json!({ "busy": busy })).into_response()
}
