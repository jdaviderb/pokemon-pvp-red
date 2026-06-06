# NES-over-WebRTC — Buildable Design

Server-side NES emulation that streams **live VP8 video + Opus audio** to a browser over
**WebRTC**, with **keyboard input** sent back over a **data channel**. A page at
`http://localhost:3000` shows the running game. The ROM is
`~/projects-2026/nes-MK1/out/MK1.nes` (mapper 4 / MMC3, NES 2.0 header).

This document synthesizes four verified research blueprints (`research/emulator.md`,
`research/webrtc.md`, `research/codec.md`, `research/architecture.md`) into ONE internally
consistent, copy-pasteable design. Every crate version, API name, and tricky snippet below
was taken from research that **actually compiled/ran on this machine** (macOS arm64,
rustc 1.86.0, homebrew libvpx 1.16.0, libopus 1.6.1).

Environment facts re-verified while writing this design:
- `MK1.nes` exists, 393232 bytes, header `4e 45 53 1a 08 20 42 08 00 00 70 00 ...` — **byte 10 = `0x70`** (the NES 2.0 RAM-size overflow trigger; see Risk R1).
- `pkg-config --modversion vpx` → `1.16.0`; `opus` → `1.6.1`.
- `rustc 1.86.0`.

---

## 1. Cargo.toml (all versions pinned; conflicts resolved)

```toml
[package]
name = "nes-web"
version = "0.1.0"
edition = "2021"

[dependencies]
# --- NES emulation (pure Rust, no C deps) ---
tetanes-core = "0.14.1"

# --- async runtime (shared by axum + webrtc-rs) ---
tokio = { version = "1", features = ["full"] }

# --- HTTP / signaling server ---
axum       = "0.8"
tower-http = { version = "0.6", features = ["fs"] }

# --- WebRTC (server side) ---
webrtc = "0.17.1"

# --- video encoding (VP8) -> binds SYSTEM libvpx 1.16.0 via pkg-config ---
# `ffi-generate` is MANDATORY here: env-libvpx-sys ships pre-generated bindings only
# up to libvpx 1.12/1.13, but this machine has 1.16.0. This feature runs bindgen
# against the installed 1.16.0 headers at build time. See Risk R3.
vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }

# --- audio encoding (Opus) -> binds SYSTEM libopus 1.6.1 via pkg-config ---
opus = "0.3.1"

# --- shared types / serialization / errors ---
bytes      = "1"                                   # webrtc::media::Sample.data is bytes::Bytes; MUST stay major 1.x
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow     = "1"

# --- logging ---
tracing            = "0.1"
tracing-subscriber = "0.3"
```

### Version-conflict resolutions (where the research fragments disagreed)

| Dep | research/webrtc | research/codec | research/architecture | **Final choice** | Why |
|---|---|---|---|---|---|
| `tokio` | `1.52` | `1` | `1` | **`"1"` + `["full"]`** | semver-loose pin resolves to latest 1.x ≥ 1.52; `full` gives `time`, `sync`, `net`, `rt-multi-thread`. |
| `axum` | `0.8` | — | `0.8.9` | **`"0.8"`** | both are 0.8.x; loose pin picks 0.8.9. |
| `tower-http` | `["fs"]` | — | `["fs","cors"]` | **`["fs"]`** | page and `/offer` are same-origin → no CORS needed. |
| `webrtc` | `0.17.1` | `0.17.1` | `0.17.1` | **`0.17.1`** | unanimous; the `0.20.0-alpha` is rejected (alpha, restructured examples). |
| `bytes` | `1` | `1` | `1` | **`"1"`** | MUST share webrtc's `bytes = "1"` major so `Bytes` unifies. |
| `vpx-encode` | — | `0.6.2 + ffi-generate` | `0.6.2 + ffi-generate` | **`0.6.2` + `ffi-generate`** | verified compile/run; `ffi-generate` is mandatory for libvpx 1.16.0. |
| `opus` | — | `0.3.1` | `0.3.1` | **`0.3.1`** | verified: links homebrew libopus 1.6.1 statically. |
| VP8 timebase | — | `[1,1000]` (ms pts) | `[1,60]` | **`[1,1000]`** | codec.md verified ms-pts; cleaner and decoupled from RTP (duration drives RTP, not pts). |
| RGBA→I420 | — | integer BT.601 *limited* | float BT.601 *full* | **integer BT.601 limited** (codec.md) | limited range is the WebRTC-correct default; integer kernel is faster, no float rounding. |

> **Edition note:** our crate is `edition = "2021"`. `tetanes-core 0.14.1` is itself `edition = "2024"` / `rust-version = "1.85"`; rustc 1.86 consumes that dependency fine. We do **not** need our crate on edition 2024.

> **Build cost note:** webrtc 0.17.1 default features compile vendored OpenSSL; first build is slow (minutes), then cached. Do not `cargo build` speculatively in tight loops.

---

## 2. File tree

