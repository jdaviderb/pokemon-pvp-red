# Game Boy / Game Boy Color via gambatte (libretro) — VERIFIED headless probe

**Date:** 2026-06-06
**Core:** `gambatte_libretro.dylib` (Gambatte v0.5.0-netlink, build `6716e6e`), arm64 nightly
from `https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/gambatte_libretro.dylib.zip`
**Result:** WORKS. Both ROMs run headless through the exact same hand-rolled libretro frontend used by
`src/n64.rs`. DMG renders classic 4-shade grayscale; the `.gbc` renders in **real color** (auto-detected from
the 0x143 header). Working dylib copied to `~/pokemon-pvp-red/cores/gambatte_libretro.dylib`.

> The only frontend difference vs N64: gambatte requests **RGB565** (not XRGB8888), so `src/video.rs`
> needs an RGB565 -> I420 path (the N64 converter only handles XRGB8888). Ready code below.

---

## 1. Framebuffer (CRITICAL for the converter)

| Property | Value |
|---|---|
| width x height | **160 x 144** (constant; base == max, never resizes) |
| pitch (row stride) | **512 bytes** |
| pixel format requested via `RETRO_ENVIRONMENT_SET_PIXEL_FORMAT` | **2 = RETRO_PIXEL_FORMAT_RGB565** |
| bytes per pixel | 2 |
| captured bytes per frame | `pitch * height` = `512 * 144` = **73728** |
| aspect ratio reported | 1.1111 (10:9) |
| HW render (`SET_HW_RENDER`) | **never requested** — pure software, headless via `video_refresh` works trivially |

**pitch is 512, NOT 320.** 160 px * 2 bytes = 320 bytes of real data per row, but gambatte pads the stride to
512 bytes (256 u16 slots). **You MUST honor the `pitch` argument** from `video_refresh` and index rows as
`y * pitch`, never `y * width * 2`. The trailing 192 bytes/row are padding — ignore them.

### Exact RGB565 byte layout (little-endian u16 per pixel)
Each pixel is a 16-bit little-endian value. In memory bytes `[lo, hi]`, reassemble `v = lo | (hi << 8)`, then:
```
v bits:  RRRRR GGGGGG BBBBB   (MSB..LSB)
R = (v >> 11) & 0x1F   (5 bits)
G = (v >>  5) & 0x3F   (6 bits)   <- green has 6 bits
B = (v      ) & 0x1F   (5 bits)
```
Expand to 8-bit: `r8 = (r<<3)|(r>>2)`, `g8 = (g<<2)|(g>>4)`, `b8 = (b<<3)|(b>>2)`.

This differs from the N64/angrylion path which is XRGB8888 = 4 bytes/pixel in memory order `B,G,R,X`.

---

## 2. Audio

| Property | Value |
|---|---|
| sample_rate (`av_info.timing.sample_rate`) | **32768.0 Hz** |
| channels | **2 (stereo)** |
| sample type | **i16** interleaved L,R |
| delivery | `audio_sample_batch(*const i16, frames)` — ~549 stereo frames per video frame (32768/59.7) |

