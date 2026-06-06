# WebRTC in Rust (`webrtc` crate / webrtc-rs) — Server→Browser Media Streaming Blueprint

Component for the NES-over-WebRTC server. This document grounds the **exact, current API of `webrtc` v0.17.1** against the real example source in the webrtc-rs GitHub repo (tag `v0.17.1`) and the crate's source tree. Every type name, method signature, import path, and serde shape below was read from real source, not guessed.

---

## 0. Version pins (all confirmed to resolve on crates.io as of 2026-06)

`webrtc` latest **stable** is `0.17.1` (released 2026-02-06). There is a `0.20.0-alpha.1` pre-release but it is alpha and the repo `master` has been restructured toward it (the example paths changed), so **pin the stable 0.17.1**, which is exactly what all the examples read below correspond to.

```toml
[dependencies]
# --- the star of the show ---
webrtc       = "0.17.1"          # confirmed: `cargo add webrtc --dry-run` -> v0.17.1

# --- async runtime + signaling HTTP server (axum is cleaner than the example's raw hyper) ---
tokio        = { version = "1.52", features = ["full"] }   # resolves to 1.52.3
axum         = "0.8"             # resolves to 0.8.9  (serves the page + /offer endpoint)
tower-http   = { version = "0.6", features = ["fs"] }      # 0.6.11; ServeDir for index.html
bytes        = "1"              # 1.11.1; MUST be the same major (1.x) as webrtc's `bytes = "1"`
serde        = { version = "1", features = ["derive"] }    # 1.0.228
serde_json   = "1"              # 1.0.150
anyhow       = "1"              # 1.0.102 (optional, examples use it)
```

> CRITICAL `bytes` note: `Sample.data` is `bytes::Bytes`. webrtc 0.17.1 declares `bytes = "1"`. As long as your `bytes` is `1.x` you share the same `Bytes` type (cargo unifies the 1.x semver range). Don't pin a different major.

> Build note: do NOT run a full `cargo build` of webrtc speculatively — the dep tree (tokio, rustls/openssl, sctp, dtls, ice, srtp, ...) is large. The default features pull in `openssl` (vendored) per the dry-run output: features shown were `openssl`, `pem`, `vendored-openssl`. On macOS arm64 this compiles fine but slowly the first time. If you prefer rustls, webrtc 0.17 still defaults to its bundled crypto stack; leave defaults unless you hit an OpenSSL build issue.

---

## 1. MediaEngine + VP8/Opus codecs + InterceptorRegistry + API

Two ways to register codecs. Both are real and used in examples.

### 1a. Easiest: `register_default_codecs()` (what `play-from-disk-vpx` and `data-channels` use)

This registers VP8, VP9, H264, Opus, etc. with their standard MIME types and clock rates automatically. Use this unless you need to constrain payload types.

```rust
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS, MIME_TYPE_VP8, MIME_TYPE_VP9};
use webrtc::api::APIBuilder;
use webrtc::interceptor::registry::Registry;

let mut m = MediaEngine::default();
m.register_default_codecs()?;                       // VP8 (video/VP8, 90000) + Opus (audio/opus, 48000/2) included

let mut registry = Registry::new();
registry = register_default_interceptors(registry, &mut m)?;  // NACK, RTCP reports, TWCC, etc.

let api = APIBuilder::new()
    .with_media_engine(m)
    .with_interceptor_registry(registry)
    .build();
```

The MIME constants (real, from `webrtc::api::media_engine`):
- `MIME_TYPE_VP8` = `"video/VP8"`
- `MIME_TYPE_VP9` = `"video/VP9"`
- `MIME_TYPE_OPUS` = `"audio/opus"`
- `MIME_TYPE_H264` = `"video/H264"`

### 1b. Explicit registration (what `reflect` uses) — exact field set