```
nes-web/
├── Cargo.toml
├── DESIGN.md                  (this file)
├── .cargo/
│   └── config.toml            # sets PKG_CONFIG_PATH / LIBRARY_PATH so plain `cargo run` works
├── src/
│   ├── main.rs                # entry: tracing, build Arc<API>, spawn emulator, axum server
│   ├── emu.rs                 # tetanes-core wrapper: ROM load (+header sanitizer), frame/audio/input
│   ├── video.rs               # RGBA->I420 + VP8 encoder (vpx-encode)
│   ├── audio.rs               # f32->i16 ring buffer + Opus encoder (960-sample frames)
│   ├── pipeline.rs            # the 60fps emulator task: clock -> encode -> broadcast; input apply
│   ├── webrtc.rs              # build API, per-peer PeerConnection: tracks, RTCP drain, writer tasks, data channel
│   └── signaling.rs           # axum router + POST /offer handler + AppState
└── static/
    └── index.html             # browser client: recvonly transceivers, data channel, key capture
```

`src/main.rs` declares the modules:

```rust
mod emu;
mod video;
mod audio;
mod pipeline;
mod webrtc;        // our module; refer to the crate as `::webrtc`
mod signaling;
```

> Naming collision warning: our module `webrtc.rs` shadows the `webrtc` crate inside `crate::`. Inside `src/webrtc.rs` (and anywhere both are in scope) refer to the crate with a leading `::webrtc::...`. (Alternative: rename our module `rtc.rs`. The design uses `::webrtc` to keep the requested file name.)

---

## 3. The real-time architecture

```
 BROWSER (offerer)                          SERVER (answerer, axum :3000)
 ┌──────────────────┐   POST /offer JSON    ┌───────────────────────────────────────────┐
 │ RTCPeerConnection│ ───{type,sdp}───────► │ signaling::offer_handler                  │
 │  recvonly v+a    │ ◄──{type,sdp}──────── │   webrtc::build_peer_and_answer(state, off)│
 │  DataChannel     │      answer JSON      │     - new RTCPeerConnection               │
 │   "input"        │                       │     - add VP8 + Opus TrackLocalStaticSample│
 │  <video>         │ ◄═══ VP8 RTP ════════ │     - per-peer writer tasks subscribe()    │──┐
 │                  │ ◄═══ Opus RTP ═══════ │     - on_data_channel -> input_tx          │  │
 │  keydown/keyup ══╪═══ DataChannel ══════►│       (browser->server)                    │  │
 └──────────────────┘                       └───────────────────────────────────────────┘  │
                                                                                            │
   ┌────────────────────────────────────────────────────────────────────────────────────┐ │
   │  EMULATOR TASK  (one dedicated std::thread, Instant-paced 60.0988 Hz)                │ │
   │  loop {                                                                              │ │
   │    drain input_rx (try_recv) -> deck.joypad_mut(One).set_button(btn, down)           │◄┘ input_rx (mpsc)
   │    deck.clock_frame()                                                                │
   │    rgba = deck.frame_buffer()        (256x240x4 RGBA, 245_760 B)                     │
   │    pcm  = deck.audio_samples()       (mono f32 @48k, ~799/frame)                     │
   │    deck.clear_audio_samples()                                                        │
   │    rgba -> I420 -> VP8 -> video_tx.send(EncodedVideo)   (tokio broadcast)            │─┐
   │    pcm  -> i16 ring -> [960]Opus -> audio_tx.send(EncodedAudio) (50 pkt/s)           │─┤
   │    sleep until next 16.639ms deadline (drift-compensated)                            │ │
   │  }                                                                                   │ │
   └────────────────────────────────────────────────────────────────────────────────────┘ │
                          video_tx / audio_tx broadcast ──► per-peer writer tasks ─────────┘
```

**Master clock = the emulator.** One NES frame = one VP8 sample + ~799 audio samples. Both
are produced together so they stay aligned at the source. `Sample.duration` (16.639 ms video,
20 ms audio) drives the RTP timestamps; the browser's jitter buffer does final A/V lip-sync.

**Why broadcast channels:** the single authoritative emulator runs regardless of viewers; a new
peer just `subscribe()`s and starts receiving the in-progress stream. For v1, one peer is the
target, but this generalizes to 0..N.

