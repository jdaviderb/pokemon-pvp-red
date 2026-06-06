//! axum router: serve static/index.html and handle the POST /offer SDP exchange.
//! Same-origin, so no CORS. Wire JSON is exactly {"type","sdp"} (browser-identical).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;

use ::webrtc::api::API;

use crate::pipeline::AppInner;

#[derive(Clone)]
pub struct AppState {
    pub api: Arc<API>,
    pub inner: Arc<AppInner>,
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
        .fallback_service(static_service)
        .with_state(state)
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
    let _ = state.inner.action_tx.send(action);
    StatusCode::ACCEPTED
}

/// POST /battle/save -> serialize on the emu thread, write states/battle.state, return the blob.
async fn battle_save_handler(
    State(state): State<AppState>,
) -> Result<Bytes, (StatusCode, String)> {
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
    state
        .inner
        .enemy_force
        .store(req.slot, std::sync::atomic::Ordering::Relaxed);
    StatusCode::ACCEPTED
}