`register_codec` takes an `RTCRtpCodecParameters` and an `RTPCodecType`. This is the precise shape (note `clock_rate`, `channels`, `sdp_fmtp_line`, `rtcp_feedback` are all fields of `RTCRtpCodecCapability`):

```rust
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};

// VP8 video, clock 90000
m.register_codec(
    RTCRtpCodecParameters {
        capability: RTCRtpCodecCapability {
            mime_type: MIME_TYPE_VP8.to_owned(),   // "video/VP8"
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "".to_owned(),
            rtcp_feedback: vec![],                 // or fill with NACK/PLI feedback entries
        },
        payload_type: 96,
        ..Default::default()
    },
    RTPCodecType::Video,
)?;

// Opus audio, clock 48000, 2 channels
m.register_codec(
    RTCRtpCodecParameters {
        capability: RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),  // "audio/opus"
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: 111,
        ..Default::default()
    },
    RTPCodecType::Audio,
)?;
```

> For our server-pushes-media use case, **`register_default_codecs()` is the right choice** — it guarantees the negotiated codec list matches what Chrome/Firefox offer, so VP8+Opus get selected.

### Peer connection configuration

```rust
use std::sync::Arc;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;

let config = RTCConfiguration {
    ice_servers: vec![RTCIceServer {
        urls: vec!["stun:stun.l.google.com:19302".to_owned()],
        ..Default::default()
    }],
    ..Default::default()
};

let peer_connection = Arc::new(api.new_peer_connection(config).await?);
```

> For pure localhost (browser and server on the same machine) you can even use an **empty `ice_servers: vec![]`** — host candidates on 127.0.0.1 are enough. Keeping the Google STUN server is harmless and helps if you later test across machines.

---

## 2. VP8 video + Opus audio `TrackLocalStaticSample`, `add_track`, and `write_sample`

`TrackLocalStaticSample` is the track type you push **encoded frames** into (one `Sample` = one encoded frame for video, or one packet/page worth for audio). webrtc-rs handles RTP packetization internally based on the codec MIME type.

### Imports

```rust
use std::sync::Arc;
use std::time::Duration;
use bytes::Bytes;
use webrtc::api::media_engine::{MIME_TYPE_OPUS, MIME_TYPE_VP8};
use webrtc::media::Sample;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
```

### Create the tracks (constructor: `new(codec, id, stream_id)`)

```rust
// VP8 video track
let video_track = Arc::new(TrackLocalStaticSample::new(
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_VP8.to_owned(),  // "video/VP8"
        ..Default::default()                  // clock_rate/channels default; the engine fills them on negotiation
    },
    "video".to_owned(),       // track id
    "nes".to_owned(),         // stream id (group video+audio under the same stream id to bind them)
));

// Opus audio track
let audio_track = Arc::new(TrackLocalStaticSample::new(
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(), // "audio/opus"
        ..Default::default()
    },
    "audio".to_owned(),
    "nes".to_owned(),         // same stream id as video
));
```

> Tip: use the **same stream id** (`"nes"`) for both tracks so the browser groups them into one `MediaStream` (single `ontrack` MediaStream containing both). The example uses different ids; for a single `<video>` element, sharing the stream id is convenient.

### Add tracks (note the explicit cast to `Arc<dyn TrackLocal + Send + Sync>`)

```rust
let video_rtp_sender = peer_connection
    .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
    .await?;

let audio_rtp_sender = peer_connection
    .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>)
    .await?;
```

### MANDATORY: drain RTCP on each sender

You must spawn a reader loop on the returned `RTCRtpSender`, otherwise interceptors (NACK, etc.) stall. Straight from the examples:

```rust
tokio::spawn(async move {
    let mut rtcp_buf = vec![0u8; 1500];
    while let Ok((_, _)) = video_rtp_sender.read(&mut rtcp_buf).await {}
    Result::<(), webrtc::Error>::Ok(())
});
// ...same for audio_rtp_sender
```

### THE write call — exact `Sample` construction