Measured ~109,719 stereo frames over 200 video frames (matches 32768 Hz). Resample 32768 -> 48000 before the
Opus encoder (Opus wants 48k; the N64 path already does some resampling — reuse it, just change the input rate
from the N64 core's rate to 32768).

## 3. Frame rate (fps)

`av_info.timing.fps` = **59.7275** (the canonical Game Boy ~59.73 Hz). Confirmed.

## 4. Input

- Device: standard **`RETRO_DEVICE_JOYPAD` (1)** on port 0. Set once via `retro_set_controller_port_device(0, 1)`.
- **No analog. No bitmasks needed** (we returned `false` to `GET_INPUT_BITMASKS` and the core polls per-button fine).
- Button id mapping (these are the libretro ids the core queries via `input_state`):

  | GB button | RETRO_DEVICE_ID_JOYPAD | id |
  |---|---|---|
  | B | `_B` | 0 |
  | Select | `_SELECT` | 2 |
  | Start | `_START` | 3 |
  | Up | `_UP` | 4 |
  | Down | `_DOWN` | 5 |
  | Left | `_LEFT` | 6 |
  | Right | `_RIGHT` | 7 |
  | A | `_A` | 8 |

  (Y=1, X=9, L/R/L2/R2 exist in the device but the GB has no such buttons; the core simply never acts on them.)

Input plumbing proven: pressing **Start** changed the frame checksum, then pressing **A** changed it again
(see proof output — `changed_vs_before: true`, `changed_vs_B: true`).

## 5. Core options needed

**NONE. gambatte auto-detects DMG vs GBC from the ROM header byte 0x143.** Our probe returns `false` for every
`GET_VARIABLE` (option "not set") and both ROMs run correctly — `.gb` (0x143=0x00) in grayscale, `.gbc`
(0x143=0xC0) in color. No DMG/GBC forcing, no color-correction option required for color to appear.

Optional polish later (all default off, NOT required):
- `gambatte_gb_colorization` — only affects DMG games (can fake-colorize a mono game); leave default. The probe
  supports forcing it via the `PROBE_GB_COLORIZATION` env var for experimentation.
- `gambatte_gbc_color_correction` (`GBC only` / `always` / `disabled`) — a subtle palette tweak for GBC's
  display gamma; default is fine. Not needed for color to render.

---

## PROOF — real probe output (both ROMs, ~200 frames + long run)

The probe (built with `rustup run 1.92 cargo build --release`) prints dims+pitch+pixfmt, an FNV-1a checksum,
non-zero byte count, and **distinct-color count**. It then runs ~1800 more frames tapping Start/A to advance
past the boot logos into the colorful intro, tracking the peak color variety, and dumps a PPM at the peak.

### DMG — `Pokemon Red.gb` (header 0x143 = 0x00)
```
library: Gambatte v0.5.0-netlink 6716e6e | exts: gb|gbc|dmg | need_fullpath: false
rom size = 1048576 bytes | header CGB-flag(0x143) = 0x00
[env] SET_PIXEL_FORMAT -> 2
retro_load_game -> true
av_info: geom base 160x144 max 160x144 aspect 1.1111 | fps 59.7275 sample_rate 32768.0
HW_RENDER requested by core: false
FRAMEBUFFER after 200 frames: 160x144 pitch=512 fmt=2 (RGB565)
  bytes captured: 73728 | checksum(FNV1a)=0x7a71233907851601 | nonzero bytes: 44328 / 73728
  DISTINCT COLORS in frame: 3
AUDIO: 109719 stereo frames seen | rate 32768.0 Hz | channels 2 (i16)
PERF: 200 frames in 0.018s => 10822.6 fps (target 59.7275)
AFTER pressing START (+90 frames): checksum=0x49c51b2a60d12725 | nonzero 25600 | distinct_colors 2 | changed_vs_before: true
AFTER pressing A (+60 frames): checksum=0x7ed031fd5ffb30bb | distinct_colors 4 | changed_vs_B: true
LONG RUN: 1800 more frames, tapping Start/A, tracking peak color variety
PEAK DISTINCT COLORS over long run: 4
  peak frame top colors (RGB888 -> count):
    #ffffff x12300
    #000000 x10580
    #adaaad x104
    #525552 x56
```
=> 4 distinct colors, ALL grayscale (white, black, two grays). Classic ~4-shade Game Boy. **0 chromatic pixels.**

### GBC — `Pokemon Red Color.gbc` (header 0x143 = 0xC0)
```
rom size = 1048576 bytes | header CGB-flag(0x143) = 0xc0
[env] SET_PIXEL_FORMAT -> 2
retro_load_game -> true
av_info: geom base 160x144 max 160x144 aspect 1.1111 | fps 59.7275 sample_rate 32768.0
HW_RENDER requested by core: false
FRAMEBUFFER after 200 frames: 160x144 pitch=512 fmt=2 (RGB565)
  bytes captured: 73728 | checksum(FNV1a)=0x6368d4a966e77ae1 | nonzero bytes: 44328 / 73728
  DISTINCT COLORS in frame: 3
AUDIO: 109997 stereo frames seen | rate 32768.0 Hz | channels 2 (i16)
PERF: 200 frames in 0.011s => 17826.5 fps (target 59.7275)
AFTER pressing START (+90 frames): checksum=0x4b454987e6532725 | nonzero 25600 | distinct_colors 2 | changed_vs_before: true
AFTER pressing A (+60 frames): checksum=0x5a73a055a7bb65f1 | distinct_colors 4 | changed_vs_B: true
LONG RUN: 1800 more frames, tapping Start/A, tracking peak color variety
PEAK DISTINCT COLORS over long run: 9
  peak frame top colors (RGB888 -> count):
    #fffbff x17058
    #e7c300 x1655   <- GOLD/YELLOW
    #2961b5 x1601   <- BLUE
    #000000 x837
    #949a94 x699
    #ff8a00 x512    <- ORANGE
    #ff0000 x503    <- RED
    #ad0021 x138    <- DARK RED
    #393839 x37
```
=> genuine chromatic colors: gold `#e7c300`, blue `#2961b5`, orange `#ff8a00`, red `#ff0000`, dark-red `#ad0021`.
This is the colored Pokémon Red title screen.

### Independent corroboration (Python re-read of the dumped PPMs)
```
peak_dmg: 160x144 distinct_colors=4 chromatic_pixels=0/23040       <- pure grayscale
peak_gbc: 160x144 distinct_colors=9 chromatic_pixels=4409/23040    <- 19% of pixels are colored
frame_gbc:160x144 distinct_colors=4 chromatic_pixels=1004/23040
```
"chromatic" = pixels whose `max(r,g,b) - min(r,g,b) > 24` (i.e. not a gray). DMG: **zero**. GBC: **4409**.

### Visual confirmation (PPM -> PNG via `sips`)
- `/tmp/gbprobe/peak_gbc.png` (5203 bytes): the full-color Pokémon Red title — cyan/blue "Pokémon" logo with
  yellow outline, red "Red Version", orange Charmander, red-clad trainer. **CONFIRMED COLOR.**
- `/tmp/gbprobe/peak_dmg.png` (1245 bytes): monochrome GAMEFREAK logo, grayscale only.
- (Also `/tmp/gbprobe/frame_dmg.png`, `/tmp/gbprobe/frame_gbc.png` for the 320-frame snapshots.)

> Note on the distinct-color count: GBC games legitimately use small per-scene palettes (a few BG + sprite
> palettes), so a title screen showing ~9 colors is normal and correct — the decisive proof is the **chromatic
> pixel count** (4409 vs 0) and the visible hues, not a raw color total.

### Checksum changes on input (input plumbing proof)
- DMG: warm `0x7a71233907851601` -> after Start `0x49c51b2a60d12725` -> after A `0x7ed031fd5ffb30bb` (all distinct).
- GBC: warm `0x6368d4a966e77ae1` -> after Start `0x4b454987e6532725` -> after A `0x5a73a055a7bb65f1` (all distinct).

---

## Integration notes for the main app (`src/n64.rs` + `src/video.rs`)

The libretro frontend in `src/n64.rs` is reusable as-is for gambatte with these adjustments:

1. **Pixel format.** gambatte requests RGB565 (fmt 2). `src/n64.rs` already *accepts* RGB565 in its
   `SET_PIXEL_FORMAT` handler (`f == PIXFMT_RGB565`), so loading succeeds — but its `fmt` field defaults to
   XRGB8888 and the converter assumes XRGB8888. Wire the captured `pixfmt` through to the converter and add an
   RGB565 branch (below). For GB, `fmt` will be 2.

2. **pitch = 512, not width*2.** Always use the `pitch` from `video_refresh`. The N64 code already passes pitch
   through; just don't assume 4 bytes/pixel.

3. **Canvas size.** First real frame is 160x144. VP8 can't resize mid-stream, so size the encoder canvas to
   160x144 (or a fixed scaled canvas and letterbox, exactly like the N64 path).

4. **Audio rate.** 32768 Hz stereo i16 -> resample to 48000 for Opus (the N64 resampler just needs its input
   rate set to 32768).

5. **Core options.** None required. The forced-option table can be empty for GB (return `None` for all keys so
   gambatte uses defaults and auto-detects DMG/GBC).

### RGB565 -> I420 converter to add to `src/video.rs`
Mirror of the existing `xrgb_to_i420` but reading 2-byte little-endian RGB565 pixels:

```rust
/// RGB565 source (little-endian u16 per pixel: RRRRRGGGGGGBBBBB), dims `sw x sh`,
/// row stride `pitch` bytes, -> packed I420 on a fixed `dw x dh` canvas (BT.601 limited range).
/// gambatte (Game Boy / GBC) uses this format with sw=160, sh=144, pitch=512.
pub fn rgb565_to_i420(
    src: &[u8], sw: usize, sh: usize, pitch: usize,
    dst: &mut [u8], dw: usize, dh: usize,
) {
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
        let row = j * pitch;
        let dy = oy + j;
        for i in 0..cw {
            let p = row + i * 2; // 2 bytes per pixel
            if p + 1 >= src.len() {
                continue;
            }
            let v = (src[p] as u16) | ((src[p + 1] as u16) << 8); // little-endian
            let r5 = ((v >> 11) & 0x1f) as i32;
            let g6 = ((v >> 5) & 0x3f) as i32;
            let b5 = (v & 0x1f) as i32;
            // expand to 8-bit
            let r = ((r5 << 3) | (r5 >> 2)) as i32;
            let g = ((g6 << 2) | (g6 >> 4)) as i32;
            let b = ((b5 << 3) | (b5 >> 2)) as i32;
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
```
Then in the pipeline dispatch on the captured `pixfmt`: `1 => xrgb_to_i420(...)`, `2 => rgb565_to_i420(...)`.

---

## Reproduce

```sh
# 1. fetch core (arm64)
mkdir -p /tmp/gbprobe && cd /tmp/gbprobe
curl -sL -o gambatte.zip "https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/gambatte_libretro.dylib.zip"
unzip -o gambatte.zip

# 2. probe source lives at /tmp/gbprobe/gbprobe (copied from research/n64-harness, GB-adapted)
cd /tmp/gbprobe/gbprobe
rustup run 1.92 cargo build --release

# 3. run both ROMs (arg3 = tag used for PPM filenames)
./target/release/gbprobe /tmp/gbprobe/gambatte_libretro.dylib "~/pokemon-pvp-red/Pokemon Red.gb" dmg
./target/release/gbprobe /tmp/gbprobe/gambatte_libretro.dylib "~/pokemon-pvp-red/Pokemon Red Color.gbc" gbc

# 4. view (PPM -> PNG)
sips -s format png /tmp/gbprobe/peak_gbc.ppm --out /tmp/gbprobe/peak_gbc.png
```

The probe is a near-verbatim copy of `research/n64-harness` (same C ABI, callbacks, logshim). Differences:
forced-option table is empty (GB needs no forcing), frame count raised to 200, and it computes distinct-color +
chromatic-pixel metrics and a color histogram to prove DMG-vs-GBC color. The mupen-specific forced options were
removed; the `GET_LOG_INTERFACE` shim is kept (harmless, gambatte doesn't crash without it but it's safe).

## Artifacts
- Working core (copied into project): `~/pokemon-pvp-red/cores/gambatte_libretro.dylib`
  (sha256 `3bc49f04d573f5bf5c4f8c17874b0cab0bd0657422366379e6662e345d6490b1`)
- Probe project: `/tmp/gbprobe/gbprobe/` (Cargo.toml, src/main.rs, build.rs, logshim.c)
- Frame dumps: `/tmp/gbprobe/{frame,peak}_{dmg,gbc}.ppm` and matching `.png`
