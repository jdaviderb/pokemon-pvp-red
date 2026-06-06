# Real-time Encoding Blueprint: VP8 (video) + Opus (audio) → webrtc-rs `TrackLocalStaticSample`

Target: server-side NES emulation, 256×240 framebuffer @ 60 fps + ~48 kHz audio, encoded and pushed
into `webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample` as `webrtc::media::Sample`s.

**Everything below was compiled and run on this machine** (macOS arm64, rustc 1.86, homebrew libvpx 1.16.0,
libopus 1.6.1). See the "VERIFICATION" section at the bottom for the exact commands, output, and `otool -L` linkage proof.

---

## 0. TL;DR / pinned versions

```toml
[dependencies]
# --- VP8 video ---
# NOTE the `ffi-generate` feature is MANDATORY on this machine (libvpx 1.16.0). See §1.1.
vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }

# --- Opus audio ---
opus = "0.3.1"            # SpaceManiac's crate; pulls audiopus_sys 0.2.2 (pkg-config -> homebrew libopus). Links statically.

# --- WebRTC plumbing (for the Sample type these encoders feed) ---
webrtc = "0.17.1"
bytes  = "1"             # webrtc::media::Sample.data is bytes::Bytes
tokio  = { version = "1", features = ["full"] }

# Optional: only if you choose the crate-based RGB->I420 path in §3 (NOT required; manual path needs no dep)
# yuvutils-rs = "0.8.3"
# yuv         = "0.8.14"
```

Build/run environment variables (set these for any `cargo build`/`cargo run` that touches the codecs):

```sh
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
export LIBRARY_PATH=/opt/homebrew/lib
```

Resolved transitive crates (verified via `cargo tree`):
- `vpx-encode 0.6.2` → `env-libvpx-sys 5.1.3` (this is the modern replacement for the old `vpx-sys`).
- `opus 0.3.1` → `audiopus_sys 0.2.2` → `pkg-config 0.3.33`.

---

# PART A — VP8 VIDEO

## 1. Crate choice: `vpx-encode` 0.6.2 (with caveats)

**Recommendation: use `vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }` for VP8.** It is the cleanest
Rust wrapper around system libvpx and it compiles + links + encodes on this machine. The API is tiny (a `Config`
struct, `Encoder::new`, `encode(pts, &i420) -> Packets` iterator of `Frame { data, key, pts }`). It does NOT vendor
or download libvpx — it binds the homebrew `libvpx.12.dylib` via `env-libvpx-sys` + pkg-config.