`write_sample` signature (read from `track_local_static_sample.rs`):

```rust
// impl TrackLocalStaticSample
pub async fn write_sample(&self, sample: &Sample) -> Result<()>
```

`Sample` struct (read from the `webrtc-media` crate, re-exported as `webrtc::media::Sample`):

```rust
pub struct Sample {
    pub data: Bytes,                 // the encoded bitstream (VP8 frame / Opus packet)
    pub timestamp: SystemTime,       // wallclock; Default = SystemTime::now()
    pub duration: Duration,          // how long this sample plays — drives RTP timestamp advance
    pub packet_timestamp: u32,       // Default 0; leave 0, webrtc computes RTP ts from duration
    pub prev_dropped_packets: u16,   // Default 0
    pub prev_padding_packets: u16,   // Default 0
}
// impl Default { data: Bytes::new(), timestamp: SystemTime::now(), duration: 0, rest: 0 }
```

So you construct with `..Default::default()` and set only `data` + `duration`:

```rust
// VIDEO: one encoded VP8 frame at ~60fps NES => duration = 1/60s.
// `data` must be a `bytes::Bytes`. From a Vec<u8> the VP8 encoder gave you:
let encoded_vp8: Vec<u8> = /* one VP8 frame from your encoder */;
video_track
    .write_sample(&Sample {
        data: Bytes::from(encoded_vp8),
        duration: Duration::from_nanos(16_666_667), // 1/60s; for 30fps use 33_333_333
        ..Default::default()
    })
    .await?;

// AUDIO: one Opus packet. With 20ms framing (typical) duration = 20ms.
let opus_packet: Vec<u8> = /* one encoded Opus packet (e.g. 960 samples @48k = 20ms) */;
audio_track
    .write_sample(&Sample {
        data: Bytes::from(opus_packet),
        duration: Duration::from_millis(20),
        ..Default::default()
    })
    .await?;
```

> The `duration` field is what advances the RTP timestamp. For VP8 at clock 90000 a 1/60s frame advances the RTP ts by 1500; for Opus at clock 48000 a 20ms packet advances by 960. webrtc-rs computes this from `duration` automatically, so just pass the wall-clock duration of the chunk. Get it right or A/V will drift.

> Pacing: write samples at real-time rate. Use a `tokio::time::interval` ticker (the examples do) — for 60fps NES, an `interval(Duration::from_nanos(16_666_667))` and `ticker.tick().await` between frames. Don't burst all frames; you'll get massive packet loss.

> Wait for connection before pushing: gate the push loop on the connection being established. The examples use a `tokio::sync::Notify` fired from `on_ice_connection_state_change` when state == `RTCIceConnectionState::Connected`, and the push tasks `notify.notified().await` before the first write. See §6.

---

## 3. SIGNALING over plain HTTP (single non-trickle answer) — full axum handler

The examples use stdin paste or a raw-hyper `http_sdp_server`. For our app (page at `:3000`, browser POSTs offer JSON, gets answer JSON back) the clean implementation is an axum `POST /offer` handler. The WebRTC flow is identical to every example:

`set_remote_description(offer)` → `create_answer(None)` → `gathering_complete_promise()` → `set_local_description(answer)` → **`gather_complete.recv().await`** (blocks until ICE gathering done so the answer carries all candidates = non-trickle) → return `local_description()`.

### The serde shape is browser-compatible out of the box

`RTCSessionDescription` (read from `webrtc/src/peer_connection/sdp/session_description.rs`):

```rust
pub struct RTCSessionDescription {
    #[serde(rename = "type")]   // serializes as "type"
    pub sdp_type: RTCSdpType,   // serde: "offer" | "answer" | "pranswer" | "rollback"
    pub sdp: String,            // serializes as "sdp"
    #[serde(skip)]
    pub(crate) parsed: Option<SessionDescription>,  // skipped in (de)serialization
}
```

