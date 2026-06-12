//! WebRTC peer plumbing: build the shared API once; per browser offer, build a
//! RTCPeerConnection, attach VP8+Opus tracks fed from the broadcast channels, drain
//! RTCP, wire the input data channel, run non-trickle offer/answer signaling, and
//! keep the connection alive.
//!
//! NOTE: this module is named `webrtc`, which shadows the `webrtc` crate inside
//! `crate::`. Refer to the crate with a leading `::webrtc::...` everywhere below.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use ::webrtc::ice_transport::ice_server::RTCIceServer;

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

use crate::pipeline::AppInner;

/// Video Sample duration (~1/60 s). Emu runs ~60 fps; this drives the 90 kHz RTP step.
/// Exact A/V sync is handled by the browser jitter buffer + RTCP sender reports.
const VIDEO_FRAME_NANOS: u64 = 16_666_667;

/// Live PeerConnections (players + spectators), capped by SPECTATOR_MAX (unbounded if unset).
static PEER_COUNT: AtomicUsize = AtomicUsize::new(0);

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
    // Admission control: refuse new peers past SPECTATOR_MAX (unbounded if unset).
    let cap = std::env::var("SPECTATOR_MAX").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(usize::MAX);
    if PEER_COUNT.load(Ordering::Relaxed) >= cap {
        anyhow::bail!("peer limit reached");
    }
    // Localhost: host candidates on 127.0.0.1 connect instantly (no STUN). For public-internet peers
    // behind NAT, set STUN_URLS (comma-separated; add a TURN entry for symmetric NAT).
    let mut config = RTCConfiguration::default();
    if let Ok(urls) = std::env::var("STUN_URLS") {
        let list: Vec<String> =
            urls.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if !list.is_empty() {
            config.ice_servers = vec![RTCIceServer { urls: list, ..Default::default() }];
        }
    }
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
        "game".to_owned(),
    ));
    let video_sender = pc
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    // MANDATORY: drain RTCP or NACK/report interceptors stall.
    tokio::spawn(async move {
        let mut buf = vec![0u8; 1500];
        while video_sender.read(&mut buf).await.is_ok() {}
    });

    // --- AUDIO track (Opus, STEREO) ---
    let audio_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1".to_owned(),
            ..Default::default()
        },
        "audio".to_owned(),
        "game".to_owned(),
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
        let vkf = inner.keyframe_req.clone(); // ask for a keyframe after a lag so the decoder recovers
        let video_dur = Duration::from_nanos(VIDEO_FRAME_NANOS); // ~16.67 ms -> 90kHz RTP step
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
                        vkf.store(true, Ordering::Relaxed); // recover this viewer with a fresh keyframe
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
        let input_gate = inner.input_gate.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let input_tx = input_tx.clone();
            let input_gate = input_gate.clone();
            Box::pin(async move {
                if dc.label() == "input" {
                    let input_tx = input_tx.clone();
                    let input_gate = input_gate.clone();
                    dc.on_message(Box::new(move |msg: DataChannelMessage| {
                        let input_tx = input_tx.clone();
                        let input_gate = input_gate.clone();
                        Box::pin(async move {
                            if let Ok(ev) =
                                serde_json::from_slice::<crate::pipeline::InputEvent>(&msg.data)
                            {
                                // Fight mode mutes players while the server runs the random-pick
                                // char-select bootstrap; server-injected input bypasses this.
                                if input_gate.load(Ordering::Relaxed) {
                                    let _ = input_tx.send(ev);
                                }
                            }
                        })
                    }));
                }
            })
        }));
    }

    // --- Lifecycle: keep the PC alive past this fn WITHOUT a self-referential Arc, and CLOSE it on
    //     disconnect / connect-timeout so PeerConnections don't leak (each freed peer drops its
    //     tracks, ICE/UDP, and RTCP-drain tasks). PEER_COUNT tracks live peers for admission. ---
    PEER_COUNT.fetch_add(1, Ordering::Relaxed);
    {
        let close = Arc::new(tokio::sync::Notify::new());
        let connected = Arc::new(AtomicBool::new(false));
        // Guardian: holds the only strong Arc<pc> past this fn; closes + drops it (and decrements
        // PEER_COUNT) when signalled. The state handler holds NO strong pc -> no Arc cycle.
        {
            let pc_guard = Arc::clone(&pc);
            let close = Arc::clone(&close);
            tokio::spawn(async move {
                close.notified().await;
                let _ = pc_guard.close().await;
                PEER_COUNT.fetch_sub(1, Ordering::Relaxed);
            });
        }
        // Connect deadline: if it never reaches Connected within 30s, give up + close.
        {
            let close = Arc::clone(&close);
            let connected = Arc::clone(&connected);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if !connected.load(Ordering::Relaxed) {
                    close.notify_one();
                }
            });
        }
        let keyframe_req = inner.keyframe_req.clone();
        let alive = Arc::clone(&alive);
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            tracing::info!("peer state: {s:?}");
            match s {
                // New viewer is live -> ask the emulator for a fresh keyframe.
                RTCPeerConnectionState::Connected => {
                    connected.store(true, Ordering::Relaxed);
                    keyframe_req.store(true, Ordering::Relaxed);
                }
                // Terminal/abandoned -> stop this peer's writer tasks + close the PC (frees it).
                RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Closed => {
                    alive.store(false, Ordering::Relaxed);
                    close.notify_one();
                }
                _ => {}
            }
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
