//! Server-side NES emulation streamed to the browser over WebRTC.
//!
//! The emulator runs entirely on the server; the browser at http://localhost:3000
//! receives live VP8 video + Opus audio and sends keyboard input back over a data
//! channel. Pass a ROM path as the first CLI arg to override the default.

mod audio;
mod emu;
mod pipeline;
mod signaling;
mod video;
mod webrtc; // our module; refer to the crate as `::webrtc`

use std::net::SocketAddr;

use signaling::{router, AppState};

const DEFAULT_ROM: &str = "~/projects-2026/nes-MK1/out/MK1.nes";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nes_web=info,webrtc=warn".into()),
        )
        .init();

    // 1. Resolve + read the ROM (header sanitizer runs inside Emu::new).
    let rom_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_ROM.to_string());
    let rom_name = std::path::Path::new(&rom_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "room.nes".to_string());
    let rom_bytes =
        std::fs::read(&rom_path).map_err(|e| anyhow::anyhow!("read {rom_path}: {e}"))?;
    tracing::info!("loaded room '{rom_name}' ({} bytes) from {rom_path}", rom_bytes.len());

    // 2. Start the emulator thread + broadcast channels.
    let inner = pipeline::start(rom_bytes, rom_name);

    // 3. Build the shared WebRTC API once.
    let api = crate::webrtc::build_api()?;

    // 4. Serve.
    let state = AppState { api, inner };
    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  open http://localhost:3000  (click Connect)");
    axum::serve(listener, app).await?;
    Ok(())
}