So the JSON on the wire is exactly `{"type":"offer","sdp":"..."}` — **identical to what `JSON.stringify(pc.localDescription)` produces in the browser.** You can `serde_json::from_str::<RTCSessionDescription>(&body)` the browser offer directly, and `serde_json::to_string(&answer)` to send back. (`parsed` is `None` after deserialize, but `set_remote_description` re-parses the SDP internally, so this is fine.)

### Full handler (axum 0.8) — receives offer JSON, returns answer JSON

```rust
use std::sync::Arc;
use axum::{extract::State, response::IntoResponse, Json};
use axum::http::StatusCode;
use webrtc::api::API;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

// You hold a prebuilt `Arc<API>` (MediaEngine+interceptors+registry) in app state and
// build a fresh RTCPeerConnection per offer.
#[derive(Clone)]
struct AppState {
    api: Arc<API>,
}

async fn offer_handler(
    State(state): State<AppState>,
    Json(offer): Json<RTCSessionDescription>,   // axum deserializes {"type","sdp"} straight into it
) -> Result<Json<RTCSessionDescription>, (StatusCode, String)> {
    // helper to map webrtc errors into HTTP 500
    let err500 = |e: webrtc::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());

    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };

    let pc = Arc::new(state.api.new_peer_connection(config).await.map_err(err500)?);

    // --- attach your VP8/Opus tracks + data channel handler here (see §2 and §4) ---
    // setup_media_and_input(&pc).await?;

    // 1. remote = browser offer
    pc.set_remote_description(offer).await.map_err(err500)?;

    // 2. create answer
    let answer = pc.create_answer(None).await.map_err(err500)?;

    // 3. promise that completes when ICE gathering finishes
    let mut gather_complete = pc.gathering_complete_promise().await;

    // 4. set local (starts UDP listeners + ICE gathering)
    pc.set_local_description(answer).await.map_err(err500)?;

    // 5. BLOCK until gathering done => single non-trickle answer with all candidates baked in
    let _ = gather_complete.recv().await;

    // 6. return the gathered local description as JSON
    match pc.local_description().await {
        Some(local_desc) => Ok(Json(local_desc)),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "no local description".into())),
    }

    // NOTE: `pc` is an Arc; to keep the connection alive after this handler returns,
    // move it into the media push task / store it in a session map. If `pc` drops
    // here the connection closes. See §6 for the keep-alive pattern.
}
```

### Wiring axum (serve the page + the /offer route)

```rust
use axum::{routing::{get_service, post}, Router};
use tower_http::services::ServeDir;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // build the Arc<API> once (MediaEngine::default + register_default_codecs + interceptors)
    let api = build_api()?;                    // returns Arc<webrtc::api::API>
    let state = AppState { api };

    let app = Router::new()
        .route("/offer", post(offer_handler))
        // serve ./web/index.html (and assets) as the static site
        .fallback_service(get_service(ServeDir::new("web")))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app).await?;        // axum 0.8 serve API
    Ok(())
}
```

> `build_api()` must return `Arc<webrtc::api::API>`. `API` is the type `APIBuilder::build()` returns; import it as `use webrtc::api::API;`. It is `Send + Sync` and cheaply shareable, so build it once and create many peer connections from it.

---

## 4. DataChannel `input` — receive controller input from the browser

Two architectures. **For our case the browser opens the channel and the server listens** (browser is the offerer and creates the `input` channel in its offer; server reacts via `on_data_channel`). This is the simplest because the server is answering.

### Imports

```rust
use std::sync::Arc;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
```

`DataChannelMessage` (read from source):
```rust
pub struct DataChannelMessage {
    pub is_string: bool,   // true if the peer sent a text frame
    pub data: Bytes,       // the payload
}
```

### Server side: react to the browser-created channel via `on_data_channel`

