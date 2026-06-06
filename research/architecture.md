# NES-over-WebRTC: Overall Architecture, HTTP Server, and Prior Art

> Blueprint for a Rust server that runs an NES emulator and streams live VP8 video +
> Opus audio to a browser over WebRTC, with controller input sent back over a data channel.
>
> All versions and APIs below were verified against real source on GitHub / docs.rs / crates.io
> in June 2026. Pinned versions exist on crates.io. Code snippets use the **exact** current API.

---

## 0. TL;DR — the recommended stack (pin these exact versions)

```toml
[package]
name = "nes-web"
version = "0.1.0"
edition = "2021"

[dependencies]
# --- async runtime ---
tokio = { version = "1", features = ["full"] }

# --- HTTP / signaling server ---
axum = "0.8.9"
tower-http = { version = "0.6.11", features = ["fs", "cors"] }

# --- WebRTC (server side) ---
webrtc = "0.17.1"

# --- NES emulation ---
tetanes-core = "0.14.1"

# --- video encoding (VP8) -> binds SYSTEM libvpx 1.16.0 via pkg-config ---
# IMPORTANT: the `ffi-generate` feature is MANDATORY on this machine (see §6 gotchas).
vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }

# --- audio encoding (Opus) -> binds SYSTEM libopus 1.6.1 via pkg-config ---
opus = "0.3.1"

# --- pixel format conversion RGBA -> I420 (see §6) ---
# either hand-roll (recommended, ~30 lines) or use a crate:
# yuv = "0.x"  # optional; hand-rolled converter avoids a dep

# --- misc ---
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bytes = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

Build env (only needed if pkg-config can't find the homebrew libs):

```sh
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
export LIBRARY_PATH=/opt/homebrew/lib
# vpx-encode's ffi-generate uses bindgen -> needs libclang (present at
# /Library/Developer/CommandLineTools/usr/lib/libclang.dylib, auto-found by bindgen on macOS)
```

Confirmed on this machine:
- `pkg-config --modversion vpx` -> **1.16.0** (`/opt/homebrew/lib/libvpx.12.dylib` -> Cellar/libvpx/1.16.0)
- `pkg-config --modversion opus` -> **1.6.1**
- `/usr/bin/clang` + `/Library/Developer/CommandLineTools/usr/lib/libclang.dylib` present (bindgen OK)

---

## 1. Prior art (what exists, what to steal, what to avoid)

Searched GitHub/web for "rust webrtc game/emulator streaming", "cloud gaming rust", "server side
emulator webrtc". Findings, most relevant first:

### 1a. `rlamarche/bevy_streaming` — Bevy cloud gaming over WebRTC
- **Stack:** Uses **GStreamer's `webrtcsink`** (NOT webrtc-rs) for the WebRTC transport, `bevy_capture`
  to grab Bevy's camera frames, and external signaling servers (GstWebRTC / Unreal PixelStreaming /
  LiveKit / WHIP). Video via GStreamer VP8/VP9/H264/H265 plugins or NVENC.
- **Reusable pattern:** *Input events are sent from the browser to the server over a WebRTC data
  channel* — exactly the controller-input model we want. Capture-then-encode-then-webrtc pipeline.
- **Why we don't copy it directly:** GStreamer is a heavy native dependency and a different
  WebRTC implementation. For a from-scratch Rust app, `webrtc-rs` + `vpx-encode` is far lighter and
  has no GStreamer install/runtime burden. But the *architecture* (game loop -> encode -> track,
  input over data channel) is the blueprint.
- **Gotcha noted there:** Unreal PixelStreaming 5.5 has a default feature that breaks the WebRTC
  connection on some Chrome versions. Not relevant to webrtc-rs, but a reminder that codec/SDP
  negotiation quirks are real.

### 1b. `JRF63/desktop-streaming` — Steam-Link-style desktop streamer in Rust
- **Stack:** Captures the desktop via Windows `IDXGIOutputDuplication`, encodes with **NVEnc**, and
  pushes the encoded stream **through webrtc-rs**. This is the closest "encode frames yourself, hand
  them to webrtc-rs as a track" precedent.
- **Reusable pattern:** Exactly our model — *you own the encoder, you `write_sample()` the encoded
  frames into a `TrackLocalStaticSample`*. (Windows/NVEnc specifics don't port to macOS, but the
  webrtc-rs track-feeding pattern does.)

### 1c. `webrtc-rs/webrtc` official examples — the canonical reference
- The `examples/examples/play-from-disk-vpx` example is the **single most important reference**: it
  shows VP8 video + Opus audio tracks fed via `write_sample`, paced with `tokio::time::interval`,
  with offer/answer SDP signaling. Our server is essentially "play-from-disk-vpx, but the frames
  come from a live encoder instead of an IVF file, and signaling is HTTP POST instead of stdin".
- `examples/examples/data-channels` shows `on_data_channel` / `on_message` — our controller input path.

### 1d. `zheland/rust-webrtc-client-server-example`
- **Stack:** async Rust, `tokio-tungstenite` WebSocket signaling + `webrtc-rs`. Confirms the
  webrtc-rs server pattern. We use HTTP POST signaling instead of WebSocket (simpler for one-shot
  offer/answer, no trickle ICE).

### Key takeaways for our design
1. **Own the encoder, feed `TrackLocalStaticSample::write_sample`.** This is the proven webrtc-rs
   pattern (1b, 1c). No need for `TrackLocalStaticRTP` or manual RTP packetization — `write_sample`
   handles VP8/Opus packetization internally.
2. **Input over a data channel** is the established cloud-gaming approach (1a).
3. **HTTP one-shot offer/answer** (no trickle ICE) is the simplest signaling; the official examples
   gather ICE fully before returning the answer. We replace stdin paste with an axum POST handler.
4. **Avoid GStreamer** — `webrtc-rs` + `vpx-encode` + `opus` is a pure-crates path that compiles
   against the homebrew libs already installed.

---

## 2. HTTP layer: axum 0.8.9 (recommended; coexists with webrtc-rs on tokio)

**Recommendation: axum.** Rationale: webrtc-rs is built on tokio; axum is the tokio-native HTTP
framework. They share one runtime with zero glue. axum 0.8.9 is the current stable (released
2026-04-14). `tower-http`'s `ServeDir`/`ServeFile` serve the static client. No better fit exists for
"serve a page + one POST endpoint, on the same tokio runtime as webrtc-rs".

### 2a. Routes we need
- `GET /`           -> serves `static/index.html`
- `GET /*` (assets) -> serves `static/` (the client JS if you split it out; inline is fine for v1)
- `POST /offer`     -> receives the browser's SDP offer (JSON), returns the server's SDP answer (JSON)

### 2b. Exact axum 0.8 setup (compiles against axum 0.8.9)

```rust
use std::sync::Arc;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use tower_http::services::ServeDir;
use serde::{Deserialize, Serialize};

// Shared app state: holds whatever the /offer handler needs to spin up a peer
// (e.g. a handle to the running emulator's broadcast channels). See §3.
#[derive(Clone)]
struct AppState {
    inner: Arc<AppInner>,
}

struct AppInner {
    // e.g. broadcast::Sender<EncodedFrame> for video, etc. Filled in §3.
}

// The SDP wire types. webrtc-rs's RTCSessionDescription already implements
// Serialize/Deserialize, so you can also just use that type directly in Json<...>.
// Using a thin local struct keeps the HTTP layer decoupled; convert in the handler.
#[derive(Deserialize)]
struct OfferRequest {
    sdp: String,
    #[serde(rename = "type")]
    kind: String, // "offer"
}

#[derive(Serialize)]
struct AnswerResponse {
    sdp: String,
    #[serde(rename = "type")]
    kind: String, // "answer"
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let state = AppState { inner: Arc::new(AppInner { /* ... */ }) };

    // Serve ./static, with index.html as the directory index for "/".
    let static_service = ServeDir::new("static")
        .append_index_html_on_directories(true);

    let app = Router::new()
        .route("/offer", post(offer_handler))
        // fallback_service serves any path not matched above from ./static
        .fallback_service(static_service)
        .with_state(state);

    // Bind. Use 127.0.0.1:3000 for local-only, or 0.0.0.0:3000 to accept LAN clients.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    tracing::info!("listening on http://127.0.0.1:3000");
    axum::serve(listener, app).await?;
    Ok(())
}

