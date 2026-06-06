# Game Boy / Game Boy Color via the libretro frontend — SameBoy (and gambatte) probe

**Date:** 2026-06-06 · **Platform:** macOS arm64 · **Toolchain:** Rust 1.92 (pinned)
**Goal:** run DMG (`Pokemon Red.gb`) and GBC (`Pokemon Red Color.gbc`) headless through the *same*
hand-rolled libretro frontend used for N64, capture framebuffer + audio + input facts, and prove
the `.gbc` renders in real color while the `.gb` is the classic 4-shade gray.

## TL;DR / recommendation

Both **SameBoy** and **gambatte** build clean on arm64, run both ROMs headless, never request HW
render, and produce **correct GBC color with ZERO core options** (pure auto-detect from header 0x143).

**Ship SameBoy** for accuracy + simplest video path, **but with one caveat to plan around:**

| | SameBoy 1.0.3 | gambatte v0.5.0 | mgba 0.11-dev |
|---|---|---|---|
| Pixel format | **XRGB8888** (fmt=1) | RGB565 (fmt=2) | RGB565 (fmt=2) |
| Framebuffer | 160x144 | 160x144 | 160x144 |
| Pitch (bytes) | **640** (= 160 px × 4B, tight) | **512** (= **256** px × 2B, padded!) | 512 (256 px × 2B, padded!) |
| Audio sample_rate | **2 097 152 Hz** (2^21, native APU) | **32 768 Hz** | 131 072 Hz |
| fps | 59.7275 | 59.7275 | 59.7275 |
| GBC color | correct (auto) | correct (auto) | correct (auto) |
| DMG | 4-shade grayscale | 4-shade grayscale | 4-shade grayscale |
| Core options needed | none | none | none |
| dylib size | 323 KB | 3.9 MB | 2.8 MB |

- **SameBoy wins on video**: it emits **XRGB8888 with the exact same in-memory byte order
  (`B, G, R, X`) the N64/angrylion path already produces**, and its pitch is tight (160 px). The
  existing `src/video.rs::xrgb_to_i420(src, sw, sh, pitch, dst, dw, dh)` works **unmodified** — just
  feed it `sw=160, sh=144, pitch=640`. No new converter needed.
- **SameBoy's catch is audio**: it reports a native APU rate of **2 097 152 Hz** and floods
  ~35 100 stereo i16 frames per video frame. There is **no core option to lower it** (the
  `sameboy_audio_output` option is just the DC-offset/high-pass filter, not a rate selector). Your
  Opus pipeline already has to resample to 48 kHz, so this is fine — but the resampler must handle a
  2.097 MHz → 48 kHz ratio (≈43.7:1 decimation) and the audio drain buffer must be sized for ~35 k
  stereo samples/frame, not a few hundred. **This is the one real gotcha.**
- **gambatte is the fallback** if the 2 MHz audio is undesirable: clean 32 768 Hz audio (trivial
  resample), tiny data rate. Cost: you must add an `rgb565_to_i420` path **and** honor the padded
  pitch (512 B = 256 px; the visible image is only the first 160 px of each row).

If audio-resampler simplicity matters more than reusing `xrgb_to_i420`, ship **gambatte**. Otherwise
**SameBoy** is the cleaner video integration and the most accurate emulator. Both dylibs are copied to
`cores/`.

---

## 1. Framebuffer (CRITICAL for the converter)

- **Dimensions:** 160 × 144 (native GB), confirmed by both `av_info.geometry.base_*` and every
  `video_refresh` call. `max` differs by core (SameBoy 256×224 to allow SGB borders; gambatte
  160×144).
- **SameBoy pixel format:** `RETRO_ENVIRONMENT_SET_PIXEL_FORMAT` → **1 = XRGB8888**.
  - In-memory byte layout per pixel (little-endian `0x00RRGGBB`): **`[B, G, R, X]`** — i.e.
    `byte0=Blue, byte1=Green, byte2=Red, byte3=unused`. **Identical to the N64/angrylion layout.**
  - Pitch = **640 bytes** = 160 px × 4 B (tight, no padding). `frame_bytes = pitch*height = 92 160`.
  - **Converter action: NONE.** `src/video.rs::xrgb_to_i420` already decodes `(R,G,B) = (src[o+2],
    src[o+1], src[o])`. Call it with `pitch=640`.