`on_data_channel` takes `OnDataChannelHdlrFn = Box<dyn FnMut(Arc<RTCDataChannel>) -> Pin<Box<dyn Future<Output=()> + Send>> + Send + Sync>`. Pattern (adapted from the `data-channels` example):

```rust
// Call this on the per-connection `pc` BEFORE set_remote_description / create_answer.
fn setup_input_channel(pc: &Arc<webrtc::peer_connection::RTCPeerConnection>) {
    pc.on_data_channel(Box::new(move |d: Arc<RTCDataChannel>| {
        let label = d.label().to_owned();
        println!("DataChannel opened by browser: '{label}' (id={})", d.id());

        Box::pin(async move {
            if d.label() == "input" {
                d.on_open(Box::new(move || {
                    println!("input channel open");
                    Box::pin(async {})
                }));

                d.on_message(Box::new(move |msg: DataChannelMessage| {
                    if msg.is_string {
                        // controller input as JSON / text, e.g. {"key":"A","down":true}
                        if let Ok(s) = String::from_utf8(msg.data.to_vec()) {
                            // parse and forward to the emulator's input queue here:
                            // input_tx.send(parse_key(&s)).ok();
                            println!("input: {s}");
                        }
                    } else {
                        // or a compact binary protocol: msg.data is Bytes
                        // e.g. [button_bitmask_lo, button_bitmask_hi]
                        let bytes = &msg.data;
                        // forward bytes to emulator...
                        println!("input bytes: {bytes:?}");
                    }
                    Box::pin(async {})
                }));

                d.on_close(Box::new(|| {
                    println!("input channel closed");
                    Box::pin(async {})
                }));
            }
        })
    }));
}
```

> To get the input out of the closure and into your emulator loop, capture a `tokio::sync::mpsc::Sender` (clone it into the closure with `move`). On each `on_message`, `tx.try_send(button_state)` (or `tx.send(...).await` inside the returned future). The emulator's frame loop polls the latest controller state from the receiver.

### Send signatures (if you ever push from server → browser)

```rust
// impl RTCDataChannel
pub async fn send(&self, data: &bytes::Bytes) -> Result<usize>      // binary
pub async fn send_text(&self, s: impl Into<String>) -> Result<usize> // text
pub fn label(&self) -> &str
pub fn id(&self) -> u16
```

### Alternative: server creates the channel (only if server is the offerer)

`let dc = pc.create_data_channel("input", None).await?;` with an optional `RTCDataChannelInit { ordered, max_packet_life_time, max_retransmits, protocol, negotiated }`. Since our browser is the offerer, prefer `on_data_channel` and let the **browser** call `pc.createDataChannel("input")` — see §5. (For low-latency input you may set `ordered: false` + `max_retransmits: 0` on the browser side.)

---

## 5. Browser-side JavaScript (offerer; receives media, sends input)

Standard W3C WebRTC API. The browser is the **offerer**: it adds recvonly transceivers for audio+video, creates the `input` data channel, POSTs the offer JSON to `/offer`, applies the answer, and attaches the incoming `MediaStream` to a `<video>`.