// POST /offer  { "sdp": "...", "type": "offer" }  ->  { "sdp": "...", "type": "answer" }
async fn offer_handler(
    State(state): State<AppState>,
    Json(offer): Json<OfferRequest>,
) -> Result<Json<AnswerResponse>, axum::http::StatusCode> {
    // 1. Build RTCPeerConnection (see §4), set remote description from `offer`,
    //    add the VP8 + Opus tracks that the emulator task feeds, create answer,
    //    wait for ICE gathering complete, return the answer SDP.
    // 2. On error, map to 500.
    let answer = build_peer_and_answer(&state, offer)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(answer))
}

// stub — real impl in §4
async fn build_peer_and_answer(
    _state: &AppState,
    _offer: OfferRequest,
) -> anyhow::Result<AnswerResponse> { unimplemented!() }
```

Notes on axum 0.8 API specifics that bite people:
- `axum::serve(listener, app)` is the 0.7/0.8 way (there is no `Server::bind` builder anymore).
- `State<T>` extractor + `.with_state(state)` is how you pass shared state; `T: Clone`.
- `.fallback_service(ServeDir::new("static"))` is the idiomatic "serve SPA/static + a few API routes"
  pattern. `ServeDir` is a `tower::Service`, so it goes through `*_service` methods, not `get(...)`.
- If you prefer an explicit index route instead of the fallback, use
  `.route_service("/", ServeFile::new("static/index.html"))` (needs `tower_http::services::ServeFile`).
- CORS: same-origin (page and POST both on :3000) means you do **not** need CORS. Only add
  `tower_http::cors::CorsLayer` if you serve the page from a different origin.

---

## 3. Async architecture (single emulator, one peer, real-time pacing)

### 3a. Tasks and data flow

```
                         ┌─────────────────────────────────────────────────────┐
                         │  EMULATOR TASK (1 tokio task, real-time 60.0988 Hz)  │
                         │  - tetanes_core::ControlDeck                          │
                         │  - tokio::time::interval at the NTSC frame period     │
   data channel (input)  │  loop {                                               │
   browser ──JSON──┐     │    apply pending input -> joypad_mut().set_button()   │
                   ▼     │    deck.clock_frame()                                 │
            input_tx ───►│    rgba = deck.frame_buffer()      (256x240x4 RGBA)   │
       (mpsc, lock-free) │    pcm  = deck.audio_samples()     (f32 @ 48000)      │
                         │    rgba -> I420 -> vp8 -> video_tx.send(EncodedFrame) │
                         │    f32  -> i16  -> opus -> audio_tx.send(EncodedPkt)  │
                         │    deck.clear_audio_samples()                         │
                         │    interval.tick().await                              │
                         │  }                                                    │
                         └───────────────┬───────────────────────┬──────────────┘
                                         │ video_tx              │ audio_tx
                                  (tokio broadcast)       (tokio broadcast)
                                         │                       │
                         ┌───────────────▼───────────────────────▼──────────────┐
                         │  PER-PEER WRITER TASKS (spawned when a peer connects) │
                         │  video: while let Ok(f)=rx.recv(){ track.write_sample}│
                         │  audio: while let Ok(p)=rx.recv(){ track.write_sample}│
                         └───────────────────────────────────────────────────────┘
