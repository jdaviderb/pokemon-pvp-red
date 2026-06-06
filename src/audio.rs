//! f32 -> i16 buffering + Opus encoder. Turns the per-frame ~799 mono f32 samples
//! into legal 960-sample (20 ms @ 48 kHz) Opus packets (~50/s).
//!
//! Verified (research/codec.md): opus 0.3.1 statically links homebrew libopus 1.6.1.

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
    pcm: Vec<i16>, // mono i16 ring (drain-from-front)
    out: Vec<u8>,  // reusable encode output
}

impl OpusStreamer {
    pub fn new() -> opus::Result<Self> {
        // NES audio is MONO. Application::Audio = best quality for game music/SFX.
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, Channels::Mono, Application::Audio)?;
        enc.set_bitrate(Bitrate::Bits(96_000))?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        Ok(Self {
            enc,
            pcm: Vec::with_capacity(4096),
            out: vec![0u8; 4000],
        })
    }

    /// Feed this frame's mono f32 samples (48 kHz). Clamps + scales to i16.
    pub fn push_f32(&mut self, samples: &[f32]) {
        self.pcm
            .extend(samples.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
    }

    /// Drain every full 960-sample frame, encoding each. Returns packets to broadcast.
    pub fn take_packets(&mut self) -> opus::Result<Vec<OpusPacket>> {
        let mut packets = Vec::new();
        while self.pcm.len() >= OPUS_FRAME {
            let frame: Vec<i16> = self.pcm.drain(..OPUS_FRAME).collect();
            let n = self.enc.encode(&frame, &mut self.out)?;
            packets.push(OpusPacket {
                data: self.out[..n].to_vec(),
                samples: OPUS_FRAME as u32,
            });
        }
        Ok(packets)
    }
}
