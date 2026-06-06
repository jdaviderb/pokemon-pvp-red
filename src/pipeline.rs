//! The master clock: one dedicated OS thread runs the NES core at real-time 60 fps,
//! encodes each frame to VP8 + Opus, and fans the encoded media out over broadcast
//! channels to every connected peer. Browser input arrives on `input_rx`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use crate::audio::OpusStreamer;
use crate::emu::{map_button, Emu};
use crate::video::{make_vp8_encoder, rgba_to_i420, H, I420_LEN, W};

/// NTSC NES frame period: 1e9 / 60.098814 Hz ≈ 16_639_267 ns.
pub const NTSC_FRAME_NANOS: u64 = 16_639_267;

#[derive(Clone)]
pub struct EncodedVideo {
    pub data: Bytes,
}

#[derive(Clone)]
pub struct EncodedAudio {
    pub data: Bytes,
    pub samples: u32,
}

/// Input event from the browser data channel: {"type":"down"|"up","button":"A"}.
#[derive(serde::Deserialize)]
pub struct InputEvent {
    #[serde(rename = "type")]
    pub kind: String, // "down" | "up"
    pub button: String, // "A" "B" "Up" "Down" "Left" "Right" "Start" "Select"
}

/// Shared state behind AppState (see signaling.rs).
pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>,
    pub audio_tx: broadcast::Sender<EncodedAudio>,
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
    /// Set true to make the emulator reset the VP8 encoder next frame -> a fresh
    /// keyframe, so a newly-connected viewer gets a clean picture immediately.
    pub keyframe_req: Arc<AtomicBool>,
}

/// Build channels + spawn the emulator thread. Returns the AppInner to share.
pub fn start(rom_bytes: Vec<u8>, rom_name: String) -> Arc<AppInner> {
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(16);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(32);
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let keyframe_req = Arc::new(AtomicBool::new(false));

    let v = video_tx.clone();
    let a = audio_tx.clone();
    let kf = keyframe_req.clone();
    // Dedicated OS thread (NOT tokio): steady 60 fps, encoders never cross threads.
    std::thread::spawn(move || {
        if let Err(e) = run_loop(rom_bytes, rom_name, v, a, input_rx, kf) {
            tracing::error!("emulator loop ended: {e:?}");
        }
    });

    Arc::new(AppInner {
        video_tx,
        audio_tx,
        input_tx,
        keyframe_req,
    })
}

fn run_loop(
    rom_bytes: Vec<u8>,
    rom_name: String,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    keyframe_req: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut emu = Emu::new(&rom_bytes, &rom_name)?;
    let mut vpx = make_vp8_encoder().map_err(|e| anyhow::anyhow!("vpx init: {e:?}"))?;
    let mut opus = OpusStreamer::new().map_err(|e| anyhow::anyhow!("opus init: {e:?}"))?;

    let mut i420 = vec![0u8; I420_LEN]; // reused scratch
    let frame_period = Duration::from_nanos(NTSC_FRAME_NANOS);
    let mut next = Instant::now();
    let mut frame_idx: u64 = 0;

    // Lightweight throughput stats (logged ~every 5 s) so the pipeline is observable.
    let mut stat_t = Instant::now();
    let mut stat_frames: u64 = 0;
    let mut stat_vpkts: u64 = 0;
    let mut stat_apkts: u64 = 0;

    loop {
        // 1. Apply all pending input to player one (sticky until released).
        while let Ok(ev) = input_rx.try_recv() {
            if let Some(btn) = map_button(&ev.button) {
                tracing::info!("input P1: {} {}", ev.kind, ev.button);
                emu.set_button(btn, ev.kind == "down");
            }
        }

        // 2. A viewer just connected -> reset the encoder so the next frame is a keyframe.
        if keyframe_req.swap(false, Ordering::Relaxed) {
            match make_vp8_encoder() {
                Ok(e) => {
                    vpx = e;
                    tracing::info!("vp8 encoder reset -> forcing keyframe for new viewer");
                }
                Err(e) => tracing::warn!("vp8 reset failed: {e:?}"),
            }
        }

        // 3. Advance one frame.
        emu.clock_frame()?;

        // 4. VIDEO: RGBA -> I420 -> VP8 -> broadcast (copy out of the encoder's buffer).
        rgba_to_i420(emu.frame_buffer(), W, H, &mut i420);
        let pts_ms = (frame_idx * 1000 / 60) as i64; // ms pts for timebase [1,1000]
        match vpx.encode(pts_ms, &i420) {
            Ok(packets) => {
                for frame in packets {
                    stat_vpkts += 1;
                    let _ = video_tx.send(EncodedVideo {
                        data: Bytes::copy_from_slice(frame.data),
                    });
                }
            }
            Err(e) => tracing::warn!("vpx encode: {e:?}"),
        }

        // 5. AUDIO: f32 -> i16 ring -> 960-sample Opus packets -> broadcast.
        opus.push_f32(emu.audio_samples());
        emu.clear_audio_samples(); // REQUIRED every frame
        match opus.take_packets() {
            Ok(pkts) => {
                for p in pkts {
                    stat_apkts += 1;
                    let _ = audio_tx.send(EncodedAudio {
                        data: Bytes::from(p.data),
                        samples: p.samples,
                    });
                }
            }
            Err(e) => tracing::warn!("opus encode: {e:?}"),
        }

        // 6. Periodic throughput stats.
        stat_frames += 1;
        if stat_t.elapsed() >= Duration::from_secs(5) {
            let secs = stat_t.elapsed().as_secs_f64();
            tracing::info!(
                "emu: {:.1} fps | {} video pkts | {} audio pkts (last {:.0}s) | viewers v={} a={}",
                stat_frames as f64 / secs,
                stat_vpkts,
                stat_apkts,
                secs,
                video_tx.receiver_count(),
                audio_tx.receiver_count(),
            );
            stat_t = Instant::now();
            stat_frames = 0;
            stat_vpkts = 0;
            stat_apkts = 0;
        }

        // 7. Drift-compensated pacing to the next 16.639 ms deadline.
        frame_idx += 1;
        next += frame_period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            // Fell behind (encode took too long); resync the clock to avoid a burst.
            next = now;
        }
    }
}