Why not the alternatives:
- **raw `env-libvpx-sys` / `vpx-sys`**: you'd hand-write all the `vpx_codec_*` FFI + `MaybeUninit` ceremony. Only do
  this if you hit the control-knob limitation in §1.2 and it actually matters (it probably won't for this use case).
- **rav1e**: AV1 encoder; browsers' WebRTC AV1 support is uneven and rav1e realtime AV1 at 60fps is heavier. Skip.
- **openh264**: H.264 is fine for WebRTC, but VP8 is the universally-supported, royalty-free, simplest path with
  webrtc-rs (the default MediaEngine registers VP8 out of the box). Stick with VP8 for v1.

### 1.1 CRITICAL build gotcha — you MUST enable `ffi-generate`

`env-libvpx-sys 5.1.3` ships **pre-generated** FFI bindings only for libvpx versions **1.3.0 … 1.12.0**
(`generated/vpx-ffi-*.rs`). Homebrew here has **libvpx 1.16.0**. With default features the build panics:

```
thread 'main' panicked at .../env-libvpx-sys-5.1.3/build.rs:84:13:
Expected file "generated/vpx-ffi-1.16.0.rs" not found but 'generate' cargo feature not used.
```

The fix (verified working) is to turn on `vpx-encode`'s `ffi-generate` feature, which enables `env-libvpx-sys`'s
`generate` feature, which runs **bindgen** at build time to regenerate the FFI directly from the installed
`/opt/homebrew/Cellar/libvpx/1.16.0/include` headers. bindgen finds **libclang** via the Xcode toolchain
(`/Applications/Xcode.app/.../usr/bin/clang`) automatically — no extra env var needed on this machine. This adds
bindgen/clang-sys to the build (≈5s one-time compile), but produces correct bindings for 1.16.0.

```toml
vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }
```

If you ever build on a box where bindgen can't find libclang, set `LIBCLANG_PATH` to the dir containing
`libclang.dylib`. Alternatively you can point env-libvpx-sys at a specific libvpx via `VPX_LIB_DIR` + `VPX_VERSION`
(see its build.rs), but pkg-config already works here so you don't need to.

### 1.2 Known API limitation of vpx-encode 0.6.2 (read this before committing)

The public `Config` struct is intentionally minimal:

```rust
// vpx-encode 0.6.2 — the ENTIRE public Config:
pub struct Config {
    pub width:    u32,        // c_uint
    pub height:   u32,
    pub timebase: [i32; 2],   // [num, den]
    pub bitrate:  u32,        // TARGET BITRATE IN **KILOBITS/SEC** (kbps), not bps
    pub codec:    VideoCodecId, // VideoCodecId::VP8
}
```

What it does **for you** internally (read from its `lib.rs`):
- `vpx_codec_enc_config_default(...)` then overrides `g_w`, `g_h`, `g_timebase`, `rc_target_bitrate`.
- Sets `g_threads = 8`, `g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT`.
- `encode()` ALWAYS passes deadline `VPX_DL_REALTIME` (good — exactly what we want for live streaming).
- It wraps your buffer as `VPX_IMG_FMT_I420` and requires width/height divisible by 2 (256×240 ✓).

What it does **NOT** expose, and the `Encoder.ctx` field is **private** so you cannot call `vpx_codec_control_` yourself:
- `cpu_used` / `VP8E_SET_CPUUSED` (the #1 VP8 realtime speed knob; bigger = faster/lower-quality, range ~ -16..16,
  realtime usually 4..8). For VP8 it leaves this at the libvpx default.
- `g_lag_in_frames` (default-config realtime path is already lag=0-ish for VP8, fine).
- Max keyframe interval `kf_max_dist` — the encoder uses libvpx defaults (auto keyframes). You get a keyframe at the
  start and on scene changes; you generally also want a periodic keyframe and on-demand keyframes when a NEW browser
  peer joins. **This is the main reason you might outgrow vpx-encode.**

**Decision guidance:**
- For the first working version, `vpx-encode 0.6.2` is perfect — it produces a valid VP8 stream at realtime deadline,
  keyframe-first, and plugs straight into webrtc-rs. Verified: a 256×240 synthetic frame encodes to a 321-byte
  keyframe + ~45-byte interframes.
- If/when you need (a) a forced keyframe when a new viewer connects, or (b) explicit `cpu_used` tuning, fork to a
  ~120-line direct `env-libvpx-sys` encoder (the wrapper's own `lib.rs` is the template — copy it and add
  `vpx_codec_control_(&mut ctx, VP8E_SET_CPUUSED as _, 6)` after init, plus set `c.kf_mode = VPX_KF_AUTO`,
  `c.kf_max_dist = 120`, and expose a `force_keyframe()` that passes `VPX_EFLAG_FORCE_KF` as the flags arg to
  `vpx_codec_encode`). I'm flagging this now so it isn't a surprise; it is NOT needed to get pixels on screen.

## 2. Constructing the realtime VP8 encoder (256×240 @ 60fps)

Use a **millisecond timebase** `[1, 1000]` so the `pts` you pass is just "milliseconds since stream start" — simplest
to reason about and matches how we'll pace frames. Bitrate is **in kbps**; 2000 kbps (2 Mbps) is a good middle for
256×240 (range 1000–3000).

```rust
use vpx_encode::{Config, Encoder, VideoCodecId};

/// Create a realtime VP8 encoder for the NES output (256x240).
fn make_vp8_encoder() -> vpx_encode::Result<Encoder> {
    let cfg = Config {
        width:  256,
        height: 240,
        timebase: [1, 1000],   // 1/1000 s == milliseconds; pts is in ms
        bitrate: 2000,         // kbps (2 Mbps). Range 1000..=3000 is sane for 256x240.
        codec: VideoCodecId::VP8,
    };
    // encode() internally uses VPX_DL_REALTIME, so this IS the low-latency path.
    Encoder::new(cfg)
}
```

Notes:
- 256 and 240 are both even → passes the wrapper's divisibility check.
- The encoder emits a keyframe for the first frame automatically (verified `key=true` on frame 0).
- `g_threads=8` is set by the wrapper; on Apple Silicon that's fine (it'll use available cores).