- **gambatte / mgba pixel format:** `SET_PIXEL_FORMAT` → **2 = RGB565**.
  - 2 bytes/px, little-endian `u16`: `R = bits 15..11 (5b)`, `G = bits 10..5 (6b)`, `B = bits 4..0
    (5b)`. Expand: `R8=(r<<3)|(r>>2)`, `G8=(g<<2)|(g>>4)`, `B8=(b<<3)|(b>>2)`.
  - **Pitch = 512 bytes = 256 px × 2 B — NOT 160×2=320.** The buffer is padded to 256 px wide; only
    the first **160** px of each row are the visible image. A converter MUST stride by `pitch` and
    only read `x < 160`. (My probe handles this correctly via `pitch`-strided row offsets.)
  - **Converter action:** add an `rgb565_to_i420(src, 160, 144, pitch=512, …)` path.
- Neither core ever called `SET_HW_RENDER` (HW_RENDER requested = **false**) — pure software, headless
  via `video_refresh` exactly like the task expected.
- Nobody requests `0RGB1555`.

## 2. Audio

- **i16 stereo, interleaved L,R**, delivered via `retro_set_audio_sample_batch` (confirmed
  `via batch_cb: true`). Channels = 2.
- `av_info.timing.sample_rate`:
  - **SameBoy: 2 097 152 Hz** (= 2^21, the GB APU master rate / not resampled). ~35 100 stereo
    frames per 1/59.73 s video frame. No option to change it.
  - **gambatte: 32 768 Hz** (= 2^15). ~549 stereo frames/video-frame.
  - **mgba: 131 072 Hz**.
- All need resampling to 48 kHz for Opus. SameBoy's ratio is the steep one.

## 3. fps

- All three: **`av_info.timing.fps` = 59.7275** (the canonical GB/GBC refresh). Use this as the VP8
  frame-pacing clock, not 60.

## 4. Input

- Device: **`RETRO_DEVICE_JOYPAD` (1)** on port 0 (`retro_set_controller_port_device(0, 1)`).
  **No analog** — `input_state` for `RETRO_DEVICE_ANALOG` can return 0 forever; GB has none.
- Button id mapping (RETRO_DEVICE_ID_JOYPAD_*) — suggested keyboard binding for the web frontend:

  | GB button | libretro id | const | suggested key |
  |---|---|---|---|
  | A | **8** | `JOYPAD_A` | X |
  | B | **0** | `JOYPAD_B` | Z |
  | Select | **2** | `JOYPAD_SELECT` | Right Shift / Backspace |
  | Start | **3** | `JOYPAD_START` | Enter |
  | Up | **4** | `JOYPAD_UP` | ArrowUp |
  | Down | **5** | `JOYPAD_DOWN` | ArrowDown |
  | Left | **6** | `JOYPAD_LEFT` | ArrowLeft |
  | Right | **7** | `JOYPAD_RIGHT` | ArrowRight |

  (Y=1, X=9, L=10, R=11 etc. are unused by GB.) Input plumbing verified — pressing Start/A changed
  the framebuffer checksum (see proof). `GET_INPUT_BITMASKS` is declined; cores fall back to
  per-id `input_state` polling, which works.

## 5. Core options

- **None required. The core auto-detects DMG vs GBC from header byte 0x143.** Verified: SameBoy's
  log prints `Initializing as model: cgbE/cgb` for the `.gbc` (0x143=0xC0) and renders color, and
  renders the `.gb` (0x143=0x00) in 4-shade gray. The probe forces **no** variables (declines every
  `GET_VARIABLE`) and both ROMs are correct.
- Optional tunables (not needed, but available if you want to tweak the look):
  - SameBoy: `sameboy_model` (default Auto), `sameboy_color_correction_mode`, `sameboy_mono_palette`
    (recolor DMG games), `sameboy_high_pass_filter_mode`, `sameboy_rumble`.
  - gambatte: `gambatte_gb_colorization` (default `auto` — colorizes DMG games on a GBC-style
    palette if you ever want the `.gb` in color too), `gambatte_gbc_color_correction` (**default
    ON** — warms/darkens to mimic the real GBC LCD; set off for a brighter web stream),
    `gambatte_gb_internal_palette`.