```

Why broadcast channels: a newly-connected browser just `subscribe()`s to the broadcast senders and
immediately starts receiving the *in-progress* stream — no special "join" logic. The single shared
emulator keeps running regardless of who's connected (it's the authoritative game). For v1, one peer
at a time is fine; broadcast also trivially supports 0 or N viewers later.

### 3b. The channels and message types

```rust
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

/// One encoded VP8 frame ready for write_sample.
#[derive(Clone)]
struct EncodedVideo {
    data: Bytes,
    // duration we tell webrtc-rs for this sample (one NTSC frame).
}

/// One encoded Opus packet (covers `frame_dur` of audio, e.g. 20ms / 960 samples @48k).
#[derive(Clone)]
struct EncodedAudio {
    data: Bytes,
    samples: u32, // PCM samples this packet represents (for duration math)
}

/// Input event coming FROM the browser data channel.
#[derive(serde::Deserialize)]
struct InputEvent {
    #[serde(rename = "type")]
    kind: String,   // "down" | "up"
    button: String, // "A" | "B" | "Up" | "Down" | "Left" | "Right" | "Start" | "Select"
}

struct AppInner {
    video_tx: broadcast::Sender<EncodedVideo>, // capacity e.g. 8
    audio_tx: broadcast::Sender<EncodedAudio>, // capacity e.g. 16
    input_tx: mpsc::UnboundedSender<InputEvent>, // browser -> emulator
}
```

### 3c. The emulator task (real-time NTSC pacing)

NTSC NES frame rate is **60.098814 Hz** -> frame period **16.63943 ms**. Use a `tokio::time::interval`
(which compensates for drift, unlike repeated `sleep`). Set the APU sample rate to **48000** so the
audio cleanly matches Opus's native rate and `samples/frame ≈ 48000/60.0988 ≈ 798.7`.

```rust
use std::time::Duration;
use tetanes_core::{control_deck::ControlDeck, input::{JoypadBtn, Player}};

// NTSC frame period. 1_000_000_000 / 60.098814 ≈ 16_639_267 ns
const NTSC_FRAME_NANOS: u64 = 16_639_267;

fn spawn_emulator(
    rom_bytes: Vec<u8>,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
) {
    // The encoders (vpx + opus) are NOT Send-friendly to share; build them inside the task.
    // ControlDeck is also kept inside the task; all emulation is single-threaded here.
    tokio::task::spawn_blocking(move || {
        // ^ spawn_blocking is appropriate because encode() is CPU-bound and synchronous.
        //   We do our own pacing with std::thread timing OR run the loop on a dedicated
        //   thread. If you want tokio::time::interval pacing, use a normal `tokio::spawn`
        //   and call the synchronous encoders directly (they're fast enough for 60fps at
        //   256x240). Either works; see note below.

        let mut deck = ControlDeck::new();
        // load_rom takes (name: impl ToString, rom: &mut impl std::io::Read).
        // A &[u8] is Read, and &mut &[u8] satisfies &mut impl Read:
        let mut cursor: &[u8] = &rom_bytes;
        deck.load_rom("MK1.nes", &mut cursor).expect("valid ROM");
        deck.set_sample_rate(48_000.0); // match Opus

        // ... build vpx encoder (§5) and opus encoder (§5) here ...

        let frame_period = Duration::from_nanos(NTSC_FRAME_NANOS);
        let mut next = std::time::Instant::now();
        loop {
            // 1. Drain pending input and apply to player one's joypad.
            while let Ok(ev) = input_rx.try_recv() {
                if let Some(btn) = map_button(&ev.button) {
                    let pressed = ev.kind == "down";
                    deck.joypad_mut(Player::One).set_button(btn, pressed);
                }
            }

            // 2. Advance exactly one frame.
            if deck.clock_frame().is_err() { break; }

            // 3. Pull video + audio for this frame.
            let rgba: &[u8] = deck.frame_buffer();       // 256*240*4 = 245_760 bytes RGBA
            let pcm_f32: &[f32] = deck.audio_samples();  // ~799 mono samples @48k

            // 4. Encode (see §5) and broadcast. (encode calls elided here.)
            // let _ = video_tx.send(EncodedVideo { data: vp8_bytes });
            // let _ = audio_tx.send(EncodedAudio { data: opus_bytes, samples: n });

            deck.clear_audio_samples();

            // 5. Pace. (If using tokio::time::interval, replace this with interval.tick().)
            next += frame_period;
            let now = std::time::Instant::now();
            if next > now { std::thread::sleep(next - now); } else { next = now; }
        }
    });
}