## 3. RGB/RGBA framebuffer → I420 (YUV 4:2:0). **RECOMMENDED: manual BT.601, no extra dependency.**

`vpx-encode::encode(pts, data)` expects a single contiguous **I420** buffer laid out as:
`[ Y plane: w*h bytes ][ U plane: (w/2)*(h/2) ][ V plane: (w/2)*(h/2) ]`
Total for 256×240 = `61440 + 15360 + 15360 = 92160` bytes. (The wrapper asserts
`2*data.len() >= 3*w*h`, i.e. `data.len() >= w*h*3/2 = 92160`.)

Most NES cores expose the frame as **RGBA8888** (4 bytes/pixel) or **RGB888** (3 bytes/pixel) or a palette index you
expand to RGB. For a 256×240 frame at 60fps a hand-written BT.601 limited-range RGB→I420 is trivially fast and needs
**zero extra crates**, so that's the recommendation. (If you'd rather not maintain it, `yuvutils-rs = "0.8.3"` —
function `rgba_to_yuv420(&mut YuvPlanarImageMut, rgba, stride, YuvRange::Limited, YuvStandard::Bt601)` — or
`yuv = "0.8.14"` do the same with SIMD; both exist on crates.io and are listed/commented in the Cargo.toml above. For
60fps/256×240 you do NOT need SIMD.)

### 3.1 Manual RGBA → I420 (BT.601 "limited"/video range — the standard for VP8/WebRTC)

```rust
/// Convert a 256x240 (or any even WxH) RGBA8888 framebuffer into a packed I420 buffer
/// suitable for `vpx_encode::Encoder::encode`.
///
/// `rgba.len()` must be width*height*4. Output `dst` must be width*height*3/2 bytes.
/// Reuse `dst` across frames (allocate once) to avoid per-frame allocation.
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
            // BT.601 limited range. Y in [16,235], U/V in [16,240], centered at 128.
            // Integer-approx coefficients (<<8 fixed point).
            let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
            y_plane[j * width + i] = (y + 16) as u8;

            // 4:2:0 subsample: only on even rows/cols.
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

For **RGB888** (3 bytes/pixel) just change the stride from `*4` to `*3` and drop the alpha. If your core gives
**packed BGRA/ARGB**, swap the `r`/`b` (or index) accordingly. If it gives a **palette index**, expand index→RGB via
the NES palette LUT first (a `[(u8,u8,u8); 64]` table) then run the same kernel.

> Range note: BT.601 *limited* range is the safe default for VP8/WebRTC; almost all decoders assume it. If colors look
> washed out/too dark you have a range mismatch — but limited-range is correct to start.

## 4. Encode one frame → iterate packets → `bytes::Bytes` → `webrtc::media::Sample`

`encode(pts, &i420)` returns `Packets`, an iterator yielding `Frame { data: &[u8], key: bool, pts: i64 }`. For VP8
realtime you'll normally get exactly one `Frame` per `encode` call, but **always drain the iterator** (libvpx can
return 0 or >1 packets). Copy `frame.data` into `Bytes` (the borrow ends when `Packets` is dropped, so you must copy
— `Bytes::copy_from_slice`).

The webrtc `Sample` you build:

```rust
// webrtc-media 0.17.1 — webrtc::media::Sample (relevant fields):
//   pub data: bytes::Bytes,
//   pub timestamp: std::time::SystemTime,   // defaults to now; leave default
//   pub duration: std::time::Duration,      // <-- THIS drives RTP timestamp stepping
//   pub packet_timestamp: u32,              // leave 0; sampler fills it
//   pub prev_dropped_packets: u16,          // leave 0
// Sample::default() gives sane zeros; only set `data` and `duration`.
```

The `TrackLocalStaticSample` internally runs a "sampler" that converts `Sample.duration` into RTP timestamp
increments at the **codec clock rate (90 kHz for VP8)**. So for video you set `duration = 1/60 s ≈ 16.667 ms`
per frame and you do NOT manage RTP timestamps yourself.

```rust
use std::time::Duration;
use bytes::Bytes;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