**Pacing choice:** a dedicated `std::thread` with `Instant`-based drift compensation (research
recommended this over `tokio::time::interval` for steadier 60fps, because `clock_frame` + VP8
encode is synchronous CPU work we don't want fighting the async scheduler). The encoders
(`vpx-encode`, `opus`) and `ControlDeck` all live inside that one thread — none are shared
across threads.

**Audio packetization:** ~799 samples/frame is **not** a legal Opus frame. We buffer f32→i16 in
a ring and emit exactly **960-sample (20 ms @ 48 kHz) mono** packets, ~50/s. Never pad/truncate.

---

## 4. Source files

### 4.1 `src/emu.rs` — tetanes-core wrapper

**Purpose:** load `MK1.nes` (working around the NES 2.0 header overflow bug), set 48 kHz APU,
expose `clock_frame` → `(rgba, pcm)`, and apply button input. Verified API from
`tetanes-core 0.14.1` source.

```rust
use tetanes_core::control_deck::ControlDeck;
use tetanes_core::input::{JoypadBtn, Player};

/// Zero the NES 2.0 PRG-RAM/CHR-RAM size header bytes (10 & 11) to dodge the
/// tetanes-core 0.14.1 `64 << n` overflow on ROMs that declare battery NVRAM.
/// MK1.nes has byte10 = 0x70 -> `64 << 112` overflows -> load aborts without this.
/// No-op for clean headers, so keep it unconditional. (Verified: ROM then loads as
/// mapper 4 / Txrom / NTSC / battery_backed.)
fn sanitize_nes2_ram_header(bytes: &mut [u8]) {
    if bytes.len() >= 16 && &bytes[0..4] == b"NES\x1a" && (bytes[7] & 0x0C) == 0x08 {
        bytes[10] = 0x00; // PRG-RAM / PRG-NVRAM size (the 0x70 that overflows)
        bytes[11] = 0x00; // CHR-RAM / CHR-NVRAM size (defensive)
    }
}

pub struct Emu {
    deck: ControlDeck,
}

impl Emu {
    pub fn new(rom_bytes: &[u8], name: &str) -> anyhow::Result<Self> {
        let mut deck = ControlDeck::new();
        deck.set_sample_rate(48_000.0);     // match Opus; default is 44_100
        deck.set_concurrent_dpad(true);     // allow opposite D-pad directions (fighting game)

        let mut bytes = rom_bytes.to_vec();
        sanitize_nes2_ram_header(&mut bytes);
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        // load_rom auto-resets (running = true). clock_frame errs if not running.
        deck.load_rom(name, &mut cur)
            .map_err(|e| anyhow::anyhow!("load_rom failed: {e:?}"))?;
        debug_assert!(deck.is_running());
        Ok(Self { deck })
    }

    /// Advance exactly one NTSC frame. Returns Err if the CPU corrupts / ROM unloaded.
    pub fn clock_frame(&mut self) -> anyhow::Result<()> {
        self.deck.clock_frame().map_err(|e| anyhow::anyhow!("clock_frame: {e:?}"))
    }

    /// RGBA8888, 256x240, 245_760 bytes, alpha always 255.
    pub fn frame_buffer(&mut self) -> &[u8] { self.deck.frame_buffer() }

    /// Mono f32 @ 48 kHz accumulated since last clear (~799 samples/frame).
    pub fn audio_samples(&self) -> &[f32] { self.deck.audio_samples() }

    /// MUST be called every frame or samples accumulate across frames.
    pub fn clear_audio_samples(&mut self) { self.deck.clear_audio_samples(); }

    pub fn set_button(&mut self, btn: JoypadBtn, pressed: bool) {
        self.deck.joypad_mut(Player::One).set_button(btn, pressed);
    }
}

/// Browser wire button name -> tetanes JoypadBtn. Unknown -> None (ignored).
pub fn map_button(b: &str) -> Option<JoypadBtn> {
    Some(match b {
        "A" => JoypadBtn::A,
        "B" => JoypadBtn::B,
        "Up" => JoypadBtn::Up,
        "Down" => JoypadBtn::Down,
        "Left" => JoypadBtn::Left,
        "Right" => JoypadBtn::Right,
        "Start" => JoypadBtn::Start,
        "Select" => JoypadBtn::Select,
        _ => return None,
    })
}
```

Key verified facts: `frame_buffer()` len = `245_760` (RGBA, alpha=255); `audio_samples()` mono
f32; `set_button` is sticky across frames (keydown=true / keyup=false); `JoypadBtn` is in
`tetanes_core::input` (NOT the prelude).

---

### 4.2 `src/video.rs` — RGBA→I420 + VP8 encoder

**Purpose:** convert tetanes RGBA to planar I420 and run the realtime VP8 encoder. Verified:
256×240 synthetic frame → 321-byte keyframe + ~45-byte interframes, linked to libvpx 1.16.0.

```rust
use vpx_encode::{Config, Encoder, VideoCodecId};

pub const W: usize = 256;
pub const H: usize = 240;
/// I420 packed size for 256x240 = 61440 (Y) + 15360 (U) + 15360 (V) = 92160 bytes.
pub const I420_LEN: usize = W * H + 2 * ((W / 2) * (H / 2));

/// Realtime VP8 encoder for the NES output (256x240). timebase [1,1000] => pts is milliseconds.
/// bitrate is in KILOBITS/sec (kbps), not bps. encode() always uses VPX_DL_REALTIME internally.
pub fn make_vp8_encoder() -> vpx_encode::Result<Encoder> {
    Encoder::new(Config {
        width: W as u32,
        height: H as u32,
        timebase: [1, 1000],  // 1/1000 s == ms; pts in ms
        bitrate: 2000,        // kbps (2 Mbps). 1000..=3000 is sane for 256x240.
        codec: VideoCodecId::VP8,
    })
}

/// RGBA8888 (256x240) -> packed I420, BT.601 *limited* range (the WebRTC-correct default).
/// `dst.len()` must be >= I420_LEN. Reuse `dst` across frames (allocate once).
pub fn rgba_to_i420(rgba: &[u8], width: usize, height: usize, dst: &mut [u8]) {
    debug_assert_eq!(rgba.len(), width * height * 4);
    let y_size = width * height;
    let c_w = width / 2;
    let c_h = height / 2;
    debug_assert!(dst.len() >= y_size + 2 * c_w * c_h);

    let (y_plane, uv) = dst.split_at_mut(y_size);
    let (u_plane, v_plane) = uv.split_at_mut(c_w * c_h);

    for j in 0..height {
        for i in 0..width {
            let p = (j * width + i) * 4;
            let r = rgba[p] as i32;
            let g = rgba[p + 1] as i32;
            let b = rgba[p + 2] as i32;
            // BT.601 limited: Y in [16,235], U/V centered at 128. <<8 fixed point.
            let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
            y_plane[j * width + i] = (y + 16) as u8;
            if (j & 1) == 0 && (i & 1) == 0 {
                let u = (-38 * r - 74 * g + 112 * b + 128) >> 8;
                let v = (112 * r - 94 * g - 18 * b + 128) >> 8;
                let ci = (j / 2) * c_w + (i / 2);
                u_plane[ci] = (u + 128) as u8;
                v_plane[ci] = (v + 128) as u8;
            }
        }
    }
}
```

> **Encoder borrow gotcha:** `enc.encode(pts, i420)` returns a `Packets` iterator yielding
> `Frame { data: &[u8], key: bool, pts: i64 }`. `frame.data` borrows the encoder's internal
> buffer — **copy it with `Bytes::copy_from_slice` before the next `encode()`**. Always drain
> the iterator (usually 1 frame, occasionally 0 or >1). The encode+copy happens in
> `pipeline.rs` (§4.4) so this module stays sync-only and Send-free.

> vpx-encode 0.6.2 cannot force keyframes or set `cpu_used`/`kf_max_dist` (the `ctx` is
> private). libvpx auto-emits keyframes, so a late joiner recovers within ~1–2 s. See Risk R4
> for the direct-FFI fallback if that's too slow.

---

### 4.3 `src/audio.rs` — f32→i16 ring buffer + Opus encoder

**Purpose:** turn the per-frame ~799 mono f32 samples into legal 960-sample (20 ms) Opus
packets. Verified: `opus 0.3.1` links homebrew libopus 1.6.1, encodes a 20 ms frame to a few
hundred bytes.

```rust
use opus::{Application, Bitrate, Channels, Encoder as OpusEncoder};

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
pub const OPUS_FRAME: usize = 960; // 20 ms @ 48 kHz, MONO. The only size we emit.

/// One encoded Opus packet plus the PCM sample count it represents (for duration math).
pub struct OpusPacket {
    pub data: Vec<u8>,
    pub samples: u32, // always OPUS_FRAME (960) here
}

pub struct OpusStreamer {
    enc: OpusEncoder,
    pcm: Vec<i16>,    // mono i16 ring (drain-from-front)
    out: Vec<u8>,     // reusable encode output
}

impl OpusStreamer {
    pub fn new() -> opus::Result<Self> {
        // NES audio is MONO. Application::Audio = best quality for game music/SFX.
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, Channels::Mono, Application::Audio)?;
        enc.set_bitrate(Bitrate::Bits(96_000))?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        Ok(Self { enc, pcm: Vec::with_capacity(4096), out: vec![0u8; 4000] })
    }

    /// Feed this frame's mono f32 samples (48 kHz). Clamps + scales to i16.
    pub fn push_f32(&mut self, samples: &[f32]) {
        self.pcm.extend(samples.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
    }

    /// Drain every full 960-sample frame, encoding each. Returns the packets to broadcast.
    pub fn take_packets(&mut self) -> opus::Result<Vec<OpusPacket>> {
        let mut packets = Vec::new();
        while self.pcm.len() >= OPUS_FRAME {
            // contiguous frame; for mono, per-channel count == input.len() == 960.
            let frame: Vec<i16> = self.pcm.drain(..OPUS_FRAME).collect();
            let n = self.enc.encode(&frame, &mut self.out)?;
            packets.push(OpusPacket { data: self.out[..n].to_vec(), samples: OPUS_FRAME as u32 });
        }
        Ok(packets)
    }
}
```

> Why mono: the tetanes APU is a single mixed stream. Mono Opus negotiates fine with Chrome
> and Firefox and uses less bitrate. (If you ever want stereo, duplicate L=R and switch to
> `Channels::Stereo` with input length `1920`.)

---

### 4.4 `src/pipeline.rs` — the 60fps emulator task

**Purpose:** the master clock. Build `Emu` + both encoders inside one dedicated thread, run the
drift-compensated loop, broadcast encoded video/audio, and apply input. Defines the shared
message types and channels.

```rust
use std::time::{Duration, Instant};
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use crate::audio::OpusStreamer;
use crate::emu::{map_button, Emu};
use crate::video::{make_vp8_encoder, rgba_to_i420, I420_LEN, H, W};

/// NTSC NES frame period: 1e9 / 60.098814 Hz ≈ 16_639_267 ns.
pub const NTSC_FRAME_NANOS: u64 = 16_639_267;

#[derive(Clone)]
pub struct EncodedVideo { pub data: Bytes }

#[derive(Clone)]
pub struct EncodedAudio { pub data: Bytes, pub samples: u32 }

/// Input event from the browser data channel: {"type":"down"|"up","button":"A"}.
#[derive(serde::Deserialize)]
pub struct InputEvent {
    #[serde(rename = "type")]
    pub kind: String,    // "down" | "up"
    pub button: String,  // "A" "B" "Up" "Down" "Left" "Right" "Start" "Select"
}

/// Shared state behind AppState (see signaling.rs).
pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>, // capacity 8
    pub audio_tx: broadcast::Sender<EncodedAudio>, // capacity 32
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
}

/// Build channels + spawn the emulator thread. Returns the AppInner to share.
pub fn start(rom_bytes: Vec<u8>) -> std::sync::Arc<AppInner> {
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(8);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(32);
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();

    let v = video_tx.clone();
    let a = audio_tx.clone();
    // Dedicated OS thread (NOT tokio): steady 60fps, encoders never cross threads.
    std::thread::spawn(move || {
        if let Err(e) = run_loop(rom_bytes, v, a, input_rx) {
            tracing::error!("emulator loop ended: {e:?}");
        }
    });

    std::sync::Arc::new(AppInner { video_tx, audio_tx, input_tx })
}

fn run_loop(
    rom_bytes: Vec<u8>,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
) -> anyhow::Result<()> {
    let mut emu = Emu::new(&rom_bytes, "MK1.nes")?;
    let mut vpx = make_vp8_encoder().map_err(|e| anyhow::anyhow!("vpx init: {e:?}"))?;
    let mut opus = OpusStreamer::new().map_err(|e| anyhow::anyhow!("opus init: {e:?}"))?;

    let mut i420 = vec![0u8; I420_LEN];        // reused scratch
    let frame_period = Duration::from_nanos(NTSC_FRAME_NANOS);
    let start = Instant::now();
    let mut next = start;
    let mut frame_idx: u64 = 0;

    loop {
        // 1. Apply all pending input to player one (sticky until released).
        while let Ok(ev) = input_rx.try_recv() {
            if let Some(btn) = map_button(&ev.button) {
                emu.set_button(btn, ev.kind == "down");
            }
        }

        // 2. Advance one frame.
        emu.clock_frame()?;

        // 3. VIDEO: RGBA -> I420 -> VP8 -> broadcast (copy out of the encoder's buffer).
        rgba_to_i420(emu.frame_buffer(), W, H, &mut i420);
        let pts_ms = (frame_idx * 1000 / 60) as i64; // ms pts for timebase [1,1000]
        match vpx.encode(pts_ms, &i420) {
            Ok(packets) => {
                for frame in packets {
                    let _ = video_tx.send(EncodedVideo {
                        data: Bytes::copy_from_slice(frame.data),
                    });
                }
            }
            Err(e) => tracing::warn!("vpx encode: {e:?}"),
        }

        // 4. AUDIO: f32 -> i16 ring -> 960-sample Opus packets -> broadcast.
        opus.push_f32(emu.audio_samples());
        emu.clear_audio_samples(); // REQUIRED every frame
        match opus.take_packets() {
            Ok(pkts) => for p in pkts {
                let _ = audio_tx.send(EncodedAudio { data: Bytes::from(p.data), samples: p.samples });
            },
            Err(e) => tracing::warn!("opus encode: {e:?}"),
        }

        // 5. Drift-compensated pacing to the next 16.639 ms deadline.
        frame_idx += 1;
        next += frame_period;
        let now = Instant::now();
        if next > now { std::thread::sleep(next - now); } else { next = now; }
    }
}
```

> `broadcast::send` returns `Err` only when there are no subscribers — we ignore it (`let _ =`),
> so the emulator keeps running with zero viewers. A slow subscriber that lags past the channel
> capacity gets `RecvError::Lagged` on its side (handled in the writer task, §4.5) and resyncs at
> the next keyframe.

---

### 4.5 `src/webrtc.rs` — peer connection, tracks, writer tasks, data channel

**Purpose:** build the shared `Arc<API>` once; per offer, build a `RTCPeerConnection`, attach
VP8+Opus tracks, drain RTCP, spawn writer tasks that pull from the broadcast and `write_sample`,
wire the input data channel, run offer/answer signaling, and keep the peer alive.

(Inside this file, the crate is `::webrtc` because the module itself is named `webrtc`.)

```rust
use std::sync::Arc;
use std::time::Duration;

use ::webrtc::api::interceptor_registry::register_default_interceptors;
use ::webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use ::webrtc::api::{APIBuilder, API};
use ::webrtc::data_channel::data_channel_message::DataChannelMessage;
use ::webrtc::data_channel::RTCDataChannel;
use ::webrtc::ice_transport::ice_server::RTCIceServer;
use ::webrtc::interceptor::registry::Registry;
use ::webrtc::media::Sample;
use ::webrtc::peer_connection::configuration::RTCConfiguration;
use ::webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use ::webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use ::webrtc::peer_connection::RTCPeerConnection;
use ::webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use ::webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use ::webrtc::track::track_local::TrackLocal;

use crate::pipeline::{AppInner, NTSC_FRAME_NANOS};

/// Build the shared API once (MediaEngine + default codecs (VP8/Opus) + default interceptors).
pub fn build_api() -> anyhow::Result<Arc<API>> {
    let mut m = MediaEngine::default();
    m.register_default_codecs()?;                  // registers VP8 (video/VP8) + Opus (audio/opus)
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?; // NACK, reports, TWCC
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();
    Ok(Arc::new(api))
}

/// Build a peer for one browser offer; return the gathered answer SDP. Keeps `pc` alive
/// inside the spawned writer tasks (their Arc clones outlive this function).
pub async fn build_peer_and_answer(
    api: &Arc<API>,
    inner: &Arc<AppInner>,
    offer_sdp: String,
) -> anyhow::Result<String> {
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let pc = Arc::new(api.new_peer_connection(config).await?);

    // --- VIDEO track (VP8). Same stream id "nes" groups A+V into one MediaStream. ---
    let video_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability { mime_type: MIME_TYPE_VP8.to_owned(), ..Default::default() },
        "video".to_owned(),
        "nes".to_owned(),
    ));
    let video_sender = pc
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    // MANDATORY: drain RTCP or NACK/report interceptors stall.
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while let Ok((_, _)) = video_sender.read(&mut buf).await {}
    });

    // --- AUDIO track (Opus) ---
    let audio_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability { mime_type: MIME_TYPE_OPUS.to_owned(), ..Default::default() },
        "audio".to_owned(),
        "nes".to_owned(),
    ));
    let audio_sender = pc
        .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while let Ok((_, _)) = audio_sender.read(&mut buf).await {}
    });

    // --- WRITER TASKS: subscribe to the broadcast, write_sample into this peer's tracks.
    //     The Arc<TrackLocalStaticSample> clones moved here keep the tracks (and thus the
    //     session media flow) alive after this function returns. ---
    {
        let mut vrx = inner.video_tx.subscribe();
        let vtrack = Arc::clone(&video_track);
        let video_dur = Duration::from_nanos(NTSC_FRAME_NANOS); // ~16.639 ms drives 90kHz RTP step
        tokio::spawn(async move {
            loop {
                match vrx.recv().await {
                    Ok(f) => {
                        let _ = vtrack.write_sample(&Sample {
                            data: f.data,
                            duration: video_dur,
                            ..Default::default()
                        }).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break, // channel closed
                }
            }
        });

        let mut arx = inner.audio_tx.subscribe();
        let atrack = Arc::clone(&audio_track);
        tokio::spawn(async move {
            loop {
                match arx.recv().await {
                    Ok(p) => {
                        // honest duration: samples * 1000 / 48000 ms (960 -> 20 ms)
                        let dur = Duration::from_millis((p.samples as u64 * 1000) / 48_000);
                        let _ = atrack.write_sample(&Sample {
                            data: p.data,
                            duration: dur,
                            ..Default::default()
                        }).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    // --- INPUT data channel: browser creates "input"; we react via on_data_channel. ---
    {
        let input_tx = inner.input_tx.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let input_tx = input_tx.clone();
            Box::pin(async move {
                if dc.label() == "input" {
                    let input_tx = input_tx.clone();
                    dc.on_message(Box::new(move |msg: DataChannelMessage| {
                        let input_tx = input_tx.clone();
                        Box::pin(async move {
                            if let Ok(ev) = serde_json::from_slice::<crate::pipeline::InputEvent>(&msg.data) {
                                let _ = input_tx.send(ev);
                            }
                        })
                    }));
                }
            })
        }));
    }

    // --- Keep the connection alive past this function & log teardown. ---
    let pc_hold = Arc::clone(&pc);
    pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        tracing::info!("peer state: {s}");
        let _ = &pc_hold; // hold an Arc inside the long-lived handler closure
        Box::pin(async {})
    }));

    // --- Signaling: offer -> answer -> gather ICE fully (no trickle) -> return answer SDP. ---
    let offer = RTCSessionDescription::offer(offer_sdp)?;
    pc.set_remote_description(offer).await?;
    let answer = pc.create_answer(None).await?;
    let mut gather_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(answer).await?; // starts UDP listeners + ICE gathering
    let _ = gather_complete.recv().await;    // BLOCK until all candidates gathered

    let local = pc
        .local_description()
        .await
        .ok_or_else(|| anyhow::anyhow!("no local description"))?;
    Ok(local.sdp)
}
```

Verified API facts (webrtc 0.17.1 source): `add_track` needs the explicit
`as Arc<dyn TrackLocal + Send + Sync>` cast; RTCP must be drained per sender; ordering is
`create_answer → gathering_complete_promise → set_local_description → gather_complete.recv()`;
`Sample` is constructed with `..Default::default()` setting only `data` + `duration`;
`RTCSessionDescription::offer(String)` rebuilds an offer from a raw SDP string.

> **Keep-alive:** the writer-task `Arc<TrackLocalStaticSample>` clones plus the
> `on_peer_connection_state_change` closure each hold the connection's resources alive after
> `build_peer_and_answer` returns. For a hardened multi-peer build, also stash
> `Arc<RTCPeerConnection>` in a session map on `AppInner` and remove it on `Failed/Closed`.

---

### 4.6 `src/signaling.rs` — axum router + `/offer`

**Purpose:** serve `static/index.html` and handle `POST /offer`. Same-origin, so no CORS. The
wire JSON is exactly `{"type","sdp"}`.

```rust
use std::sync::Arc;
use axum::{extract::State, routing::post, Json, Router};
use axum::http::StatusCode;
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
    let answer_sdp = crate::webrtc::build_peer_and_answer(&state.api, &state.inner, offer.sdp)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(AnswerResponse { sdp: answer_sdp, kind: "answer".to_owned() }))
}
```

---

### 4.7 `src/main.rs` — entry point

**Purpose:** init tracing, read the ROM, start the emulator pipeline, build the WebRTC API,
serve axum on `127.0.0.1:3000`.

```rust
mod emu;
mod video;
mod audio;
mod pipeline;
mod webrtc;
mod signaling;

use std::net::SocketAddr;
use signaling::{router, AppState};

const ROM_PATH: &str = "~/projects-2026/nes-MK1/out/MK1.nes";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // 1. Read the ROM (header sanitizer runs inside Emu::new).
    let rom_bytes = std::fs::read(ROM_PATH)
        .map_err(|e| anyhow::anyhow!("read {ROM_PATH}: {e}"))?;

    // 2. Start the emulator thread + broadcast channels.
    let inner = pipeline::start(rom_bytes);

    // 3. Build the shared WebRTC API once.
    let api = crate::webrtc::build_api()?;

    // 4. Serve.
    let state = AppState { api, inner };
    let app = router(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
```

---

### 4.8 `static/index.html` — browser client

**Purpose:** offerer that receives media and sends keyboard input. Click-to-connect satisfies
autoplay-with-audio policy. `recvonly` transceivers give the server m-lines to answer into.

```html
<!doctype html>
<html>
<head><meta charset="utf-8"><title>NES over WebRTC</title></head>
<body>
  <h1>NES over WebRTC — MK1</h1>
  <button id="connect">Connect</button>
  <p id="status">idle</p>
  <video id="video" autoplay playsinline controls
         style="width:512px;height:480px;image-rendering:pixelated;background:#000"></video>

  <script>
  const KEYMAP = {
    "ArrowUp": "Up", "ArrowDown": "Down", "ArrowLeft": "Left", "ArrowRight": "Right",
    "KeyZ": "B", "KeyX": "A", "Enter": "Start", "ShiftRight": "Select", "ShiftLeft": "Select",
  };

  let pc, inputChannel;
  const statusEl = document.getElementById("status");
  const video = document.getElementById("video");
  document.getElementById("connect").onclick = connect;

  async function connect() {
    statusEl.textContent = "connecting...";
    pc = new RTCPeerConnection({ iceServers: [{ urls: "stun:stun.l.google.com:19302" }] });

    // We only receive media. recvonly transceivers create m-lines the server answers into.
    pc.addTransceiver("video", { direction: "recvonly" });
    pc.addTransceiver("audio", { direction: "recvonly" });

    const remote = new MediaStream();
    video.srcObject = remote;
    pc.ontrack = (e) => { remote.addTrack(e.track); };

    pc.oniceconnectionstatechange = () => { statusEl.textContent = "ice: " + pc.iceConnectionState; };

    // Browser creates the input channel; server reacts via on_data_channel.
    inputChannel = pc.createDataChannel("input", { ordered: true });
    inputChannel.onopen = () => { statusEl.textContent = "connected"; };

    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await waitIceGatheringComplete(pc);   // non-trickle: POST a complete offer

    const resp = await fetch("/offer", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ sdp: pc.localDescription.sdp, type: pc.localDescription.type }),
    });
    const answer = await resp.json();     // { sdp, type:"answer" }
    await pc.setRemoteDescription(answer);
  }

  function waitIceGatheringComplete(pc) {
    if (pc.iceGatheringState === "complete") return Promise.resolve();
    return new Promise((resolve) => {
      const check = () => {
        if (pc.iceGatheringState === "complete") {
          pc.removeEventListener("icegatheringstatechange", check);
          resolve();
        }
      };
      pc.addEventListener("icegatheringstatechange", check);
    });
  }

  // Capture keys; send {type:'down'|'up', button:'A'}; de-dupe key-repeat with a Set.
  const pressed = new Set();
  function sendInput(kind, button) {
    if (inputChannel && inputChannel.readyState === "open") {
      inputChannel.send(JSON.stringify({ type: kind, button }));
    }
  }
  window.addEventListener("keydown", (e) => {
    const btn = KEYMAP[e.code]; if (!btn) return;
    e.preventDefault(); if (pressed.has(e.code)) return;
    pressed.add(e.code); sendInput("down", btn);
  });
  window.addEventListener("keyup", (e) => {
    const btn = KEYMAP[e.code]; if (!btn) return;
    e.preventDefault(); pressed.delete(e.code); sendInput("up", btn);
  });
  </script>
</body>
</html>
```

### Keyboard ↔ NES mapping (authoritative)

| NES button | `e.code` | wire `button` | `JoypadBtn` |
|---|---|---|---|
| Up/Down/Left/Right | `ArrowUp/Down/Left/Right` | `Up`/`Down`/`Left`/`Right` | `JoypadBtn::Up/Down/Left/Right` |
| B | `KeyZ` | `B` | `JoypadBtn::B` |
| A | `KeyX` | `A` | `JoypadBtn::A` |
| Start | `Enter` | `Start` | `JoypadBtn::Start` |
| Select | `ShiftLeft`/`ShiftRight` | `Select` | `JoypadBtn::Select` |

---

## 5. `.cargo/config.toml` (so plain `cargo run` finds the homebrew libs)

```toml
[env]
PKG_CONFIG_PATH = "/opt/homebrew/lib/pkgconfig"
LIBRARY_PATH    = "/opt/homebrew/lib"
```

These let `vpx-encode`/`opus` discover libvpx 1.16.0 / libopus 1.6.1 via pkg-config without
exporting env vars by hand. (`ffi-generate`'s bindgen finds libclang automatically on macOS.)

---

## 6. Build & run

```sh
# From the project root: ~/pokemon-pvp-red

# Build (env is also set by .cargo/config.toml, but exporting is belt-and-suspenders):
PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig LIBRARY_PATH=/opt/homebrew/lib cargo build --release

# Run:
PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig LIBRARY_PATH=/opt/homebrew/lib cargo run --release
```

Then open **http://localhost:3000** in Chrome (or Firefox), click **Connect**, and play
(`Z`=B, `X`=A, `Enter`=Start, arrows=D-pad, `Shift`=Select).

> First build is slow because webrtc compiles vendored OpenSSL and `ffi-generate` runs bindgen.
> Subsequent builds are cached.

---

## 7. Risks & fallbacks

**R1 — NES 2.0 header overflow blocks MK1.nes (HIGH likelihood, fully mitigated).**
`MK1.nes` byte 10 = `0x70`; tetanes-core 0.14.1 computes `64 << 112` → `checked_shl` overflow →
`load_rom` returns `InvalidHeader { byte: 11, ... "header ram size larger than 64" }`.
*Fallback:* the `sanitize_nes2_ram_header` in `emu.rs` zeros header bytes 10 & 11 before load
(verified working; ROM loads as mapper 4 / Txrom / NTSC / battery_backed). It's a no-op for
clean headers, so it stays unconditional. If a future tetanes release fixes the masking, the
sanitizer remains harmless.

**R2 — `tetanes-core` API drift (LOW; API read from v0.14.1 source, pin exact).**
The blueprint pins `=0.14.1`-compatible `"0.14.1"`. If a method differs (e.g. `set_concurrent_dpad`,
`clock_frame_output`), the explicit `clock_frame` + `frame_buffer` + `audio_samples` +
`clear_audio_samples` path is the lowest-common-denominator and was the verified one.
*Fallback:* if the crate moved on, pin literally `tetanes-core = "=0.14.1"`; if `ControlDeck`
itself changed, `plastic_core` is the documented secondary core (clock + `pixel_buffer()` +
`audio_buffer()`).

**R3 — libvpx 1.16.0 FFI build failure (HIGH without the feature, mitigated).**
Default `env-libvpx-sys` ships bindings only to ~1.12/1.13 and panics:
`Expected file "generated/vpx-ffi-1.16.0.rs" not found but 'generate' cargo feature not used.`
*Fallback:* `features = ["ffi-generate"]` (already in Cargo.toml) runs bindgen against the
installed headers — verified. If bindgen can't find libclang, `export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib`.
Last-resort (discouraged): `export VPX_VERSION=1.13.0` to force the bundled 1.13 binding (ABI risk).

**R4 — Late joiner sees gray/garbled video until next keyframe (MEDIUM).**
vpx-encode 0.6.2 cannot force a keyframe (private `ctx`); libvpx auto-emits periodic keyframes,
so recovery is ~1–2 s. For one local viewer who clicks Connect at startup, often a non-issue.
*Fallback:* drop to a ~120-line direct `env-libvpx-sys` encoder (copy vpx-encode's `lib.rs`),
set `c.kf_max_dist = 120`, add `vpx_codec_control_(&mut ctx, VP8E_SET_CPUUSED, 6)`, and expose
`force_keyframe()` (pass `VPX_EFLAG_FORCE_KF` to `vpx_codec_encode`) called when a new peer's ICE
reaches Connected.

**R5 — VP8/audio timing or A/V drift (MEDIUM).**
RTP timestamps come from `Sample.duration`, not your loop precision. Wrong durations = drift.
*Mitigation:* video `duration = NTSC_FRAME_NANOS` (16.639 ms), audio `duration = samples*1000/48000`
(960 → 20 ms). Emulator is the single clock; ~799 audio samples/frame buffered into exact 960-sample
Opus packets (~50/s) absorb the 60-vs-50 mismatch. *Fallback if drift appears:* verify the APU
sample rate is actually 48 kHz (`set_sample_rate(48_000.0)`), and never pad/truncate audio frames.

**R6 — Safari vs Chrome codec/autoplay issues (MEDIUM).**
Chrome/Firefox negotiate VP8+Opus from `register_default_codecs()` cleanly. Safari's WebRTC VP8
support is historically weaker and autoplay-with-audio is strict.
*Fallback:* primary target is **Chrome on localhost**. The `<video>` is `muted`-free but gated
behind the Connect click (user gesture) to satisfy autoplay. If Safari refuses VP8, the codec
swap is H.264 via `openh264` + `MIME_TYPE_H264` (register explicitly with payload type 96/102);
keep VP8 for v1.

**R7 — Peer connection drops right after `/offer` returns (MEDIUM, mitigated).**
If every `Arc<RTCPeerConnection>` drops at the end of the handler, the session tears down.
*Mitigation:* the two writer tasks hold `Arc<TrackLocalStaticSample>` clones and the
`on_peer_connection_state_change` closure holds an `Arc<pc>`, keeping it alive. *Fallback for
multi-peer robustness:* add a `Mutex<HashMap<peer_id, Arc<RTCPeerConnection>>>` to `AppInner`,
insert on connect, remove on `Failed/Disconnected/Closed`.

**R8 — `webrtc` module-name shadows the `webrtc` crate (LOW, mitigated).**
Inside `src/webrtc.rs` use `::webrtc::...` (leading `::`) for the crate. *Fallback:* rename the
module `rtc.rs` and update `main.rs` + the `crate::webrtc::` call in `signaling.rs`.

**R9 — RTCP not drained → interceptors stall (LOW, mitigated).**
Each `RTCRtpSender` has a `sender.read(&mut buf)` loop spawned. Removing it breaks NACK/reports.
Keep both drain loops.

**R10 — ICE gathering latency on first connect (LOW).**
Non-trickle signaling blocks on `gathering_complete_promise().recv()`, adding ~0.5–2 s to the
first `/offer` while STUN gathers. Acceptable for localhost (host candidates on 127.0.0.1 are
enough — `ice_servers` could even be empty). *Fallback for production:* trickle ICE over a
WebSocket.
