//! XRGB8888 (BGRX) -> I420 + realtime VP8 encoder for the N64 output.
//!
//! angrylion delivers XRGB8888 (pitch-strided, memory byte order B,G,R,X). The encoder canvas
//! is sized from the first real frame (pipeline.rs); we copy/letterbox each live frame onto that
//! fixed canvas (VP8 cannot resize mid-stream).

use vpx_encode::{Config, Encoder, VideoCodecId};

// libretro pixel-format ids (mirror of src/n64.rs)
pub const PIXFMT_XRGB8888: u32 = 1; // 4-byte BGRX in memory (N64/angrylion, SameBoy)
pub const PIXFMT_RGB565: u32 = 2; // 2-byte little-endian u16 (gambatte / mGBA Game Boy)

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

/// Format-aware entry point: decode a libretro framebuffer of pixel format `fmt`
/// (XRGB8888 or RGB565) into packed I420 on a fixed `dw x dh` canvas. The pipeline calls THIS
/// and passes `Frame.fmt` straight through, so cores of either format work unchanged.
pub fn frame_to_i420(
    src: &[u8],
    sw: usize,
    sh: usize,
    pitch: usize,
    fmt: u32,
    dst: &mut [u8],
    dw: usize,
    dh: usize,
) {
    match fmt {
        PIXFMT_RGB565 => rgb565_to_i420(src, sw, sh, pitch, dst, dw, dh),
        // XRGB8888 (1) and anything else fall back to the 4-byte BGRX path (N64/angrylion, SameBoy).
        _ => xrgb_to_i420(src, sw, sh, pitch, dst, dw, dh),
    }
}

/// RGB565 source (little-endian u16 per pixel: RRRRR GGGGGG BBBBB), dims `sw x sh`, row stride
/// `pitch` bytes -> packed I420 on a fixed `dw x dh` canvas (BT.601 limited range). gambatte uses
/// this with sw=160, sh=144, pitch=512 (256-px padded; we read only i < sw, honoring pitch).
pub fn rgb565_to_i420(src: &[u8], sw: usize, sh: usize, pitch: usize, dst: &mut [u8], dw: usize, dh: usize) {
    let y_size = dw * dh;
    let c_w = dw / 2;
    let c_h = dh / 2;
    let (y_plane, uv) = dst.split_at_mut(y_size);
    let (u_plane, v_plane) = uv.split_at_mut(c_w * c_h);

    y_plane.fill(16);
    u_plane.fill(128);
    v_plane.fill(128);

    let ox = dw.saturating_sub(sw) / 2;
    let oy = dh.saturating_sub(sh) / 2;
    let cw = sw.min(dw.saturating_sub(ox));
    let ch = sh.min(dh.saturating_sub(oy));

    for j in 0..ch {
        let row = j * pitch; // HONOR pitch (e.g. 512), not sw*2
        let dy = oy + j;
        for i in 0..cw {
            let p = row + i * 2;
            if p + 1 >= src.len() {
                continue;
            }
            let v = (src[p] as u16) | ((src[p + 1] as u16) << 8); // little-endian
            let r5 = ((v >> 11) & 0x1f) as i32;
            let g6 = ((v >> 5) & 0x3f) as i32;
            let b5 = (v & 0x1f) as i32;
            let r = (r5 << 3) | (r5 >> 2); // 5->8 bit
            let g = (g6 << 2) | (g6 >> 4); // 6->8 bit
            let b = (b5 << 3) | (b5 >> 2); // 5->8 bit
            let dx = ox + i;
            let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
            y_plane[dy * dw + dx] = (y + 16) as u8;
            if (dy & 1) == 0 && (dx & 1) == 0 {
                let u = (-38 * r - 74 * g + 112 * b + 128) >> 8;
                let v2 = (112 * r - 94 * g - 18 * b + 128) >> 8;
                let ci = (dy / 2) * c_w + (dx / 2);
                u_plane[ci] = (u + 128) as u8;
                v_plane[ci] = (v2 + 128) as u8;
            }
        }
    }
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