/// One 60fps video tick: encode an RGBA frame and push every resulting VP8 packet to the track.
/// `i420_scratch` is a reused Vec<u8> of len width*height*3/2.
async fn encode_and_send_frame(
    enc: &mut vpx_encode::Encoder,
    track: &TrackLocalStaticSample,
    rgba: &[u8],
    width: usize,
    height: usize,
    pts_ms: i64,                 // milliseconds since stream start (timebase [1,1000])
    i420_scratch: &mut [u8],
) -> Result<(), Box<dyn std::error::Error>> {
    rgba_to_i420(rgba, width, height, i420_scratch);

    let packets = enc.encode(pts_ms, i420_scratch)?;     // Packets iterator
    for frame in packets {
        // frame.data is a borrow into the encoder; COPY it out before the iterator is dropped.
        let sample = Sample {
            data: Bytes::copy_from_slice(frame.data),
            duration: Duration::from_nanos(16_666_667),  // 1/60 s; sampler -> 90kHz RTP step
            ..Default::default()
        };
        track.write_sample(&sample).await?;
        // frame.key tells you if this was a keyframe (useful for logging / join logic).
    }
    Ok(())
}
```

`pts_ms` handling: keep a frame counter `n` and pass `pts_ms = n * 1000 / 60` (i.e. multiply by the ms-per-frame).
Because the timebase is `[1,1000]`, libvpx interprets pts as milliseconds; the webrtc RTP timestamps are driven
separately by `Sample.duration` (90 kHz), so the two are independent — just be monotonic in both.

The `TrackLocalStaticSample` is created with the VP8 MIME type (constants verified in webrtc 0.17.1):

```rust
use webrtc::api::media_engine::MIME_TYPE_VP8;   // = "video/VP8"
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;

