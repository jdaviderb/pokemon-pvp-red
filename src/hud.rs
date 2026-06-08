//! Hide the in-battle FIGHT/ITEM/PKMN/RUN menu. In PvP both sides' moves are injected, so the menu is
//! just noise — when it's up we paint the bottom box over with the game's OWN background color
//! (sampled from the frame so it blends) on the RGB565 framebuffer, before VP8 encode. The field stays
//! clean and the game's own battle text ("X used Y!", "it's super effective!") is untouched — we only
//! blank during the menu phase, never during the game's messages. Baked into the frame, so it also
//! applies to the TV wall and any other stream consumer.
//!
//! TOGGLE: set env `HIDE_BATTLE_MENU=0` to turn this OFF (then the real FIGHT/ITEM menu shows). The
//! call site (and the gate) is in `pipeline.rs::run_loop`.

#[inline]
fn put(frame: &mut [u8], pitch: usize, w: usize, h: usize, x: usize, y: usize, c: u16) {
    if x >= w || y >= h {
        return;
    }
    let off = y * pitch + x * 2;
    if off + 1 < frame.len() {
        frame[off] = (c & 0xff) as u8; // RGB565 little-endian
        frame[off + 1] = (c >> 8) as u8;
    }
}

fn fill(frame: &mut [u8], pitch: usize, w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: u16) {
    for y in y0..y1.min(h) {
        for x in x0..x1.min(w) {
            put(frame, pitch, w, h, x, y, c);
        }
    }
}

#[inline]
fn read(frame: &[u8], pitch: usize, x: usize, y: usize) -> u16 {
    let off = y * pitch + x * 2;
    if off + 1 < frame.len() {
        (frame[off] as u16) | ((frame[off + 1] as u16) << 8)
    } else {
        0xFFFF
    }
}

#[inline]
fn lum(c: u16) -> u32 {
    let r = ((c >> 11) & 0x1f) as u32;
    let g = ((c >> 5) & 0x3f) as u32;
    let b = (c & 0x1f) as u32;
    r * 2 + g + b * 2
}

/// Paint the bottom text-box region with the game's background color (the lightest of the four frame
/// corners = the battle field bg), so the FIGHT/ITEM menu vanishes cleanly. RGB565 frames only.
pub fn hide_battle_menu(frame: &mut [u8], pitch: usize, w: usize, h: usize) {
    if w < 80 || h < 60 {
        return;
    }
    let top = h.saturating_sub(48); // the bottom box + its border occupy ~the last rows
    let bg = [
        read(frame, pitch, 2, 2),
        read(frame, pitch, w - 3, 2),
        read(frame, pitch, 2, top.saturating_sub(2)),
        read(frame, pitch, w - 3, top.saturating_sub(2)),
    ]
    .into_iter()
    .max_by_key(|&c| lum(c))
    .unwrap_or(0xFFFF);
    fill(frame, pitch, w, h, 0, top, w, h, bg);
}
