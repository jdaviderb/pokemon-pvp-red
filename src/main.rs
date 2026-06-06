//! Server-side N64 emulation streamed to the browser over WebRTC.
//!
//! A libretro core (parallel_n64 / mupen64plus-next, angrylion software RDP) runs headless on
//! the server; the browser at http://localhost:3000 receives live VP8 video + stereo Opus audio
//! and sends keyboard input back over a data channel.
//!
//! Args: [1] ROM path (.z64), [2] core dylib path. Both optional.

mod audio;
mod battle;
mod n64;
mod pipeline;
mod species_data;
mod signaling;
mod video;
mod webrtc; // our module; refer to the crate as `::webrtc`

use std::net::SocketAddr;

use signaling::{router, AppState};

// Default to the GBC color romhack so `cargo run` shows color out of the box. The original
// grayscale DMG ROM is "Pokemon Red.gb" (pass it as argv[1] to play the classic monochrome look).
const DEFAULT_ROM: &str = "~/pokemon-pvp-red/Pokemon Red Color.gbc";
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

    let state = AppState { api, inner };
    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  open http://localhost:3000  (click Connect)");
    axum::serve(listener, app).await?;
    Ok(())
}