```html
<!-- web/index.html -->
<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>NES over WebRTC</title></head>
<body>
  <video id="screen" autoplay playsinline></video>
  <p id="status">connecting…</p>
  <script>
  async function start() {
    const statusEl = document.getElementById('status');
    const video = document.getElementById('screen');

    const pc = new RTCPeerConnection({
      iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
    });

    // We only RECEIVE media from the server. Declare recvonly transceivers so the
    // offer advertises a media section the server can answer with its sendonly tracks.
    pc.addTransceiver('video', { direction: 'recvonly' });
    pc.addTransceiver('audio', { direction: 'recvonly' });

    // Attach incoming server media to the <video>. With matching stream ids on the
    // server, ev.streams[0] carries both audio+video.
    const inboundStream = new MediaStream();
    pc.ontrack = (ev) => {
      // simplest: use the stream the server grouped
      if (ev.streams && ev.streams[0]) {
        video.srcObject = ev.streams[0];
      } else {
        inboundStream.addTrack(ev.track);
        video.srcObject = inboundStream;
      }
    };

    pc.onconnectionstatechange = () => {
      statusEl.textContent = 'state: ' + pc.connectionState;
    };

    // Controller input channel (browser is the creator => server sees it via on_data_channel).
    // For low-latency input, make it unreliable/unordered:
    const input = pc.createDataChannel('input', {
      ordered: false,
      maxRetransmits: 0,
    });
    input.onopen = () => {
      // send key events as JSON; or a compact binary protocol via input.send(Uint8Array)
      const send = (key, down) => {
        if (input.readyState === 'open') {
          input.send(JSON.stringify({ key, down }));
        }
      };
      window.addEventListener('keydown', (e) => { if (!e.repeat) send(e.key, true); });
      window.addEventListener('keyup',   (e) => send(e.key, false));
    };

    // 1. create offer + set local
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);

    // 2. WAIT for ICE gathering to complete so we POST a single non-trickle offer
    //    (matches the server which returns a single non-trickle answer)
    await new Promise((resolve) => {
      if (pc.iceGatheringState === 'complete') return resolve();
      const check = () => {
        if (pc.iceGatheringState === 'complete') {
          pc.removeEventListener('icegatheringstatechange', check);
          resolve();
        }
      };
      pc.addEventListener('icegatheringstatechange', check);
    });

    // 3. POST the full offer JSON to the server
    const resp = await fetch('/offer', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      // pc.localDescription serializes to {"type":"offer","sdp":"..."} — exactly what the
      // Rust RTCSessionDescription expects.
      body: JSON.stringify(pc.localDescription),
    });

    // 4. apply the server's answer
    const answer = await resp.json();           // {"type":"answer","sdp":"..."}
    await pc.setRemoteDescription(answer);

    statusEl.textContent = 'offer sent, waiting for media…';
  }
  start().catch((e) => {
    document.getElementById('status').textContent = 'error: ' + e;
    console.error(e);
  });
  </script>
</body>
</html>
```

> Why `addTransceiver(..., recvonly)` instead of nothing: the browser offer must contain `m=video` and `m=audio` sections for the server's sendonly tracks to be negotiated. Declaring recvonly transceivers guarantees those sections exist. Alternatively the server can `add_track` first and the answer will populate them — but having the browser explicitly request recvonly is the robust pattern.

> Autoplay: keep `autoplay playsinline` on the `<video>`. Browsers block autoplay **with audio** until a user gesture. For a watch-only page, either (a) start `muted` then unmute on a click, or (b) require a click-to-start button that calls `start()`. The keyboard listeners also need the page focused.

---

## 6. End-to-end glue: keeping the connection + push loop alive

The `/offer` handler returns the answer, but the `RTCPeerConnection` must outlive the handler and a task must push encoded frames. Recommended structure:

```rust
// inside offer_handler, after building `pc` and BEFORE returning the answer:

// gate frame pushing on connection established
let notify = Arc::new(tokio::sync::Notify::new());
let notify_conn = notify.clone();
pc.on_ice_connection_state_change(Box::new(move |st: webrtc::ice_transport::ice_connection_state::RTCIceConnectionState| {
    if st == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected {
        notify_conn.notify_waiters();
    }
    Box::pin(async {})
}));

// spawn the media push task; move `pc` (or clones of the tracks) so they stay alive
let video_track2 = Arc::clone(&video_track);
let audio_track2 = Arc::clone(&audio_track);
let pc_keepalive = Arc::clone(&pc);   // keep pc alive for the lifetime of the stream
tokio::spawn(async move {
    let _hold = pc_keepalive;         // do not drop until the task ends
    notify.notified().await;          // wait until ICE Connected
    // run emulator frame loop:
    //   loop {
    //     let (vp8, opus) = emulator.step_one_frame_and_encode();
    //     video_track2.write_sample(&Sample { data: Bytes::from(vp8), duration: FRAME_DUR, ..Default::default() }).await?;
    //     audio_track2.write_sample(&Sample { data: Bytes::from(opus), duration: AUDIO_DUR, ..Default::default() }).await?;
    //     ticker.tick().await;       // pace to real time
    //   }
});
```