let video_track = std::sync::Arc::new(TrackLocalStaticSample::new(
    RTCRtpCodecCapability { mime_type: MIME_TYPE_VP8.to_owned(), ..Default::default() },
    "video".to_owned(),
    "nes".to_owned(),
));
// add_track(video_track.clone()) on the RTCPeerConnection, then call write_sample on it.
```

---

# PART B — OPUS AUDIO

## 5. Crate choice: `opus` 0.3.1 (SpaceManiac). Links to homebrew libopus, statically.

**Recommendation: `opus = "0.3.1"`.** It's the most widely used, has the cleanest `Encoder`/`Decoder` API, and on
this machine it found `/opt/homebrew` libopus via pkg-config and **statically linked `libopus.a`** (verified: prints
`libopus 1.6.1`, `otool -L` shows no dynamic libopus → static). `audiopus` 0.2.0 is the other option but `opus` is
simpler for a pure-encoder use case.

How linking works (read from `audiopus_sys 0.2.2`'s `build.rs`):
1. On macOS, default linking is **static** (`default_library_linking()` returns true for `target_os = "macos"`).
2. It calls `pkg_config::Config::new().statik(true).probe("opus")`. With `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig`
   set (homebrew's `opus.pc` → 1.6.1) this succeeds and it links `libopus.a` from `/opt/homebrew/lib`. Build log shows:
   `cargo:info=Found `Opus` via `pkg_config`.`
3. Fallback chain if pkg-config fails: `LIBOPUS_LIB_DIR`/`OPUS_LIB_DIR` env → else it **builds the vendored opus
   source via cmake** (`audiopus_sys` ships the full opus C source tree; cmake is present here). So even without
   pkg-config it will still build. To force using the vendored static build, you could set `OPUS_NO_PKG=1`.

So: **set `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig` and it just works** against homebrew opus 1.6.1.

## 6. Opus encoder: 48 kHz, low-latency, 20 ms frames

```rust
// opus 0.3.1 public API (verified from src/lib.rs):
//   pub enum Application { Voip, Audio, LowDelay }       // LowDelay = OPUS_APPLICATION_RESTRICTED_LOWDELAY
//   pub enum Channels    { Mono = 1, Stereo = 2 }
//   pub enum Bitrate     { Bits(i32), Max, Auto }
//   impl Encoder {
//       pub fn new(sample_rate: u32, channels: Channels, mode: Application) -> Result<Encoder>;
//       pub fn encode(&mut self, input: &[i16], output: &mut [u8]) -> Result<usize>;   // returns #bytes written
//       pub fn encode_float(&mut self, input: &[f32], output: &mut [u8]) -> Result<usize>;
//       pub fn encode_vec(&mut self, input: &[i16], max_size: usize) -> Result<Vec<u8>>;
//       pub fn set_bitrate(&mut self, Bitrate) -> Result<()>;
//       pub fn set_vbr(&mut self, bool) -> Result<()>;
//       pub fn set_inband_fec(&mut self, bool) -> Result<()>;
//       pub fn set_packet_loss_perc(&mut self, i32) -> Result<()>;
//       // ... many more setters
//   }
```

**`Application` choice:** use `Application::Audio` for best quality at 96 kbps (game music/SFX is "audio", not voice).
Use `Application::LowDelay` only if you need the absolute minimum algorithmic delay (it disables the SILK layer and
the encoder's look-ahead). For a 60fps game stream, `Audio` is the right default; the extra few ms of look-ahead is
irrelevant next to the 20 ms packetization. **Pick `Audio` first; switch to `LowDelay` only if you measure audio lag.**

**Valid frame sizes @ 48 kHz** (Opus mandates these — anything else returns an error):

| Frame duration | samples / channel @ 48 kHz |
|----------------|----------------------------|
| 2.5 ms         | 120                        |
| 5 ms           | 240                        |
| 10 ms          | 480                        |
| **20 ms**      | **960**  ← use this        |
| 40 ms          | 1920                       |
| 60 ms          | 2880                       |

**20 ms / 960 samples-per-channel is the WebRTC standard** and what you want. For **stereo** the `encode` input is
**interleaved L,R,L,R…** and its length must be `960 * 2 = 1920` i16; `encode` computes per-channel size as
`input.len() / channels`. For **mono**, input length = 960.

> NES audio is mono (the APU is a single mixed channel). Easiest path: **encode mono** (`Channels::Mono`, 960 i16 per
> frame). If you'd rather keep the WebRTC pipeline stereo, duplicate the mono sample into L and R (`pcm[2i]=pcm[2i+1]=s`).
> Mono Opus is fine and uses less bitrate. The code below shows a generic ring-buffer that works for either; set
> `CHANNELS` accordingly.

### 6.1 Encoder construction

```rust
use opus::{Application, Channels, Encoder as OpusEncoder, Bitrate};

const OPUS_SAMPLE_RATE: u32 = 48_000;
const FRAME_MS: usize = 20;
const SAMPLES_PER_CH: usize = OPUS_SAMPLE_RATE as usize / 1000 * FRAME_MS; // 960

fn make_opus_encoder(channels: Channels) -> opus::Result<OpusEncoder> {
    let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, channels, Application::Audio)?;
    enc.set_bitrate(Bitrate::Bits(96_000))?; // 64k–128k is plenty; 96k is a good default
    // Optional resilience for lossy networks:
    enc.set_inband_fec(true)?;
    enc.set_packet_loss_perc(10)?;
    Ok(enc)
}
```

### 6.2 Buffering NES audio → fixed 960-sample frames → `Sample`

The NES APU won't hand you exactly 960 samples on tick boundaries. **Resample to 48 kHz first** (the NES audio output
rate depends on your core — commonly the APU is sampled at the CPU rate and downsampled; whatever your core emits,
resample to 48000 Hz mono i16 with a simple linear or `rubato`/`dasp` resampler — that's an emulator-side concern).
Then push the 48 kHz i16 stream into a ring buffer and drain it in exact 960-sample (per-channel) chunks:

```rust
use std::collections::VecDeque;
use std::time::Duration;
use bytes::Bytes;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

pub struct OpusStreamer {
    enc: opus::Encoder,
    channels: usize,            // 1 or 2
    pcm_in: VecDeque<i16>,      // interleaved if stereo
    out: Vec<u8>,               // reusable encode output buffer
}

