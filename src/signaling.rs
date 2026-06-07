//! axum router: serve static/index.html and handle the POST /offer SDP exchange.
//! Same-origin, so no CORS. Wire JSON is exactly {"type","sdp"} (browser-identical).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRef, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::Key;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;

use ::webrtc::api::API;

use crate::pipeline::AppInner;

#[derive(Clone)]
pub struct AppState {
    pub api: Arc<API>,
    pub inner: Arc<AppInner>,
    pub db: DatabaseConnection,
    pub cookie_key: Key,
    pub game: Arc<crate::rooms::GameState>,
}

impl FromRef<AppState> for Key {
    fn from_ref(s: &AppState) -> Self {
        s.cookie_key.clone()
    }
}

#[derive(Deserialize)]
pub struct OfferRequest {
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String, // "offer"
}

#[derive(Serialize)]
pub struct AnswerResponse {
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String, // "answer"
}

pub fn router(state: AppState) -> Router {
    // Serve ./static, index.html as the directory index for "/".
    let static_service = ServeDir::new("static").append_index_html_on_directories(true);
    Router::new()
        .route("/offer", post(offer_handler))
        .route("/battle/state", get(battle_state_handler))
        .route("/battle/action", post(battle_action_handler))
        .route("/battle/save", post(battle_save_handler))
        .route("/battle/load", post(battle_load_handler))
        .route("/battle/setup", post(battle_setup_handler))
        .route("/battle/species", get(battle_species_handler))
        .route("/battle/enemy", post(battle_enemy_handler))
        .route("/battle/player", post(battle_player_handler))
        // --- auth + session ---
        .route("/auth/register", post(crate::auth::register))
        .route("/auth/login", post(crate::auth::login))
        .route("/auth/logout", post(crate::auth::logout))
        .route("/api/me", get(crate::auth::me))
        .route("/api/species", get(species_list_handler))
        .route("/api/online", get(online_handler))
        // --- realtime ---
        .route("/ws", get(crate::ws::ws_upgrade))
        .fallback_service(static_service)
        .with_state(state)
}

/// GET /api/species -> [{dex, index, name, types, moves}] (dex = position+1; index = internal
/// Gen-1 byte). The slot machine keys sprites by national dex; the engine speaks internal index.
/// `types` is a display string (e.g. "ICE/FLYING"); `moves` are the 4 base move ids.
async fn species_list_handler() -> Json<Vec<serde_json::Value>> {
    Json(
        crate::battle::SPECIES
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let moves: Vec<u8> = s.moves.iter().map(|(id, _pp)| *id).collect();
                serde_json::json!({
                    "dex": i + 1,
                    "index": s.species,
                    "name": s.name,
                    "types": type_label(s.type1, s.type2),
                    "moves": moves,
                })
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct OnlineQuery {
    #[serde(default)]
    pub id: String,
}

/// GET /api/online?id=<client id> -> {online: N}. Records a heartbeat for `id` and counts clients
/// seen in the last 12s — anyone with a page open (title/lobby/room) is "online".
async fn online_handler(
    State(state): State<AppState>,
    Query(q): Query<OnlineQuery>,
) -> Json<serde_json::Value> {
    let now = std::time::Instant::now();
    let mut m = state.game.online.lock().unwrap();
    if !q.id.is_empty() {
        m.insert(q.id, now);
    }
    m.retain(|_, t| now.duration_since(*t) < std::time::Duration::from_secs(12));
    Json(serde_json::json!({ "online": m.len() }))
}

fn gen1_type_name(t: u8) -> &'static str {
    match t {
        0 => "NORMAL", 1 => "FIGHTING", 2 => "FLYING", 3 => "POISON", 4 => "GROUND", 5 => "ROCK",
        7 => "BUG", 8 => "GHOST", 0x14 => "FIRE", 0x15 => "WATER", 0x16 => "GRASS",
        0x17 => "ELECTRIC", 0x18 => "PSYCHIC", 0x19 => "ICE", 0x1A => "DRAGON", _ => "???",
    }
}
fn type_label(t1: u8, t2: u8) -> String {
    if t1 == t2 {
        gen1_type_name(t1).to_string()
    } else {
        format!("{}/{}", gen1_type_name(t1), gen1_type_name(t2))
    }
}

async fn offer_handler(
    State(state): State<AppState>,
    Json(offer): Json<OfferRequest>,
) -> Result<Json<AnswerResponse>, (StatusCode, String)> {
    let _ = &offer.kind; // expected "offer"
    let answer_sdp = crate::webrtc::build_peer_and_answer(&state.api, &state.inner, offer.sdp)
        .await
        .map_err(|e| {
            tracing::error!("offer handling failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    Ok(Json(AnswerResponse {
        sdp: answer_sdp,
        kind: "answer".to_owned(),
    }))
}

// ---------- AI battle-arena API ----------

/// GET /battle/state -> the latest snapshot the emulator thread published.
async fn battle_state_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::battle::BattleState>, (StatusCode, String)> {
    match state.inner.battle.lock().unwrap().clone() {
        Some(st) => Ok(Json(st)),
        None => Err((StatusCode::SERVICE_UNAVAILABLE, "no battle state yet".into())),
    }
}

/// POST /battle/action  body: {"type":"move","slot":0} (also switch/run/buttons). 202 = queued.
async fn battle_action_handler(
    State(state): State<AppState>,
    Json(action): Json<crate::battle::AgentAction>,
) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    let _ = state.inner.action_tx.send(action);
    StatusCode::ACCEPTED
}

