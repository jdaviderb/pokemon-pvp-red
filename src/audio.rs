//! i16 stereo @ core rate (e.g. 44100) -> linear resample to 48000 -> STEREO Opus 960-frame packets.

use opus::{Application, Bitrate, Channels, Encoder as OpusEncoder};

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
pub const OPUS_FRAME: usize = 960; // 20 ms @ 48 kHz, PER CHANNEL. Stereo packet = 960*2 i16.
pub const CHANNELS: usize = 2;

pub struct OpusPacket {
    pub data: Vec<u8>,
    pub samples: u32, // per-channel sample count (always OPUS_FRAME = 960)
}

pub struct OpusStreamer {
    enc: OpusEncoder,
    in_rate: f64,
    pos: f64, // fractional read position carried across calls
    prev_l: i16,
    prev_r: i16,
    have_prev: bool,
    pcm: Vec<i16>, // resampled, interleaved L,R awaiting packetization
    out: Vec<u8>,
}

impl OpusStreamer {
    pub fn new(in_rate: f64) -> opus::Result<Self> {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, Channels::Stereo, Application::Audio)?;
        enc.set_bitrate(Bitrate::Bits(128_000))?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        Ok(Self {
            enc,
            in_rate,
            pos: 0.0,
            prev_l: 0,
            prev_r: 0,
            have_prev: false,
            pcm: Vec::with_capacity(8192),
            out: vec![0u8; 4000],
        })
    }

    /// Feed interleaved L,R i16 @ in_rate; linear-resample to 48000 and buffer.
    pub fn push_i16_stereo(&mut self, input: &[i16]) {
        if input.len() < 2 {
            return;
        }
        let n = input.len() / 2; // input frames
        let step = self.in_rate / OPUS_SAMPLE_RATE as f64; // input frames per output frame (<1 upsampling)
        let mut p = self.pos;
        while p < n as f64 {
            let i0 = p.floor() as usize;
            let frac = p - i0 as f64;
            let (l0, r0, l1, r1);
            if i0 == 0 && self.have_prev {
                l0 = self.prev_l as f64;
                r0 = self.prev_r as f64;
                l1 = input[0] as f64;
                r1 = input[1] as f64;
            } else {
                let a = i0.min(n - 1);
                let b = (i0 + 1).min(n - 1);
                l0 = input[a * 2] as f64;
                r0 = input[a * 2 + 1] as f64;
                l1 = input[b * 2] as f64;
                r1 = input[b * 2 + 1] as f64;
            }
            let l = l0 + (l1 - l0) * frac;
            let r = r0 + (r1 - r0) * frac;
            self.pcm.push(l.round().clamp(-32768.0, 32767.0) as i16);
            self.pcm.push(r.round().clamp(-32768.0, 32767.0) as i16);
            p += step;
        }
        self.pos = p - n as f64; // carry leftover into next call's coordinate frame
        self.prev_l = input[(n - 1) * 2];
        self.prev_r = input[(n - 1) * 2 + 1];
        self.have_prev = true;
    }

    /// Drain every full stereo 960-frame chunk, encoding each into an Opus packet.
    pub fn take_packets(&mut self) -> opus::Result<Vec<OpusPacket>> {
        let mut packets = Vec::new();
        let chunk = OPUS_FRAME * CHANNELS; // 1920 interleaved i16
        while self.pcm.len() >= chunk {
            let frame: Vec<i16> = self.pcm.drain(..chunk).collect();
            let n = self.enc.encode(&frame, &mut self.out)?;
            packets.push(OpusPacket {
                data: self.out[..n].to_vec(),
                samples: OPUS_FRAME as u32,
            });
        }
        Ok(packets)
    }
}