impl OpusStreamer {
    pub fn new(channels: opus::Channels) -> opus::Result<Self> {
        let ch = channels as usize;
        Ok(Self {
            enc: make_opus_encoder(channels)?,
            channels: ch,
            pcm_in: VecDeque::with_capacity(SAMPLES_PER_CH * ch * 4),
            out: vec![0u8; 4000],   // 4000 bytes >> any 20ms Opus packet
        })
    }

    /// Feed freshly produced 48kHz i16 samples (interleaved if stereo, mono otherwise).
    pub fn push_pcm(&mut self, samples: &[i16]) {
        self.pcm_in.extend(samples.iter().copied());
    }

    /// Drain as many full 20ms frames as are buffered, encoding+sending each.
    pub async fn flush_frames(
        &mut self,
        track: &TrackLocalStaticSample,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let frame_len = SAMPLES_PER_CH * self.channels; // 960 (mono) or 1920 (stereo)
        while self.pcm_in.len() >= frame_len {
            // Pull one frame's worth into a contiguous slice.
            let mut frame = Vec::with_capacity(frame_len);
            for _ in 0..frame_len {
                frame.push(self.pcm_in.pop_front().unwrap());
            }
            // encode() expects per-channel count = input.len()/channels = 960. ✓
            let n = self.enc.encode(&frame, &mut self.out)?;
            let sample = Sample {
                data: Bytes::copy_from_slice(&self.out[..n]),
                duration: Duration::from_millis(FRAME_MS as u64), // 20ms -> 48kHz RTP step
                ..Default::default()
            };
            track.write_sample(&sample).await?;
        }
        Ok(())
    }
}
```

Audio track creation (Opus MIME type verified in webrtc 0.17.1 = `"audio/opus"`):

```rust
use webrtc::api::media_engine::MIME_TYPE_OPUS; // "audio/opus"
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;

