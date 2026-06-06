//! axum router: serve static/index.html and handle the POST /offer SDP exchange.
//! Same-origin, so no CORS. Wire JSON is exactly {"type","sdp"} (browser-identical).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
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
