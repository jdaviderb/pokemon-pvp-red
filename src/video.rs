//! XRGB8888 (BGRX) -> I420 + realtime VP8 encoder for the N64 output.
//!
//! angrylion delivers XRGB8888 (pitch-strided, memory byte order B,G,R,X). The encoder canvas
//! is sized from the first real frame (pipeline.rs); we copy/letterbox each live frame onto that
//! fixed canvas (VP8 cannot resize mid-stream).

use vpx_encode::{Config, Encoder, VideoCodecId};

/// Packed I420 byte length for a w x h canvas.
pub fn i420_len(w: usize, h: usize) -> usize {
    w * h + 2 * ((w / 2) * (h / 2))
}

/// Realtime VP8 encoder at the given (even) dimensions. timebase [1,1000] => pts in ms.
/// A fresh encoder emits a keyframe on its first encode() — pipeline.rs uses this for new viewers.
pub fn make_vp8_encoder(w: u32, h: u32) -> vpx_encode::Result<Encoder> {
    Encoder::new(Config {
        width: w,
        height: h,
        timebase: [1, 1000],
        bitrate: 3500, // kbps; 640x240@60 with SSB64 motion wants more than NES. 2500..=5000 sane.
        codec: VideoCodecId::VP8,
    })
}

/// XRGB8888 source (memory bytes B,G,R,X), dims `sw x sh`, row stride `pitch`, ->
/// packed I420 on a fixed `dw x dh` canvas (BT.601 limited range). Centers/clips when the source
/// differs from the canvas. `dst.len()` must be >= i420_len(dw, dh); reuse it across frames.
pub fn xrgb_to_i420(src: &[u8], sw: usize, sh: usize, pitch: usize, dst: &mut [u8], dw: usize, dh: usize) {
    let y_size = dw * dh;
    let c_w = dw / 2;
    let c_h = dh / 2;
    let (y_plane, uv) = dst.split_at_mut(y_size);
    let (u_plane, v_plane) = uv.split_at_mut(c_w * c_h);

    // Black canvas baseline (Y=16, U=V=128), then paint the active region.
    y_plane.fill(16);
    u_plane.fill(128);
    v_plane.fill(128);

    // Center the source on the canvas; clip if larger.
    let ox = dw.saturating_sub(sw) / 2;
    let oy = dh.saturating_sub(sh) / 2;
    let cw = sw.min(dw.saturating_sub(ox));
    let ch = sh.min(dh.saturating_sub(oy));

    for j in 0..ch {
        let row = j * pitch;
        let dy = oy + j;
        for i in 0..cw {
            let p = row + i * 4; // B,G,R,X
            if p + 2 >= src.len() {
                continue;
            }
            let b = src[p] as i32;
            let g = src[p + 1] as i32;
            let r = src[p + 2] as i32;
            let dx = ox + i;
            let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
            y_plane[dy * dw + dx] = (y + 16) as u8;
            if (dy & 1) == 0 && (dx & 1) == 0 {
                let u = (-38 * r - 74 * g + 112 * b + 128) >> 8;
                let v = (112 * r - 94 * g - 18 * b + 128) >> 8;
                let ci = (dy / 2) * c_w + (dx / 2);
                u_plane[ci] = (u + 128) as u8;
                v_plane[ci] = (v + 128) as u8;
            }
        }
    }
}