fn map_button(b: &str) -> Option<JoypadBtn> {
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

> Pacing choice: a dedicated blocking thread with `Instant`-based drift compensation (shown above) is
> the most robust for a tight 60fps loop, because `clock_frame` + VP8 encode is CPU work that you do
> NOT want to interleave with the async scheduler's fairness. Alternatively use a normal `tokio::spawn`
> + `let mut interval = tokio::time::interval(frame_period); interval.tick().await;` — `interval`
> compensates for drift and is what the official play-from-disk-vpx example uses. Both are valid; the
> blocking-thread version gives steadier frame timing under load.

### 3d. Audio/video sync — keep it simple
- Drive **both** from the **same per-frame loop**: one video frame and one batch of ~799 audio
  samples per emulator frame. They are produced together, so they stay aligned at the source.
- Give each `write_sample` an honest `duration`:
  - Video sample duration = one NTSC frame = `Duration::from_nanos(NTSC_FRAME_NANOS)` (~16.64ms).
  - Audio sample duration = `samples * 1000 / 48000` ms (the play-from-disk-vpx example does exactly
    this granule math). webrtc-rs uses the duration to compute RTP timestamps; honest durations make
    the browser's jitter buffer line them up. NES audio+video share a clock, so as long as durations
    are honest, A/V drift is bounded.
- Don't over-engineer A/V sync for v1. The browser does the final lip-sync via RTP timestamps; your
  job is only to emit correct per-sample durations.
- Opus packetization detail: Opus encodes fixed frame sizes (2.5/5/10/20/40/60 ms). 799 samples is
  NOT a legal Opus frame. So **buffer** f32 audio across emulator frames and emit Opus packets of a
  legal size — **960 samples = 20ms @48k** is the standard. Accumulate samples in a `Vec<i16>`; every
  time you have ≥960, encode one 960-sample (mono) Opus packet and `audio_tx.send`. See §5b.

### 3e. New browser joins the in-progress stream
1. Browser POSTs its offer to `/offer`.
2. Handler builds an `RTCPeerConnection`, creates **fresh** `TrackLocalStaticSample`s for VP8 + Opus,
   `add_track`s them, sets remote description, creates+sets answer, waits for ICE gathering.
3. Handler spawns two writer tasks that `video_tx.subscribe()` / `audio_tx.subscribe()` and loop
   `write_sample` into the peer's tracks until the channel closes / peer disconnects.
4. Handler registers `on_data_channel` -> `on_message` -> parse JSON -> `input_tx.send(InputEvent)`.
5. Returns the answer JSON; browser sets it as remote description; media flows.

Because tracks are created per-peer but fed from the shared broadcast, the emulator never restarts
and a late joiner simply starts getting frames from "now". For v1 with one peer, you can even keep a
single pair of tracks and recreate them on each new offer.

---

## 4. WebRTC peer setup + signaling (webrtc 0.17.1 — exact API)

Verified against `webrtc-rs/webrtc` tag `v0.17.1`, example `play-from-disk-vpx` and `data-channels`.

```rust
use std::sync::Arc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use webrtc::api::APIBuilder;
use webrtc::interceptor::registry::Registry;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;

async fn build_peer_and_answer(
    state: &AppState,
    offer_req: OfferRequest,
) -> anyhow::Result<AnswerResponse> {
    // --- API object (MediaEngine + default interceptors) ---
    let mut m = MediaEngine::default();
    m.register_default_codecs()?;
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let pc = Arc::new(api.new_peer_connection(config).await?);

    // --- VIDEO track (VP8) ---
    let video_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability { mime_type: MIME_TYPE_VP8.to_owned(), ..Default::default() },
        "video".to_owned(),
        "nes".to_owned(),
    ));
    let video_sender = pc
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    // Must drain RTCP or interceptors (NACK etc.) misbehave:
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

    // --- Writer tasks: pull from broadcast, write_sample into this peer's tracks ---
    {
        use std::time::Duration;
        use webrtc::media::Sample;
        let mut vrx = state.inner.video_tx.subscribe();
        let vtrack = Arc::clone(&video_track);
        tokio::spawn(async move {
            while let Ok(frame) = vrx.recv().await {
                let _ = vtrack.write_sample(&Sample {
                    data: frame.data,
                    duration: Duration::from_nanos(NTSC_FRAME_NANOS),
                    ..Default::default()
                }).await;
            }
        });
        let mut arx = state.inner.audio_tx.subscribe();
        let atrack = Arc::clone(&audio_track);
        tokio::spawn(async move {
            while let Ok(pkt) = arx.recv().await {
                let dur = Duration::from_millis((pkt.samples as u64 * 1000) / 48_000);
                let _ = atrack.write_sample(&Sample {
                    data: pkt.data,
                    duration: dur,
                    ..Default::default()
                }).await;
            }
        });
    }

    // --- Data channel for controller input (browser creates it; we just receive) ---
    {
        let input_tx = state.inner.input_tx.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let input_tx = input_tx.clone();
            Box::pin(async move {
                dc.on_message(Box::new(move |msg: DataChannelMessage| {
                    let input_tx = input_tx.clone();
                    Box::pin(async move {
                        if let Ok(ev) = serde_json::from_slice::<InputEvent>(&msg.data) {
                            let _ = input_tx.send(ev);
                        }
                    })
                }));
            })
        }));
    }

    // --- Signaling: apply offer, produce answer, gather ICE fully (no trickle) ---
    let offer = RTCSessionDescription::offer(offer_req.sdp)?;
    pc.set_remote_description(offer).await?;
    let answer = pc.create_answer(None).await?;
    let mut gather_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(answer).await?;
    let _ = gather_complete.recv().await; // block until ICE candidates gathered

    let local = pc
        .local_description()
        .await
        .ok_or_else(|| anyhow::anyhow!("no local description"))?;

    // NOTE: keep `pc` alive! If it drops, the connection closes. Store the Arc in AppState
    // (e.g. a Mutex<Option<Arc<RTCPeerConnection>>>) so it lives past this function.
    state.inner.keep_alive_peer(Arc::clone(&pc));

    Ok(AnswerResponse { sdp: local.sdp, kind: "answer".to_owned() })
}
```

webrtc 0.17.1 API facts (verified against source):
- `TrackLocalStaticSample::new(codec: RTCRtpCodecCapability, id: String, stream_id: String)` — 3 args.
- `track.write_sample(&Sample { data: Bytes, duration: Duration, ..Default::default() }).await` —
  `Sample` is `webrtc::media::Sample` (re-export of `webrtc_media::Sample`). Fields: `data: Bytes`,
  `timestamp: SystemTime`, `duration: Duration`, `packet_timestamp: u32`, `prev_dropped_packets: u16`,
  `prev_padding_packets: u16`. `..Default::default()` covers everything except `data`+`duration`.
- `RTCSessionDescription::offer(String) -> Result<...>` builds an offer from a raw SDP string. (The
  examples deserialize a full `RTCSessionDescription` from JSON; we accept just the `sdp` string and
  rebuild it — both work. You can also `serde_json::from_str::<RTCSessionDescription>` if the browser
  sends the whole object.)
- `pc.gathering_complete_promise().await` returns a receiver; `recv().await` blocks until ICE
  gathering finishes (this is the "no trickle ICE / single-shot signaling" pattern).
- `register_default_codecs()` registers VP8 + Opus among others; matching `MIME_TYPE_VP8` /
  `MIME_TYPE_OPUS` for the tracks ensures negotiation succeeds.
- Drain RTCP from each `RTCRtpSender` (the `sender.read(&mut buf)` loop) or NACK-based interceptors
  stall — this is load-bearing, not optional.
- **Lifetime gotcha:** the `RTCPeerConnection` is the owner of the whole session. If the `Arc<...>`
  is dropped at the end of the handler, the connection tears down. Keep it alive in `AppState`.

---

## 5. Encoders — exact APIs and the RGBA->I420 / f32->i16 glue

### 5a. VP8 via vpx-encode 0.6.2 (binds system libvpx 1.16.0)

API verified from `astraw/vpx-encode` `src/lib.rs`:

```rust
pub enum VideoCodecId { VP8, /* VP9 behind feature */ }

pub struct Config {
    pub width: u32,        // c_uint
    pub height: u32,       // c_uint
    pub timebase: [i32; 2],// [num, den], seconds. e.g. [1, 60] or [1001, 60000] for NTSC
    pub bitrate: u32,      // kilobits/sec
    pub codec: VideoCodecId,
}

impl Encoder {
    pub fn new(config: Config) -> Result<Self>;       // width/height MUST be even
    pub fn encode(&mut self, pts: i64, data: &[u8]) -> Result<Packets>; // data MUST be I420
    pub fn finish(self) -> Result<Finish>;
}

// Packets: Iterator<Item = Frame<'a>> where
pub struct Frame<'a> { pub data: &'a [u8], pub key: bool, pub pts: i64 }
```

CRITICAL: `encode()` calls `vpx_img_wrap(..., VPX_IMG_FMT_I420, ...)` and asserts
`2 * data.len() >= 3 * width * height`. **The input must be planar I420 (YUV 4:2:0)**, NOT the RGBA
that tetanes gives you. You must convert. 256x240 -> I420 size = `256*240 + 2*(128*120)` =
`61440 + 30720 = 92160` bytes.

Setup + per-frame use:

```rust
use vpx_encode::{Config, Encoder, VideoCodecId};

const W: usize = 256;
const H: usize = 240;

let mut vpx = Encoder::new(Config {
    width: W as u32,
    height: H as u32,
    timebase: [1, 60],     // pts will be a frame counter; 60 is close enough for VP8
    bitrate: 1_500,        // ~1.5 Mbps; tune. NES is low-detail, 1-2 Mbps is plenty.
    codec: VideoCodecId::VP8,
})?;

// per frame (pts = frame_index):
let mut i420 = vec![0u8; W*H + 2*((W/2)*(H/2))];
rgba_to_i420(rgba, W, H, &mut i420);      // §5c
for frame in vpx.encode(frame_index as i64, &i420)? {
    // frame.data is one VP8 frame (Bytes). Broadcast it:
    let _ = video_tx.send(EncodedVideo { data: bytes::Bytes::copy_from_slice(frame.data) });
}
```

Notes:
- `encode()` returns a `Packets` iterator; in real-time mode (the crate uses `VPX_DL_REALTIME`)
  it typically yields exactly one frame per call, but iterate to be safe.
- `frame.data` borrows the encoder's internal buffer — copy it (`Bytes::copy_from_slice`) before the
  next `encode()` call or before sending across the broadcast channel.
- Even dimensions: 256x240 are both even. Good.
- For lower latency you can force keyframes occasionally; default config emits keyframes as needed,
  which is fine for a viewer that connects mid-stream (VP8 decoders wait for the next keyframe). If a
  late joiner shows a gray/garbled screen until the next keyframe, shorten the keyframe interval (the
  vpx-encode 0.6.2 API doesn't expose kf settings directly — if you need it, you can drop to
  env-libvpx-sys / vpx-sys and set `c.kf_max_dist`, or just rely on periodic natural keyframes).

### 5b. Opus via opus 0.3.1 (binds system libopus 1.6.1)

API verified from docs.rs/opus 0.3.1:

```rust
use opus::{Encoder, Channels, Application};

// 48 kHz, mono (NES audio is mono), low-latency-ish:
let mut opus = Encoder::new(48_000, Channels::Mono, Application::Audio)?;
// encode_vec(input: &[i16], max_out: usize) -> Result<Vec<u8>>
// or encode(input: &[i16], out: &mut [u8]) -> Result<usize>
```

Opus needs **fixed frame sizes**. Use **960 samples = 20 ms @ 48 kHz, mono**. Since each emulator
frame produces ~799 f32 samples, buffer them:

```rust
const OPUS_FRAME: usize = 960; // 20ms @ 48k mono
let mut pcm_buf: Vec<i16> = Vec::with_capacity(4096);

// per emulator frame:
let pcm_f32: &[f32] = deck.audio_samples();
pcm_buf.extend(pcm_f32.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
deck.clear_audio_samples();

while pcm_buf.len() >= OPUS_FRAME {
    let chunk: Vec<i16> = pcm_buf.drain(..OPUS_FRAME).collect();
    let encoded = opus.encode_vec(&chunk, 4000)?; // 4000 = max output bytes
    let _ = audio_tx.send(EncodedAudio {
        data: bytes::Bytes::from(encoded),
        samples: OPUS_FRAME as u32,
    });
}
```

Notes:
- `tetanes-core` `audio_samples()` returns mono `&[f32]` in roughly `[-1.0, 1.0]`; clamp+scale to i16.
- Default tetanes APU sample rate is 44_100; we called `deck.set_sample_rate(48_000.0)` so the 960
  math lines up exactly with Opus 20ms frames. If you leave it at 44_100, use `960` still but the
  duration math must use 44100, and the browser may resample — cleaner to set 48000.
- `Application::Audio` (good general quality) vs `Application::LowDelay` (lowest latency, slightly
  lower quality). For a game, `LowDelay` is defensible; `Audio` is the safe default.

### 5c. RGBA -> I420 converter (hand-rolled, no extra deps)

tetanes `frame_buffer()` is 8-bit RGBA, row-major, 256x240. Standard BT.601 full-range-ish:

```rust
fn rgba_to_i420(rgba: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let y_plane = w * h;
    let c_w = w / 2;
    let c_h = h / 2;
    let (y, uv) = out.split_at_mut(y_plane);
    let (u, v) = uv.split_at_mut(c_w * c_h);

    // luma
    for j in 0..h {
        for i in 0..w {
            let p = (j * w + i) * 4;
            let r = rgba[p] as f32;
            let g = rgba[p + 1] as f32;
            let b = rgba[p + 2] as f32;
            y[j * w + i] = (0.299 * r + 0.587 * g + 0.114 * b).round().clamp(0.0, 255.0) as u8;
        }
    }
    // chroma (2x2 subsample)
    for j in 0..c_h {
        for i in 0..c_w {
            let p = ((j * 2) * w + (i * 2)) * 4;
            let r = rgba[p] as f32;
            let g = rgba[p + 1] as f32;
            let b = rgba[p + 2] as f32;
            u[j * c_w + i] = (-0.168736 * r - 0.331264 * g + 0.5 * b + 128.0).round().clamp(0.0,255.0) as u8;
            v[j * c_w + i] = (0.5 * r - 0.418688 * g - 0.081312 * b + 128.0).round().clamp(0.0,255.0) as u8;
        }
    }
}
```

This is ~250k float ops/frame * 60fps = trivial CPU. If you want it faster later, sample chroma from
the same pixel (already doing that) or use SIMD / the `yuv` crate. For v1, this is fine.

---

## 6. GOTCHAS (read before you `cargo build`)

1. **libvpx version mismatch — the #1 build failure.** `vpx-encode 0.6.2` -> `env-libvpx-sys 5.1.x`,
   which by default `include!`s a **pre-generated** FFI file named `vpx-ffi-<version>.rs`. The crate
   only ships bindings up to **1.13.0** (files: 1.3.0 … 1.10.0, 1.11.0, 1.12.0, 1.13.0). This machine
   has **libvpx 1.16.0**, so the default path errors with:
   `Expected file "vpx-ffi-1.16.0.rs" not found but 'generate' cargo feature not used.`
   **Fix (chosen):** enable `vpx-encode`'s `ffi-generate` feature (which turns on
   `env-libvpx-sys/generate`), so bindgen generates the FFI on the fly against the installed 1.16.0
   headers. Requires libclang — present at `/Library/Developer/CommandLineTools/usr/lib/libclang.dylib`
   (bindgen finds it automatically on macOS).
   **Alternative (not recommended):** `export VPX_VERSION=1.13.0` to force the 1.13 bundled binding
   against the 1.16 lib — risks ABI/struct-layout drift. Use `ffi-generate` instead.
   ```toml
   vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }
   ```