Connection lifecycle handler (close detection):
```rust
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
    println!("peer state: {s}");
    // on RTCPeerConnectionState::Failed / Disconnected / Closed -> stop the emulator task,
    // (e.g. signal a shutdown channel) so the spawned push task ends and `pc` drops.
    Box::pin(async {})
}));
```

---

## 7. Exact import cheat-sheet (everything, copy-paste)

```rust
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use webrtc::api::API;
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS, MIME_TYPE_VP8, MIME_TYPE_VP9};
use webrtc::interceptor::registry::Registry;

use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;

use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;

use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};

use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use webrtc::media::Sample;

use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
```

Method-signature reference (all read from v0.17.1 source):

| Call | Signature |
|---|---|
| `MediaEngine::default()` | `-> MediaEngine` |
| `m.register_default_codecs()` | `-> Result<()>` |
| `m.register_codec(params, kind)` | `(RTCRtpCodecParameters, RTPCodecType) -> Result<()>` |
| `register_default_interceptors(reg, &mut m)` | `(Registry, &mut MediaEngine) -> Result<Registry>` |
| `APIBuilder::new().with_media_engine(m).with_interceptor_registry(r).build()` | `-> API` |
| `api.new_peer_connection(config)` | `(RTCConfiguration) -> Result<RTCPeerConnection>` |
| `TrackLocalStaticSample::new(cap, id, stream_id)` | `(RTCRtpCodecCapability, String, String) -> Self` |
| `pc.add_track(track)` | `(Arc<dyn TrackLocal + Send + Sync>) -> Result<Arc<RTCRtpSender>>` |
| `rtp_sender.read(&mut buf)` | `(&mut [u8]) -> Result<(usize, Attributes)>` |
| `track.write_sample(&sample)` | `(&Sample) -> Result<()>` |
| `pc.set_remote_description(desc)` | `(RTCSessionDescription) -> Result<()>` |
| `pc.create_answer(None)` | `(Option<RTCAnswerOptions>) -> Result<RTCSessionDescription>` |
| `pc.gathering_complete_promise()` | `-> mpsc::Receiver<()>` |
| `pc.set_local_description(desc)` | `(RTCSessionDescription) -> Result<()>` |
| `pc.local_description()` | `-> Option<RTCSessionDescription>` |
| `pc.on_data_channel(f)` | `(OnDataChannelHdlrFn)` — `FnMut(Arc<RTCDataChannel>) -> Pin<Box<dyn Future<Output=()> + Send>>` |
| `pc.on_track(f)` | `FnMut(Arc<TrackRemote>, Arc<RTCRtpReceiver>, Arc<RTCRtpTransceiver>) -> Pin<Box<...>>` |
| `pc.on_peer_connection_state_change(f)` | `FnMut(RTCPeerConnectionState) -> Pin<Box<...>>` |
| `pc.on_ice_connection_state_change(f)` | `FnMut(RTCIceConnectionState) -> Pin<Box<...>>` |
| `dc.on_message(f)` | `FnMut(DataChannelMessage) -> Pin<Box<...>>` |
| `dc.on_open(f)` / `dc.on_close(f)` | `FnMut() -> Pin<Box<...>>` |
| `dc.send(&Bytes)` / `dc.send_text(s)` | `-> Result<usize>` |
| `pc.close()` | `-> Result<()>` |

---

## 8. Gotchas / pitfalls (load-bearing)