- The N64 harness's mupen-specific forced options are simply never queried by a GB core, so leaving
  the `forced_option()` table empty (return `None`) is correct. Keep the `GET_LOG_INTERFACE` shim —
  SameBoy *does* call the log callback (you can see its messages), so a NULL fn-ptr would crash; the
  shim stays silent unless `PROBE_VERBOSE`.

## GB display aspect ratio

- Native resolution 160×144 → pixel-count ratio **160:144 = 10:9 ≈ 1.1111** (this is exactly what
  both cores report as `geometry.aspect_ratio = 1.1111`). GB pixels are square, so the correct
  display AR is 10:9. On a TV / Super Game Boy it's commonly shown stretched to ~4:3. For the web
  stream, encode the VP8 at 160×144 (or an integer multiple, e.g. 480×432 = 3×) and let the
  browser present at 10:9 / 4:3 as desired.

---

## PROOF — real probe output

Probe: `research/gb-harness/` (copied from `research/n64-harness/`; only `forced_option()` emptied and
reporting extended with distinct-color counting + PPM dumps). Built with `RUSTUP_TOOLCHAIN=1.92 cargo
build --release`. Run: `gbprobe <core.dylib> "<rom>" <tag>` with `PROBE_WARM=N` warm-up frames.

### SameBoy — DMG `Pokemon Red.gb` (header 0x143 = 0x00)

```
library: SameBoy 1.0.3 8230189 | exts: gb|gbc | need_fullpath: false
[env] SET_PIXEL_FORMAT -> 1
retro_load_game -> true
av_info: geom base 160x144 max 256x224 aspect 1.1111 | fps 59.7275 sample_rate 2097152.0
HW_RENDER requested by core: false
FRAMEBUFFER after 600 frames: 160x144 pitch=640 (=160 px * 4B) fmt=1 (XRGB8888)
  bytes captured: 92160 | checksum(FNV1a)=0xc535f745aa7c6c87 | nonzero bytes: 37584 / 92160
  DISTINCT COLORS in frame: 4
AUDIO: 21006690 stereo frames seen | rate 2097152.0 Hz | channels 2 (i16) | via batch_cb: true
PERF: 600 frames in 0.564s => 1064.7 fps (av target 59.7275)
AFTER mashing START+A (+192 frames): checksum=0x07a76257cc3097cc | distinct_colors 4 | changed_vs_warm: true
AFTER +240 idle frames (title settled): checksum=0x0a21435cf362d4ff | distinct_colors 4 | changed_vs_B: true
```

### SameBoy — GBC `Pokemon Red Color.gbc` (header 0x143 = 0xC0)

```
library: SameBoy 1.0.3 8230189 | exts: gb|gbc | need_fullpath: false
[core log 1] Initializing as model: cgbE      <-- AUTO-DETECTED GBC from 0x143
[env] SET_PIXEL_FORMAT -> 1
retro_load_game -> true
av_info: geom base 160x144 max 256x224 aspect 1.1111 | fps 59.7275 sample_rate 2097152.0
HW_RENDER requested by core: false
FRAMEBUFFER after 600 frames: 160x144 pitch=640 (=160 px * 4B) fmt=1 (XRGB8888)
  bytes captured: 92160 | checksum(FNV1a)=0x3c39df3b37183fb3 | nonzero bytes: 37496 / 92160
  DISTINCT COLORS in frame: 10
AUDIO: 21015981 stereo frames seen | rate 2097152.0 Hz | channels 2 (i16) | via batch_cb: true
AFTER mashing START+A (+192 frames): checksum=0xdd8a5c221b69ddfd | distinct_colors 9 | changed_vs_warm: true
AFTER +240 idle frames (title settled): checksum=0x53aa85354d7ddf9b | distinct_colors 9 | changed_vs_B: true
```

### Color-variety proof (decoded PPM analysis)

Distinct decoded `(R,G,B)` values + count of **chromatic** pixels (R/G/B spread > 16, i.e. NOT gray):

