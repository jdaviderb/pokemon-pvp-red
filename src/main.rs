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

    // Session cookie key. Set COOKIE_SECRET (>=64 bytes) in prod for stable sessions across
    // restarts; otherwise a fresh key is generated each boot (dev).
    let cookie_key = match std::env::var("COOKIE_SECRET") {
        Ok(s) if s.len() >= 64 => Key::from(s.as_bytes()),
        _ => Key::generate(),
    };

    // Game/room layer: matchmaking queue, rooms, WS hub. The matchmaker pairs queued players and
    // feeds the single emulator one match at a time.
    let game = std::sync::Arc::new(rooms::GameState::new(inner.clone(), database.clone()));
    rooms::spawn_matchmaker(game.clone());

    let state = AppState { api, inner, db: database, cookie_key, game };
    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  open http://localhost:3000  (click Connect)");
    axum::serve(listener, app).await?;
    Ok(())
}
