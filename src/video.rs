//! RGBA -> I420 color conversion + realtime VP8 encoder (vpx-encode / system libvpx).
//!
//! Verified (research/codec.md): a 256x240 synthetic frame encodes to a ~321-byte
//! keyframe + ~45-byte interframes, dynamically linked to homebrew libvpx 1.16.0.

use vpx_encode::{Config, Encoder, VideoCodecId};

pub const W: usize = 256;
pub const H: usize = 240;
/// Packed I420 size for 256x240 = 61440 (Y) + 15360 (U) + 15360 (V) = 92160 bytes.
pub const I420_LEN: usize = W * H + 2 * ((W / 2) * (H / 2));

/// Realtime VP8 encoder for the NES output (256x240). timebase [1,1000] => pts is in ms.
/// `bitrate` is in KILOBITS/sec. `encode()` always uses VPX_DL_REALTIME internally.
///
/// A freshly-created encoder emits a keyframe on its first `encode()` — we exploit this
/// in pipeline.rs to force a clean keyframe whenever a new viewer connects.
pub fn make_vp8_encoder() -> vpx_encode::Result<Encoder> {
    Encoder::new(Config {
        width: W as u32,
        height: H as u32,
        timebase: [1, 1000], // 1/1000 s == ms; pts in ms
        bitrate: 2000,       // kbps (2 Mbps). 1000..=3000 is sane for 256x240.
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