```
DMG  warm   (.gb) : distinct_colors=4  chromatic_px=0/23040 (0%)
   top colors: (255,255,255), (170,170,170), (85,85,85), (0,0,0)   <-- pure 4-shade gray
GBC  warm   (.gbc): distinct_colors=10 chromatic_px=337/23040 (1%)
GBC  title  (.gbc): distinct_colors=9  chromatic_px=3820/23040 (16%)
   top colors: (255,255,255), (231,198,0 gold), (41,99,181 blue), (148,156,148),
               (0,0,0), (255,0,0 red), (255,140,0 orange), (173,0,33 dark red)
DMG  title  (.gb) : distinct_colors=4  chromatic_px=0/23040 (0%)
   top colors: (255,255,255), (170,170,170), (0,0,0), (85,85,85)   <-- still pure gray
```

**Conclusion:** the `.gb` is *exactly* the classic 4 grayscale shades (0% chromatic) at every point;
the `.gbc` title screen has 9 colors with 16% chromatic pixels — real gold/blue/red/orange. The
input also visibly changes the frame (checksums differ after Start/A). Rendered title screens
(scaled 3× nearest-neighbor) are in `research/gb-proof/`:
`sameboy_dmg_title.png` (grayscale), `sameboy_gbc_title.png` (full color),
`gambatte_gbc_450_title.png` (full color), `gambatte_dmg_warm.png` (grayscale).
Visual check: the GBC shots show a gold "Pokémon" logo with blue outline, red "Red Version" text and
a colored Red trainer sprite (gambatte even shows a blue Squirtle); the DMG shots are pure gray.

### gambatte — comparison runs

```
-- DMG (.gb) --
library: Gambatte v0.5.0-netlink 6716e6e | exts: gb|gbc|dmg | need_fullpath: false
[env] SET_PIXEL_FORMAT -> 2
av_info: geom base 160x144 max 160x144 aspect 1.1111 | fps 59.7275 sample_rate 32768.0
FRAMEBUFFER after 600 frames: 160x144 pitch=512 (=256 px * 2B) fmt=2 (RGB565)
  DISTINCT COLORS in frame: 4
AUDIO: 329431 stereo frames seen | rate 32768.0 Hz | channels 2 (i16) | via batch_cb: true

-- GBC (.gbc) --
[env] SET_PIXEL_FORMAT -> 2
av_info: geom base 160x144 max 160x144 aspect 1.1111 | fps 59.7275 sample_rate 32768.0
FRAMEBUFFER after 450 frames: 160x144 pitch=512 (=256 px * 2B) fmt=2 (RGB565)
  DISTINCT COLORS in frame: 10
AFTER +240 idle frames (title settled): distinct_colors 11   <-- full color, auto-detected
```

---

## Ready-to-integrate notes (drop-in for src/n64.rs-style module)

1. **Core load:** identical to `src/n64.rs`. dlopen `cores/sameboy_libretro.dylib`, resolve the same
   `retro_*` symbols, register the same six callbacks, `retro_init()`, build `retro_game_info` with
   the ROM bytes (need_fullpath=false, so passing `data`+`size` is enough; path is also passed).
2. **Pixel format (SameBoy):** accept fmt 1 in the `SET_PIXEL_FORMAT` env handler (already does).
   Feed `video::xrgb_to_i420(src, 160, 144, pitch /*640*/, dst, dw, dh)` — **unchanged**.
   - If you switch to gambatte/mgba: add `rgb565_to_i420(src, 160, 144, pitch /*512*/, …)` and
     remember to stride rows by 512 and read only `x<160`.
3. **Audio:** read `av.timing.sample_rate` and resample to 48 kHz. For SameBoy that's
   2 097 152→48 000; size the drain buffer for ≥36 000 stereo i16 per frame. For gambatte it's
   32 768→48 000 (≈549/frame).
4. **fps:** pace at 59.7275.
5. **Input:** port 0 = JOYPAD, button ids per the table above, no analog.
6. **Core options:** force none. Keep the `GET_LOG_INTERFACE` shim (SameBoy calls it).

## Artifacts on disk

- Working cores copied to: `~/pokemon-pvp-red/cores/sameboy_libretro.dylib`
  (primary) and `~/pokemon-pvp-red/cores/gambatte_libretro.dylib` (fallback).
- Probe harness source: `~/pokemon-pvp-red/research/gb-harness/`
  (`src/main.rs`, `Cargo.toml`, `build.rs`, `logshim.c`).
- Proof PNGs: `~/pokemon-pvp-red/research/gb-proof/`.
