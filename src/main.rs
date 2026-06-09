//! Pokémon Red PVP — server-side Game Boy emulation streamed to the browser over WebRTC.
//!
//! A libretro core (gambatte, Game Boy / GBC) runs headless on the server; the browser at
//! http://localhost:3000 receives live VP8 video + stereo Opus audio and sends keyboard input back
//! over a data channel. (The libretro frontend is generic — it can load any core — but the game is
//! Pokémon Red PVP.)
//!
//! Args: [1] ROM path (.gb), [2] core path. Both optional.

mod audio;
mod auth;
mod battle;
mod coordinator;
mod db;
mod entities;
mod fight;
mod flags;
mod hud;
mod mcp;
mod migrations;
mod libretro;
mod oauth;
mod pipeline;
mod ranking;
mod rooms;
mod signaling;
mod species_data;
mod video;
mod webrtc; // our module; refer to the crate as `::webrtc`
mod ws;

use axum_extra::extract::cookie::Key;
use signaling::{router, AppState, Role};

// Default to "Pokemon Red.gb": gambatte's GBC auto-colorization (forced in libretro.rs) renders it
// in color out of the box, AND its savestates power the battle arena + 2-player multiplayer. Pass
// the native romhack ".gbc" as argv[1] for its own colors, or any other ROM/core for GB/GBC.
const DEFAULT_ROM: &str = "Pokemon Red.gb";
const DEFAULT_CORE: &str = "cores/gambatte_libretro.dylib";

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
    // Load THIS project's ./.env only (CLIENT_ID/SECRET/REDIRECT, DEV, COOKIE_SECRET, ...). Use
    // from_path (no parent-dir walk) on purpose: dotenvy::dotenv() climbs to the first ancestor
    // .env, which on this machine is ~/.env full of unrelated prod secrets. Real env vars win.
    let _ = dotenvy::from_path(".env");

    // `pokemon-red-pvp --mcp`: run the stdio MCP server (an AI agent plays via ARENA_TOKEN/ARENA_URL). Handled
    // FIRST and with STDERR-only logging — stdout is the MCP JSON-RPC channel and must stay clean.
    if std::env::args().any(|a| a == "--mcp") {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "pokemon_red_pvp=warn".into()),
            )
            .init();
        return mcp::run().await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pokemon_red_pvp=info,webrtc=warn".into()),
        )
        .init();

    // Args: positional [rom] [core]; flags --port N, --worker, --coordinator, --workers N.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut port: u16 = 3000;
    let mut workers: Option<usize> = None; // explicit cap; else MAX_WORKERS env; else unbounded
    let (mut worker, mut coordinator, mut solo) = (false, false, false);
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--port" => { port = raw.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(3000); i += 2; }
            "--workers" => { workers = raw.get(i + 1).and_then(|s| s.parse().ok()); i += 2; }
            "--worker" => { worker = true; i += 1; }
            "--coordinator" => { coordinator = true; i += 1; }
            "--solo" => { solo = true; i += 1; }
            other => { positional.push(other.to_string()); i += 1; }
        }
    }
    // Default to the SCALABLE coordinator (spawns a worker per battle, so N rooms run concurrently).
    // `--solo` forces the single-process arena (one emulator, one battle); `--worker` is internal.
    coordinator = coordinator || (!worker && !solo);
    let rom_path = positional.first().cloned().unwrap_or_else(|| DEFAULT_ROM.to_string());
    let core_path = positional.get(1).cloned().unwrap_or_else(|| DEFAULT_CORE.to_string());

    // Shared by every mode. Workers DON'T migrate/seed/recover — the coordinator (or solo) owns the
    // shared schema; N workers re-running migrations on boot just contends the single writer.
    let api = crate::webrtc::build_api()?;
    let database = db::connect(!worker).await?;
    if !worker {
        flags::seed_defaults(&database).await?;
        rooms::recover_abandoned(&database).await?;
    }
    // Leaderboard cache: load the last file snapshot now (instant board), and on the coordinator/solo
    // process run the background refresh job every RANKING_REFRESH_SECS (default 300 = 5 min).
    let ranking = ranking::new_cache();
    ranking::load_file(&ranking);
    if !worker {
        let secs = std::env::var("RANKING_REFRESH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
        ranking::spawn_job(database.clone(), ranking.clone(), std::time::Duration::from_secs(secs));
    }
    let cookie_key = load_cookie_key();
    let dev = std::env::var("DEV").map(|v| v != "0" && !v.is_empty()).unwrap_or(false);
    let oauth = std::sync::Arc::new(oauth::registry_from_env());
    let cookie_secure =
        std::env::var("COOKIE_SECURE").map(|v| v != "0" && !v.is_empty()).unwrap_or(!dev);

    // --- Coordinator: no emulator; spawns ephemeral worker processes on demand (scalable mode). ---
    if coordinator {
        // Max concurrent workers: explicit --workers, else MAX_WORKERS env, else UNBOUNDED.
        let max = workers
            .or_else(|| std::env::var("MAX_WORKERS").ok().and_then(|s| s.parse().ok()))
            .unwrap_or(usize::MAX);
        let db_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://./data.db?mode=rwc".to_string());
        let coord_origin = format!("http://localhost:{port}");
        let inner = pipeline::dummy();
        let game = std::sync::Arc::new(rooms::GameState::new(inner.clone(), database.clone()));
        flags::seed_cache(&game).await; // populate the flag cache before serving /api/config
        flags::spawn_cache_refresher(game.clone());
        let pool = std::sync::Arc::new(coordinator::WorkerPool::new(
            port, max, rom_path, core_path, db_url, coord_origin,
        ));
        let state = AppState {
            api, inner, db: database, cookie_key, game,
            role: Role::Coordinator, pool: Some(pool), dev, oauth, cookie_secure, ranking,
        };
        return coordinator::run_coordinator(state, port).await;
    }

    // --- Worker / Solo: real emulator on this process. ---
    tracing::info!("ROM:  {rom_path}");
    tracing::info!("core: {core_path}");
    let inner = pipeline::start(core_path, rom_path);
    let game = std::sync::Arc::new(rooms::GameState::new(inner.clone(), database.clone()));
    let role = if worker { Role::Worker } else { Role::Solo };
    if role == Role::Solo {
        // Solo runs its own matchmaker; a worker's battles are assigned by the coordinator instead.
        rooms::spawn_matchmaker(game.clone());
        rooms::spawn_presence_sweeper(game.clone());
        flags::seed_cache(&game).await;
        flags::spawn_cache_refresher(game.clone());
    }
    let state = AppState {
        api, inner, db: database, cookie_key, game,
        role, pool: None, dev, oauth, cookie_secure, ranking,
    };
    let app = router(state);
    let addr = signaling::bind_addr(port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  {role:?} on http://localhost:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}