1. **`add_track` requires an explicit cast** `Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>`. Without the cast it won't compile.
2. **You must drain RTCP** on every `RTCRtpSender` (the `rtp_sender.read` loop). Interceptors block otherwise.
3. **`gather_complete.recv().await` must come AFTER `set_local_description`** and BEFORE reading `local_description()`. Order matters: create_answer → `gathering_complete_promise()` → set_local_description → `recv().await`. This produces a complete non-trickle answer (essential since we exchange exactly one HTTP round trip — no trickle ICE).
4. **`Sample.duration` drives RTP timestamps.** Set it to the real playback duration of the chunk (1/fps for video, frame size for audio). Wrong durations = A/V drift and broken playback. Don't leave it at the example's bogus `Duration::from_secs(1)` (that's only OK because the IVF reader is paced separately).
5. **`bytes::Bytes` must be the same crate version as webrtc's.** Use `bytes = "1"`. `Bytes::from(vec)` is zero-copy-ish and the idiomatic conversion.
6. **The peer connection drops when its last `Arc` drops.** The `/offer` handler returns; you must move `Arc<RTCPeerConnection>` (or the tracks + a keepalive Arc) into the spawned media task, or stash it in a session map, or the connection closes immediately after answering.
7. **Browser autoplay-with-audio is blocked without a gesture.** Use a click-to-start button or start muted. For the "watch a game" page this matters.
8. **Declare recvonly transceivers in the browser** (`pc.addTransceiver('video'/'audio', {direction:'recvonly'})`) so the offer has media sections the server can answer into.
9. **Serde shape is already browser-identical**: `{"type":"offer|answer","sdp":"..."}`. Deserialize the browser offer straight into `RTCSessionDescription`; serialize the answer straight back. The `parsed` field is `#[serde(skip)]` and re-derived internally.
10. **`create_answer`/`create_offer` take `Option<...Options>`** — pass `None`.
11. **localhost ICE**: with both ends on 127.0.0.1, host candidates suffice; `ice_servers` can even be empty. STUN is harmless to leave in.
12. **Default features compile OpenSSL (vendored).** First build is slow; subsequent builds cache. Env from the project facts (`PKG_CONFIG_PATH`/`LIBRARY_PATH` to `/opt/homebrew/lib`) is for libvpx/libopus, not for webrtc itself, but keeping them set does no harm.

---

## 9. Sources (read directly, tag v0.17.1)

- `examples/examples/play-from-disk-vpx/play-from-disk-vpx.rs` — MediaEngine, default codecs, `TrackLocalStaticSample`, `add_track`, `write_sample(&Sample{...})`, full signaling flow, Notify gating.
- `examples/examples/reflect/reflect.rs` — explicit `register_codec` for VP8/Opus with full `RTCRtpCodecCapability` fields, `on_track`.
- `examples/examples/data-channels/data-channels.rs` — `on_data_channel` / `on_open` / `on_message` / `on_close`, `DataChannelMessage`.
- `examples/examples/data-channels-create/data-channels-create.rs` — `create_data_channel`, offerer flow.
- `examples/examples/broadcast/broadcast.rs` + `examples/examples/signal/src/lib.rs` — HTTP SDP server pattern (raw hyper), base64 encode/decode (we replace with axum + raw JSON).
- `webrtc/src/peer_connection/sdp/session_description.rs` + `sdp_type.rs` — serde shape `{"type","sdp"}`, `RTCSdpType` rename map.
- `webrtc-media` crate `src/lib.rs` — `Sample` struct + `Default` impl.
- `webrtc/src/track/track_local/track_local_static_sample.rs` — `new` + `write_sample` signatures.
- `webrtc/src/data_channel/{mod.rs, data_channel_message.rs, data_channel_init.rs}` — send sigs, message + init structs.
- `webrtc/src/peer_connection/mod.rs` — handler fn type aliases and method signatures.
- crates.io / `cargo add --dry-run` — version pins.