/// True while a multiplayer match owns the single emulator — the dev console is locked out.
fn emu_busy(state: &AppState) -> bool {
    state.game.emu_busy.load(std::sync::atomic::Ordering::Relaxed)
}

/// POST /battle/save -> serialize on the emu thread, write states/battle.state, return the blob.
async fn battle_save_handler(
    State(state): State<AppState>,
) -> Result<Bytes, (StatusCode, String)> {
    if emu_busy(&state) {
        return Err((StatusCode::CONFLICT, "match in progress".into()));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.save_tx.send(tx);
    match rx.await {
        Ok(Some(buf)) => {
            let _ = std::fs::create_dir_all("states");
            if let Err(e) = std::fs::write("states/battle.state", &buf) {
                tracing::warn!("battle.state write failed: {e}");
            }
            Ok(Bytes::from(buf))
        }
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "serialize failed".into())),
    }
}

/// POST /battle/load  body: raw savestate bytes; if empty, load states/battle.state from disk.
async fn battle_load_handler(State(state): State<AppState>, body: Bytes) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    let data = if body.is_empty() {
        match std::fs::read("states/battle.state") {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("no battle.state on disk: {e}");
                return StatusCode::NOT_FOUND;
            }
        }
    } else {
        body.to_vec()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.load_tx.send((data, tx));
    match rx.await {
        Ok(true) => StatusCode::OK,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub player: u8, // internal species index
    pub enemy: u8,  // internal species index
    #[serde(default = "default_level")]
    pub level: u8,
    #[serde(default)]
    pub player_name: String, // custom nickname ("" = species name)
    #[serde(default)]
    pub enemy_name: String,
}
fn default_level() -> u8 {
    50
}

/// GET /battle/species -> the selectable species table for the dropdowns: [[index,"NAME"], ...].
async fn battle_species_handler() -> Json<Vec<(u8, &'static str)>> {
    Json(crate::battle::species_menu())
}

/// POST /battle/setup  body: {"player":74,"enemy":75,"level":50}
/// Loads the intro savestate, injects both party slots, drives the send-out. 200 = matchup live.
async fn battle_setup_handler(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if emu_busy(&state) {
        return Err((StatusCode::CONFLICT, "match in progress".into()));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.setup_tx.send(crate::pipeline::SetupReq {
        player: req.player,
        enemy: req.enemy,
        level: req.level,
        player_name: req.player_name,
        enemy_name: req.enemy_name,
        reply: tx,
    });
    match rx.await {
        Ok(Ok(())) => Ok(StatusCode::OK),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, e)),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR, "emu thread gone".into())),
    }
}

#[derive(Deserialize)]
pub struct EnemyRequest {
    /// Enemy move slot to force every turn (0..3), or 255 = let the game AI decide.
    #[serde(default = "ai_slot")]
    pub slot: u8,
}
fn ai_slot() -> u8 {
    0xFF
}

/// POST /battle/enemy {"slot":0..3}  -> YOU pick the opponent's move (forces wEnemySelectedMove
/// each turn). {"slot":255} or empty -> hand control back to the game AI.
async fn battle_enemy_handler(State(state): State<AppState>, Json(req): Json<EnemyRequest>) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    state
        .inner
        .enemy_force
        .store(req.slot, std::sync::atomic::Ordering::Relaxed);
    StatusCode::ACCEPTED
}

/// POST /battle/player {"slot":0..3} -> force the PLAYER's selected move into wPlayerSelectedMove
/// (CCDC) each turn, so the executed move is exactly that slot even if the menu macro mis-navigates.
/// {"slot":255} or empty -> trust the menu pick.
async fn battle_player_handler(State(state): State<AppState>, Json(req): Json<EnemyRequest>) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    state
        .inner
        .player_force
        .store(req.slot, std::sync::atomic::Ordering::Relaxed);
    StatusCode::ACCEPTED
}
