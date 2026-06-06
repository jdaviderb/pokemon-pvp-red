# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`nes-web` is a Rust server that runs a **NES emulator entirely server-side** and streams the
live game to a browser over **WebRTC** (VP8 video + Opus audio), with **keyboard input** sent
back over a WebRTC data channel. Open `http://localhost:3000`, click **Connect**, watch/play.
The emulator runs continuously regardless of viewers — emulation is server-only by design.

Default ROM: `~/projects-2026/nes-MK1/out/MK1.nes` (Mortal Kombat hack, mapper 4
/ MMC3). Override with a CLI arg: `cargo run --release -- /path/to/other.nes`.

## Build & run

```sh
./run.sh                       # build + run; opens nothing, just serves :3000
cargo run --release            # equivalent (env comes from .cargo/config.toml)
cargo build --release
```

Then open **http://localhost:3000** in **Chrome** (primary target) and click **Connect**.
Controls: arrows = D-pad, `Z` = B, `X` = A, `Enter` = Start, `Shift` = Select.

### Build prerequisites (already satisfied on this machine)

- **Toolchain is pinned to Rust 1.92** via `rust-toolchain.toml` — do NOT remove it. `webrtc`
  0.17.x (webrtc-util) uses `is_multiple_of` (stable since 1.87) and `home`/`time` need 1.88+.
  The machine's default `stable` is 1.86, which cannot build this; 1.92 is installed and pinned
  here only, leaving the global default untouched.
- **System libs**: homebrew `libvpx` and `libopus` under `/opt/homebrew`. `.cargo/config.toml`
  exports `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig` and `LIBRARY_PATH=/opt/homebrew/lib` so
  the `vpx-encode` and `opus` sys-crates link. If a fresh checkout fails to find them, that file
  is why.

## Architecture

```
emulator thread (60fps, std::thread)        WebRTC (per browser peer, tokio)
  clock_frame -> RGBA 256x240 + f32 48k        VP8 track <- video_tx.subscribe()
  RGBA->I420->VP8  --video_tx (broadcast)-->   Opus track <- audio_tx.subscribe()
  f32->i16->Opus   --audio_tx (broadcast)-->   write_sample(Sample{data,duration})
        ^ input_rx (mpsc)  <-- DataChannel "input" <-- browser keydown/keyup
axum :3000 serves static/index.html + POST /offer (non-trickle SDP exchange)
```

- **Master clock = the emulator thread** (`src/pipeline.rs::run_loop`), one dedicated OS thread
  drift-paced to NTSC ~60.0988 Hz. The `ControlDeck` and both encoders live only on that thread.
- Encoded media is fanned out via `tokio::sync::broadcast`; each peer's writer task subscribes
  and pushes `Sample`s into its track. `Sample.duration` (video 16.639 ms, audio 20 ms) drives
  the RTP timestamps; the browser does final A/V sync.
- One peer = one `RTCPeerConnection` built per `POST /offer`. Multiple peers are supported.

### File map

| File | Role |
|---|---|
| `src/main.rs` | entry: read ROM, start pipeline, build WebRTC API, serve axum |
| `src/emu.rs` | `tetanes-core` wrapper: ROM load (+ NES 2.0 header fix), frame/audio/input |
| `src/video.rs` | RGBA→I420 (BT.601 limited) + realtime VP8 encoder (`vpx-encode`) |
| `src/audio.rs` | f32→i16 ring + Opus encoder (exact 960-sample / 20 ms packets) |
| `src/pipeline.rs` | the 60fps loop; broadcast channels; `AppInner`; input apply; stats |
| `src/webrtc.rs` | per-peer PeerConnection, tracks, RTCP drain, data channel, signaling, cleanup |
| `src/signaling.rs` | axum `Router`, `POST /offer` handler, `AppState` |
| `static/index.html` | browser client (recvonly transceivers, data channel, key capture) |
| `test/*.cjs` | Puppeteer end-to-end checks (media, input, cleanup) — see `test/README.md` |
| `DESIGN.md` | full verified design + 10 risks/mitigations |
| `research/*.md` | grounded API research (each was compiled/run against the real ROM) |

## Non-obvious things — READ before editing these areas

- **NES 2.0 header bug** (`src/emu.rs::sanitize_nes2_ram_header`): `MK1.nes` has header byte 10 =
  `0x70`, and tetanes-core 0.14.1 does `64usize.checked_shl(0x70)` → overflow → `load_rom` fails
  with `InvalidHeader`. We zero header bytes 10 & 11 before load. Keep this unconditional; it's a
  no-op for clean headers. Do not "simplify" it away.
- **`vpx-encode` needs `features = ["ffi-generate"]`** because homebrew libvpx is 1.16 and the
  crate's pre-generated FFI only covers ≤1.13. Without it the build panics at compile time.
- **Module name `webrtc` shadows the `webrtc` crate.** Inside `src/webrtc.rs`, always refer to the
  crate as `::webrtc::...` (leading `::`); `crate::webrtc::` is our module.
- **Force-keyframe trick**: `vpx-encode` 0.6.2 can't force a keyframe, so when a peer reaches
  `Connected` we set `AppInner.keyframe_req`; the emulator thread then recreates the encoder
  (`make_vp8_encoder`) whose first frame is a keyframe. This is why a new viewer sees a clean
  picture immediately instead of garbage. Don't drop the reset without another keyframe mechanism.
- **Per-peer cleanup**: `write_sample` does NOT error on a dead track, so writer tasks gate on a
  per-peer `alive: AtomicBool` cleared when the connection hits Disconnected/Failed/Closed.
  Removing that flag reintroduces a per-connection task leak.
- **Audio is mono** (the APU mixes to one stream), buffered into exact 960-sample Opus frames.
  ~799 samples/frame is not a legal Opus size — never pad/truncate, always buffer.
- **Signaling order is load-bearing**: `create_answer → gathering_complete_promise →
  set_local_description → gather_complete.recv() → local_description` (non-trickle, single HTTP
  round trip). ICE servers are empty (localhost host candidates only).

## Testing

End-to-end is verified with headless Chrome (Puppeteer), not just compilation. With the server
running, see `test/README.md`. Quick signal that the pipeline is alive: the server logs
`emu: ~60.0 fps | N video pkts | M audio pkts | viewers v=.. a=..` every 5 s.

## Conventions

- Edition 2021, `rust-version = "1.92"`. Keep code in the surrounding style (terse, commented
  where non-obvious). `tracing` for logs (`RUST_LOG=nes_web=info,webrtc=warn`).
- When bumping `webrtc`, re-check the API against the pinned version's source (the master branch
  is restructured toward a different release and is misleading).