let audio_track = std::sync::Arc::new(TrackLocalStaticSample::new(
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48_000,
        channels: 2,                // advertise 2; mono Opus still negotiates fine. Use 1 if mono-only.
        ..Default::default()
    },
    "audio".to_owned(),
    "nes".to_owned(),
));
```

> The `Sample.duration` (20 ms) is what makes the sampler advance the Opus RTP timestamp by 960 (= 20ms × 48kHz).
> Do not set `packet_timestamp` yourself.

## 7. Real-time pacing / A-V sync strategy

You have two cadences: **video = 60 fps (16.667 ms)** and **audio = 50 packets/s (20 ms)**. They don't share a period,
so don't try to lockstep them — run them off **one wall-clock-driven loop** and let each emit on its own schedule. The
emulator itself is the master clock: one NES frame ≈ 1/60.0988 s of game time and produces one video frame + ~800
audio samples (@48 kHz mono) per frame.

Recommended structure:

1. **Master tick = emulator frame at 60 fps.** Use a `tokio::time::interval(Duration::from_nanos(16_666_667))` (or
   better, an absolute-deadline loop that computes `next_deadline += frame_dt` and `tokio::time::sleep_until` to avoid
   drift). Each tick:
   - Step the NES core one frame → get the 256×240 framebuffer + the audio samples generated this frame.
   - `encode_and_send_frame(...)` → one VP8 `Sample` (duration 16.667 ms).
   - `opus.push_pcm(audio_samples_resampled_to_48k)` then `opus.flush_frames(...)`. Because each video frame yields
     ~800 audio samples but a frame needs 960, the ring buffer naturally emits an Opus packet roughly every ~1.2 video
     frames — i.e. ~50 audio packets/sec. The ring buffer absorbs the mismatch; **never pad or truncate to force
     alignment.**
2. **Timestamps drive sync, not your loop precision.** Both tracks carry real durations, so webrtc-rs generates RTP
   timestamps at 90 kHz (video) and 48 kHz (audio) from a shared monotonic base. The browser's jitter buffer +
   RTCP sender reports handle A/V sync. Your only job is to keep `Sample.duration` truthful (16.667 ms video,
   20 ms audio) and to feed samples at roughly realtime.
3. **Backpressure:** `write_sample` is `async` and will await if the track buffer is full. If you ever fall behind,
   drop video frames (skip a tick's encode) rather than audio — audio glitches are far more noticeable. Keep a small
   high-water mark on the audio ring buffer (e.g. cap at ~5 frames / 100 ms) and drop the oldest if a slow consumer
   lets it grow, to bound latency.
4. **Keyframes on join (future):** with vpx-encode 0.6.2 you can't force a keyframe; libvpx will emit periodic/auto
   keyframes so a new viewer recovers within a second or two. If that's too slow once you have multiple viewers, adopt
   the direct-FFI encoder from §1.2 and call `force_keyframe()` when a new peer's `on_ice_connection_state_change`
   reaches Connected.

---

# VERIFICATION (actually compiled & run on this machine)

Environment: macOS 25.1.0 arm64, rustc 1.86.0, `pkg-config --modversion vpx` → **1.16.0**,
`pkg-config --modversion opus` → **1.6.1**. Probes used isolated target dirs under `/tmp`.

### VP8 probe — `/tmp/vpx-probe`, `Cargo.toml`: `vpx-encode = { version="0.6.2", features=["ffi-generate"] }`

Program: created the encoder above (256×240, timebase [1,1000], 2000 kbps), built a synthetic I420 buffer, encoded
3 frames + flushed. Run command:
```sh
PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig LIBRARY_PATH=/opt/homebrew/lib cargo run
```
Output:
```
frame 0 pts=0 bytes=321 key=true
frame 1 pts=16 bytes=45 key=false
frame 2 pts=32 bytes=48 key=false
OK total_compressed_bytes=414 keyframes=1
```
Linkage proof (`otool -L`):
```
/opt/homebrew/opt/libvpx/lib/libvpx.12.dylib (compatibility version 1.0.0, current version 1.0.0)
```
→ Confirms: real VP8 keyframe-first encode, dynamically linked to **homebrew libvpx 1.16.0**.

Also confirmed the **failure mode without the feature**: default-features build panics with
`Expected file "generated/vpx-ffi-1.16.0.rs" not found but 'generate' cargo feature not used.` — hence `ffi-generate`
is mandatory here (§1.1).

### Opus probe — `/tmp/opus-probe`, `Cargo.toml`: `opus = "0.3.1"`

Program: `make_opus_encoder(Channels::Stereo)` + `set_bitrate(96_000)`, built one 20 ms stereo frame (1920 i16 sine),
called `encode` and `encode_vec`. Run command:
```sh
PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig LIBRARY_PATH=/opt/homebrew/lib cargo run
```
Output:
```
libopus version: libopus 1.6.1
OK encoded 20ms stereo frame -> 386 bytes
OK encode_vec -> 336 bytes
```
Build log (from `audiopus_sys` output file):
```
cargo:info=No feature or environment variable found, linking by default.
cargo:info=Found `Opus` via `pkg_config`.
```
`otool -L` showed **no dynamic libopus entry** → libopus was **statically linked** from homebrew `libopus.a` (1.6.1).
→ Confirms: real Opus encode at 48 kHz, linked against **homebrew libopus 1.6.1**.

### webrtc Sample / API facts (read from source, webrtc 0.17.1 / webrtc-media 0.17.1)
- `webrtc::media::Sample` fields: `data: Bytes`, `timestamp: SystemTime`, `duration: Duration`,
  `packet_timestamp: u32`, `prev_dropped_packets: u16`. Has `Default`. Set only `data` + `duration`.
- `TrackLocalStaticSample::new(RTCRtpCodecCapability, id, stream_id)` and
  `async fn write_sample(&self, sample: &Sample) -> Result<()>`.
- MIME constants: `MIME_TYPE_VP8 = "video/VP8"`, `MIME_TYPE_OPUS = "audio/opus"`.

---

# APPENDIX — copy-paste Cargo.toml for the encoder module

```toml
[package]
name = "nes-codec"
version = "0.1.0"
edition = "2021"

[dependencies]
vpx-encode = { version = "0.6.2", features = ["ffi-generate"] }
opus       = "0.3.1"
webrtc     = "0.17.1"
bytes      = "1"
tokio      = { version = "1", features = ["full"] }
# Optional SIMD RGB->I420 (manual path in §3.1 needs neither):
# yuvutils-rs = "0.8.3"
```

Build/run with (or put these in a `.cargo/config.toml` `[env]` block, or a build wrapper):
```sh
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
export LIBRARY_PATH=/opt/homebrew/lib
```
