//! Server-side N64 emulation streamed to the browser over WebRTC.
//!
//! A libretro core (parallel_n64 / mupen64plus-next, angrylion software RDP) runs headless on
//! the server; the browser at http://localhost:3000 receives live VP8 video + stereo Opus audio
//! and sends keyboard input back over a data channel.
//!
//! Args: [1] ROM path (.z64), [2] core dylib path. Both optional.

mod audio;
mod auth;
mod battle;
mod db;
mod entities;
mod migrations;
mod n64;
mod pipeline;
mod rooms;
mod signaling;
mod species_data;
mod video;
mod webrtc; // our module; refer to the crate as `::webrtc`
mod ws;

use std::net::SocketAddr;

use axum_extra::extract::cookie::Key;
use signaling::{router, AppState};

// Default to "Pokemon Red.gb": gambatte's GBC auto-colorization (forced in n64.rs) renders it in
// color out of the box, AND its savestates power the battle arena + 2-player multiplayer. Pass the
// native romhack ".gbc" as argv[1] for its own colors, or any other ROM/core for GB/GBC/N64.
const DEFAULT_ROM: &str = "~/pokemon-pvp-red/Pokemon Red.gb";
const DEFAULT_CORE: &str =
    "~/pokemon-pvp-red/cores/gambatte_libretro.dylib";

/// A session-cookie key that is stable across restarts (so logins survive `cargo run`):
/// COOKIE_SECRET env if set, else a random key persisted to `.cookie_key` (created once).
fn load_cookie_key() -> Key {
    if let Ok(s) = std::env::var("COOKIE_SECRET") {
        if s.len() >= 64 {
            return Key::from(s.as_bytes());
        }
    }
    if let Ok(bytes) = std::fs::read(".cookie_key") {
        if bytes.len() >= 64 {
            return Key::from(bytes.as_slice());
        }
    }
    let key = Key::generate();
    if let Err(e) = std::fs::write(".cookie_key", key.master()) {
        tracing::warn!("could not persist .cookie_key ({e}); sessions won't survive a restart");
    }
    key
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nes_web=info,webrtc=warn".into()),
        )
        .init();

    let rom_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_ROM.to_string());
    let core_path = std::env::args().nth(2).unwrap_or_else(|| DEFAULT_CORE.to_string());
    tracing::info!("ROM:  {rom_path}");
    tracing::info!("core: {core_path}");

    // The N64 core is loaded on the emulator thread inside pipeline::start.
    let inner = pipeline::start(core_path, rom_path);

    // Build the shared WebRTC API once.
    let api = crate::webrtc::build_api()?;

    // DB: create-if-missing (sqlite) + run migrations; swap to Postgres via DATABASE_URL.
    let database = db::connect_and_migrate().await?;
    rooms::recover_abandoned(&database).await?;

    // Session cookie key — STABLE across restarts so sessions survive `cargo run`. COOKIE_SECRET
    // (>=64 bytes) wins; otherwise a key is generated once and persisted to .cookie_key.
    let cookie_key = load_cookie_key();

    // Game/room layer: matchmaking queue, rooms, WS hub. The matchmaker pairs queued players and
    // feeds the single emulator one match at a time.
    let game = std::sync::Arc::new(rooms::GameState::new(inner.clone(), database.clone()));
    rooms::spawn_matchmaker(game.clone());

    // DEV mode: set env `DEV=1` to mount the dev console + the unauthenticated /battle/* endpoints.
    // Off by default (production-safe).
    let dev = std::env::var("DEV").map(|v| v != "0" && !v.is_empty()).unwrap_or(false);
    let state = AppState { api, inner, db: database, cookie_key, game, dev };
    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  open http://localhost:3000  (click Connect)");
    axum::serve(listener, app).await?;
    Ok(())
}
