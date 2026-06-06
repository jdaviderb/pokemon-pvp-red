//! WebRTC peer plumbing: build the shared API once; per browser offer, build a
//! RTCPeerConnection, attach VP8+Opus tracks fed from the broadcast channels, drain
//! RTCP, wire the input data channel, run non-trickle offer/answer signaling, and
//! keep the connection alive.
//!
//! NOTE: this module is named `webrtc`, which shadows the `webrtc` crate inside
//! `crate::`. Refer to the crate with a leading `::webrtc::...` everywhere below.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;

use ::webrtc::api::interceptor_registry::register_default_interceptors;
use ::webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS, MIME_TYPE_VP8};
use ::webrtc::api::{APIBuilder, API};
use ::webrtc::data_channel::data_channel_message::DataChannelMessage;
use ::webrtc::data_channel::RTCDataChannel;
use ::webrtc::interceptor::registry::Registry;
use ::webrtc::media::Sample;
use ::webrtc::peer_connection::configuration::RTCConfiguration;
use ::webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use ::webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use ::webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use ::webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use ::webrtc::track::track_local::TrackLocal;

use crate::pipeline::{AppInner, NTSC_FRAME_NANOS};

/// Build the shared API once (MediaEngine + default codecs (VP8/Opus) + interceptors).
pub fn build_api() -> anyhow::Result<Arc<API>> {
    let mut m = MediaEngine::default();
    m.register_default_codecs()?; // registers VP8 (video/VP8) + Opus (audio/opus)
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?; // NACK, reports, TWCC
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();
    Ok(Arc::new(api))
}

/// Build a peer for one browser offer; return the gathered answer SDP.
pub async fn build_peer_and_answer(
    api: &Arc<API>,
    inner: &Arc<AppInner>,
    offer_sdp: String,
) -> anyhow::Result<String> {
    // Localhost only: no STUN needed; host candidates on 127.0.0.1 connect instantly.
    let config = RTCConfiguration::default();
    let pc = Arc::new(api.new_peer_connection(config).await?);

    // Per-peer liveness flag: cleared when the connection ends so this peer's writer
    // tasks stop (write_sample does NOT error on a dead track, so we gate on this).
    let alive = Arc::new(AtomicBool::new(true));

    // --- VIDEO track (VP8). Stream id "nes" groups A+V into one MediaStream. ---
    let video_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_VP8.to_owned(),
            ..Default::default()
        },
        "video".to_owned(),
        "nes".to_owned(),
    ));
    let video_sender = pc
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    // MANDATORY: drain RTCP or NACK/report interceptors stall.
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while video_sender.read(&mut buf).await.is_ok() {}
    });

    // --- AUDIO track (Opus) ---
    let audio_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            ..Default::default()
        },
        "audio".to_owned(),
        "nes".to_owned(),
    ));
    let audio_sender = pc
        .add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while audio_sender.read(&mut buf).await.is_ok() {}
    });

    // --- WRITER TASKS: subscribe to the broadcast, write_sample into this peer's tracks.
    //     The Arc<TrackLocalStaticSample> clones moved here keep the tracks (and thus the
    //     session media flow) alive after this function returns. They exit when the peer
    //     drops (write_sample errors) or the emulator stops (channel closed). ---
    {
        let mut vrx = inner.video_tx.subscribe();
        let vtrack = Arc::clone(&video_track);
        let valive = Arc::clone(&alive);
        let video_dur = Duration::from_nanos(NTSC_FRAME_NANOS); // ~16.639 ms -> 90kHz RTP step
        tokio::spawn(async move {
            loop {
                match vrx.recv().await {
                    Ok(f) => {
                        if !valive.load(Ordering::Relaxed) {
                            break; // peer ended
                        }
                        let sample = Sample {
                            data: f.data,
                            duration: video_dur,
                            ..Default::default()
                        };
                        if vtrack.write_sample(&sample).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("video writer lagged {n} frames");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });

        let mut arx = inner.audio_tx.subscribe();
        let atrack = Arc::clone(&audio_track);
        let aalive = Arc::clone(&alive);
        tokio::spawn(async move {
            loop {
                match arx.recv().await {
                    Ok(p) => {
                        if !aalive.load(Ordering::Relaxed) {
                            break;
                        }
                        // honest duration: samples * 1000 / 48000 ms (960 -> 20 ms)
                        let dur = Duration::from_millis((p.samples as u64 * 1000) / 48_000);
                        let sample = Sample {
                            data: p.data,
                            duration: dur,
                            ..Default::default()
                        };
                        if atrack.write_sample(&sample).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
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
                            if let Ok(ev) =
                                serde_json::from_slice::<crate::pipeline::InputEvent>(&msg.data)
                            {
                                let _ = input_tx.send(ev);
                            }
                        })
                    }));
                }
            })
        }));
    }

    // --- Keep the connection alive past this function; force a keyframe on connect. ---
    {
        let pc_hold = Arc::clone(&pc);
        let keyframe_req = inner.keyframe_req.clone();
        let alive = Arc::clone(&alive);
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            tracing::info!("peer state: {s:?}");
            match s {
                // New viewer is live -> ask the emulator for a fresh keyframe.
                RTCPeerConnectionState::Connected => keyframe_req.store(true, Ordering::Relaxed),
                // Terminal/abandoned -> stop this peer's writer tasks (no ICE restart here).
                RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Closed => alive.store(false, Ordering::Relaxed),
                _ => {}
            }
            let _ = &pc_hold; // hold an Arc<pc> inside the long-lived handler closure
            Box::pin(async {})
        }));
    }

    // --- Signaling: offer -> answer -> gather ICE fully (no trickle) -> return SDP. ---
    let offer = RTCSessionDescription::offer(offer_sdp)?;
    pc.set_remote_description(offer).await?;
    let answer = pc.create_answer(None).await?;
    let mut gather_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(answer).await?; // starts UDP listeners + ICE gathering
    let _ = gather_complete.recv().await; // BLOCK until all candidates gathered

    let local = pc
        .local_description()
        .await
        .ok_or_else(|| anyhow::anyhow!("no local description"))?;
    Ok(local.sdp)
}