2. **pkg-config discovery.** If the build can't find vpx/opus:
   `export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig` and `export LIBRARY_PATH=/opt/homebrew/lib`.
   (Confirmed `pkg-config --modversion vpx`/`opus` work once PKG_CONFIG_PATH is set.)

3. **`opus` crate links system libopus** via pkg-config (good — uses homebrew's 1.6.1). The
   *other* common crate, `audiopus`, vendors and **builds its own Opus 1.3** unless you enable a
   `system` feature; prefer the `opus` crate to use the installed 1.6.1 and avoid a long C build.

4. **VP8 wants I420, not RGBA.** tetanes gives RGBA; you MUST convert (§5c) or `vpx_img_wrap` will
   interpret bytes wrong and you'll get a green/garbled or assertion-failing frame. The internal
   assert is `2*data.len() >= 3*w*h`.

5. **Opus frame sizing.** ~799 samples/NES-frame is not a legal Opus frame size. Buffer and emit
   960-sample (20ms@48k) packets (§5b), or Opus encode will error / desync.

6. **Keep the `RTCPeerConnection` alive.** Dropping the `Arc` closes the connection. Store it in
   `AppState` past the `/offer` handler.

7. **Drain RTCP** from every `RTCRtpSender` (`sender.read(...)` loop) — required for the default
   interceptors (NACK/reports), per the official example.

8. **Single-shot signaling = no trickle ICE.** We `gathering_complete_promise().await` before
   returning the answer. This adds ~0.5–2s to `/offer` on first connect (STUN gathering). Acceptable
   for v1; for production use trickle ICE + a WebSocket.

9. **`frame.data` from vpx-encode is a borrow** into the encoder's buffer — copy it before the next
   `encode()` call (use `Bytes::copy_from_slice`).

10. **macOS local STUN.** For localhost-only testing, ICE will use host candidates (127.0.0.1) and
    the STUN server line is harmless. If you ever go cross-network, you'll need a TURN server.

11. **`spawn_blocking` vs `tokio::spawn` for the emulator.** The encode loop is synchronous CPU work.
    Running it in `tokio::spawn` with `interval.tick().await` works (matches the official example) but
    can compete with the async scheduler. A dedicated `std::thread` (or `spawn_blocking`) with
    `Instant`-based pacing gives steadier 60fps. Either is acceptable; pick one and measure.

---

## 7. index.html (client) — structure, connect flow, input wire format

Single self-contained page served at `http://localhost:3000/`. It:
- shows a `<video>` element,
- on "Connect": creates `RTCPeerConnection`, adds a **recv-only** transceiver for audio+video (so the
  server's tracks land in `ontrack`), opens a **data channel** for input, creates an offer, POSTs it
  to `/offer`, applies the returned answer,
- captures `keydown`/`keyup`, maps keys to NES buttons, sends JSON over the data channel.

```html
<!doctype html>
<html>
<head><meta charset="utf-8"><title>NES over WebRTC</title></head>
<body>
  <h1>NES over WebRTC</h1>
  <button id="connect">Connect</button>
  <p id="status">idle</p>
  <video id="video" autoplay playsinline controls
         style="width:512px;height:480px;image-rendering:pixelated;background:#000"></video>

  <script>
  const KEYMAP = {           // keyboard -> NES button (wire name)
    "ArrowUp": "Up", "ArrowDown": "Down", "ArrowLeft": "Left", "ArrowRight": "Right",
    "KeyZ": "B", "KeyX": "A", "Enter": "Start", "ShiftRight": "Select", "ShiftLeft": "Select",
  };

  let pc, inputChannel;
  const statusEl = document.getElementById("status");
  const video = document.getElementById("video");

  document.getElementById("connect").onclick = connect;

  async function connect() {
    statusEl.textContent = "connecting...";
    pc = new RTCPeerConnection({
      iceServers: [{ urls: "stun:stun.l.google.com:19302" }],
    });

    // The server sends media; we only receive. Declare recvonly transceivers so the
    // answerer (server) attaches its tracks to these m-lines.
    pc.addTransceiver("video", { direction: "recvonly" });
    pc.addTransceiver("audio", { direction: "recvonly" });

    // Remote tracks -> attach to <video>. Audio + video share one MediaStream.
    const remote = new MediaStream();
    video.srcObject = remote;
    pc.ontrack = (e) => { remote.addTrack(e.track); };

    // Data channel for controller input (we create it; server receives via on_data_channel).
    inputChannel = pc.createDataChannel("input", { ordered: true });
    inputChannel.onopen = () => { statusEl.textContent = "connected"; };

    pc.oniceconnectionstatechange = () => {
      statusEl.textContent = "ice: " + pc.iceConnectionState;
    };

    // Create offer, then wait for ICE gathering to complete (no trickle; matches server).
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await waitIceGatheringComplete(pc);

    // POST the (now ICE-complete) offer to the server.
    const resp = await fetch("/offer", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ sdp: pc.localDescription.sdp, type: pc.localDescription.type }),
    });
    const answer = await resp.json();   // { sdp, type:"answer" }
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

  // --- Input: capture keys, send {type:'down'|'up', button:'A'} over the data channel ---
  const pressed = new Set();
  function sendInput(kind, button) {
    if (inputChannel && inputChannel.readyState === "open") {
      inputChannel.send(JSON.stringify({ type: kind, button }));
    }
  }
  window.addEventListener("keydown", (e) => {
    const btn = KEYMAP[e.code];
    if (!btn) return;
    e.preventDefault();
    if (pressed.has(e.code)) return; // de-dupe key-repeat
    pressed.add(e.code);
    sendInput("down", btn);
  });
  window.addEventListener("keyup", (e) => {
    const btn = KEYMAP[e.code];
    if (!btn) return;
    e.preventDefault();
    pressed.delete(e.code);
    sendInput("up", btn);
  });
  </script>
</body>
</html>
```

Client notes:
- Use `e.code` (physical key, e.g. `"KeyZ"`, `"ArrowUp"`, `"ShiftLeft"`) — stable regardless of layout,
  and it distinguishes left/right Shift.
- `recvonly` transceivers are essential: without them the SDP offer has no m-lines for the server to
  attach its tracks to, and you'll get no media.
- De-dupe `keydown` repeats with the `pressed` Set so you don't spam "down" events.
- `image-rendering: pixelated` keeps the upscaled NES output crisp.
- `playsinline` + `autoplay` + user-gesture (the Connect button click) satisfy browser autoplay
  policies so audio isn't muted.

---

## 8. NES button <-> keyboard mapping + wire format (authoritative table)

| NES button | Keyboard (`e.code`) | Wire `button` value | tetanes `JoypadBtn` |
|------------|---------------------|---------------------|---------------------|
| D-Pad Up    | `ArrowUp`           | `"Up"`     | `JoypadBtn::Up`     |
| D-Pad Down  | `ArrowDown`         | `"Down"`   | `JoypadBtn::Down`   |
| D-Pad Left  | `ArrowLeft`         | `"Left"`   | `JoypadBtn::Left`   |
| D-Pad Right | `ArrowRight`        | `"Right"`  | `JoypadBtn::Right`  |
| B           | `KeyZ`              | `"B"`      | `JoypadBtn::B`      |
| A           | `KeyX`              | `"A"`      | `JoypadBtn::A`      |
| Start       | `Enter`             | `"Start"`  | `JoypadBtn::Start`  |
| Select      | `ShiftLeft`/`ShiftRight` | `"Select"` | `JoypadBtn::Select` |

**Wire format (browser -> server over the data channel):** newline-free JSON, one message per event:
```json
{"type":"down","button":"A"}
{"type":"up","button":"A"}
```
- `type`: `"down"` (key pressed) | `"up"` (key released).
- `button`: one of `"A" "B" "Up" "Down" "Left" "Right" "Start" "Select"`.
- Server: `serde_json::from_slice::<InputEvent>(&msg.data)` -> `map_button(&ev.button)` ->
  `deck.joypad_mut(Player::One).set_button(btn, ev.kind == "down")`.

tetanes input facts (verified from `tetanes-core/src/input.rs` @ v0.14.1):
- `deck.joypad_mut(player: Player) -> &mut Joypad` (Player::One/Two/Three/Four).
- `Joypad::set_button(button: impl Into<JoypadBtnState>, pressed: bool)`; `JoypadBtn: Into<JoypadBtnState>`.
- `JoypadBtn` variants: `Left, Right, Up, Down, A, B, TurboA, TurboB, Select, Start`.
- `set_button` auto-prevents opposing D-pad directions unless `concurrent_dpad` is enabled — handy.

---

## 9. tetanes-core API quick reference (verified @ v0.14.1)

```rust
use tetanes_core::control_deck::ControlDeck;
use tetanes_core::input::{JoypadBtn, Player};

let mut deck = ControlDeck::new();                       // default NTSC
let mut rom: &[u8] = &rom_bytes;
deck.load_rom("MK1.nes", &mut rom)?;                     // load_rom(name: ToString, &mut impl Read)
deck.set_sample_rate(48_000.0);                          // match Opus (default is 44_100.0)

loop {
    deck.clock_frame()?;                                 // advance exactly one NTSC frame
    let rgba: &[u8] = deck.frame_buffer();               // 256*240*4 = 245_760 bytes, RGBA8
    let pcm:  &[f32] = deck.audio_samples();             // mono f32, ~ sample_rate/60 per frame
    // ... encode rgba (->I420->VP8) and pcm (->i16->Opus) ...
    deck.clear_audio_samples();                          // MUST clear or samples accumulate
    deck.joypad_mut(Player::One).set_button(JoypadBtn::A, true);
}
```
- Frame: **256 x 240, RGBA8** (`tetanes_core::video::Video::SIZE == ppu::FRAME*4`; PPU `WIDTH=256 HEIGHT=240`).
- Default video filter is Pixellate (clean RGBA). NTSC filter is available but more expensive; not needed.
- Audio: `&[f32]`, default rate **44_100 Hz** (`Apu::DEFAULT_SAMPLE_RATE`); set to 48_000 for Opus.
- `clock_frame()` returns `Result<()>`; `is_running()` / `frame_number()` available if needed.
- The MK1 ROM is mapper 4 (MMC3), which tetanes supports — `load_rom` errors on `UnimplementedMapper`,
  so a successful load confirms mapper support.

---

## Sources
- webrtc-rs (v0.17.1, examples): https://github.com/webrtc-rs/webrtc
- webrtc crate: https://crates.io/crates/webrtc , https://docs.rs/webrtc/0.17.1
- bevy_streaming (prior art, input-over-data-channel): https://github.com/rlamarche/bevy_streaming
- desktop-streaming (own-encoder -> webrtc-rs): https://github.com/JRF63/desktop-streaming
- rust-webrtc-client-server-example: https://github.com/zheland/rust-webrtc-client-server-example
- axum: https://crates.io/crates/axum (0.8.9), https://docs.rs/axum/latest
- tower-http: https://crates.io/crates/tower-http (0.6.11)
- vpx-encode: https://github.com/astraw/vpx-encode , https://docs.rs/vpx-encode/0.6.2
- env-libvpx-sys (bundled-bindings versions, generate feature): https://github.com/kbalt/env-libvpx-sys
- opus crate: https://docs.rs/opus/0.3.1
- tetanes / tetanes-core: https://github.com/lukexor/tetanes (v0.14.1), https://docs.rs/tetanes-core/0.14.1
