# DESIGN-GB — the libretro frontend running Game Boy / Game Boy Color

**Date:** 2026-06-06 · **Platform:** macOS arm64 · **Toolchain:** Rust 1.92 (pinned, see `rust-toolchain.toml`)

This document is the concrete, copy-pasteable plan for the libretro frontend
(`src/libretro.rs` + `src/video.rs` + `src/pipeline.rs`) running GB/GBC, defaulting to **Pokémon Red**.
(Other kept docs are listed in DOCS.md.)

---

## 0. Decision: WINNER = gambatte

| | **gambatte (CHOSEN)** | SameBoy | mgba |
|---|---|---|---|
| Builds on arm64 | yes (verified) | yes | yes |
| GBC color (auto from header 0x143) | correct | correct | correct |
| Pixel format | RGB565 (fmt=2) | XRGB8888 (fmt=1) | RGB565 (fmt=2) |
| Pitch | 512 B (256 px padded) | 640 B (160 px tight) | 512 B |
| **Audio sample_rate** | **32 768 Hz** (clean ~549 frames/vframe) | **2 097 152 Hz** (~35 100 frames/vframe!) | 131 072 Hz |
| Core options needed | **none** | none | none |
| dylib size | 3.9 MB | 323 KB | 2.8 MB |

**Why gambatte, not SameBoy** (despite SameBoy reusing `xrgb_to_i420` unmodified):

SameBoy's only integration cost is video-free, *but* its native APU rate is **2 097 152 Hz**, with
**no core option to lower it**. Our `OpusStreamer` (`src/audio.rs`) is a 2-tap linear interpolator —
for each 48 kHz output sample it reads exactly two adjacent input samples. At a 43.7:1 *decimation*
ratio that throws away ~42 of every 43.7 samples with no anti-alias averaging (audible aliasing), and
it allocates a fresh `Vec<i16>` of ~35 k samples every video frame. gambatte's 32 768 Hz is an
*upsample* to 48 kHz (~1.46:1), which is exactly the regime the existing resampler was written for. The
video cost of gambatte (one RGB565 branch + honoring pitch) is small, fully written below, and verified
by the probe. **gambatte is the simplest correct integration overall.** SameBoy stays in `cores/` as a
drop-in fallback if we later add a proper polyphase decimator.

**Confidence: HIGH.** Both probes ran both ROMs headless through this exact frontend; the GBC `.gbc`
rendered genuine chromatic pixels (gold/blue/red/orange title screen), the `.gb` stayed pure 4-shade
gray, input changed the framebuffer checksum, and the converter code below is the probe's verified code.

---

## 1. Final core + exact dylib path

- **Core:** `gambatte_libretro.dylib` — Gambatte v0.5.0-netlink, build `6716e6e`, arm64 nightly from
  `https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/gambatte_libretro.dylib.zip`.
- **Path (already copied into the project):**
  `cores/gambatte_libretro.dylib`
  - sha256 `3bc49f04d573f5bf5c4f8c17874b0cab0bd0657422366379e6662e345d6490b1` (verified on disk)
  - `file` reports `Mach-O 64-bit dynamically linked shared library arm64` (verified)
- Extensions: `gb|gbc|dmg`. `need_fullpath = false` → passing ROM `data`+`size` is enough (the frontend
  already `mem::forget`s the ROM buffer when `!need_fullpath`, keeping it alive — unchanged).
- No symbol/ABI changes: gambatte exports the same `retro_*` symbols the frontend already resolves.

The frontend struct is named `Emu`; it loads any libretro core.

---

## 2. PIXEL FORMAT handling (RGB565 in addition to XRGB8888)

The frontend already records the SET_PIXEL_FORMAT value into `FRAME.fmt` (`src/libretro.rs` line 199-203)
and already *accepts* RGB565 (returns `true` for fmt 0/1/2). The `Frame` struct already carries
`pub fmt: u32`. **What's missing:** the pipeline calls `xrgb_to_i420` unconditionally, which assumes
4-byte XRGB. We add an RGB565 decoder and branch on `f.fmt`.

gambatte facts the converter MUST honor (verified):
- `fmt = 2` (RGB565), `sw=160, sh=144`, **`pitch = 512` bytes** (= 256 px × 2 B, padded; only the
  first 160 px of each row are real). Stride rows by `pitch`, read only `i < 160`. Never assume
  `pitch == width*2`.
- Little-endian u16 per pixel: `R = bits 15..11 (5b)`, `G = bits 10..5 (6b)`, `B = bits 4..0 (5b)`.

### 2a. `src/video.rs` — full updated converter (drop-in)

Add `rgb565_to_i420` next to the existing `xrgb_to_i420`, plus a thin `frame_to_i420` dispatcher that
branches on `fmt`. Keep `xrgb_to_i420` as-is (XRGB8888 cores still use it via the dispatcher).

```rust
// ---- pixel format ids (mirror of src/libretro.rs) ----
pub const PIXFMT_XRGB8888: u32 = 1;
pub const PIXFMT_RGB565: u32 = 2;

/// Format-aware entry point: decode a libretro framebuffer of pixel format `fmt`
/// (XRGB8888 or RGB565) into packed I420 on a fixed `dw x dh` canvas. The pipeline
/// calls THIS and passes `f.fmt` straight through; no per-core branching upstream.
pub fn frame_to_i420(
    src: &[u8], sw: usize, sh: usize, pitch: usize, fmt: u32,
    dst: &mut [u8], dw: usize, dh: usize,
) {
    match fmt {
        PIXFMT_RGB565 => rgb565_to_i420(src, sw, sh, pitch, dst, dw, dh),
        // XRGB8888 (1) and anything else (e.g. 0RGB1555 — no core here requests it) fall back
        // to the 4-byte BGRX path (what SameBoy emits).
        _ => xrgb_to_i420(src, sw, sh, pitch, dst, dw, dh),
    }
}

/// RGB565 source (little-endian u16 per pixel: RRRRRGGGGGGBBBBB), dims `sw x sh`,
/// row stride `pitch` bytes -> packed I420 on a fixed `dw x dh` canvas (BT.601 limited range).
/// gambatte (Game Boy / GBC) uses this with sw=160, sh=144, pitch=512 (256-px padded; we read x<160).
pub fn rgb565_to_i420(
    src: &[u8], sw: usize, sh: usize, pitch: usize,
    dst: &mut [u8], dw: usize, dh: usize,
) {
    let y_size = dw * dh;
    let c_w = dw / 2;
    let c_h = dh / 2;
    let (y_plane, uv) = dst.split_at_mut(y_size);
    let (u_plane, v_plane) = uv.split_at_mut(c_w * c_h);

    // Black canvas baseline (Y=16, U=V=128), then paint the active region (letterbox/center).
    y_plane.fill(16);
    u_plane.fill(128);
    v_plane.fill(128);

    let ox = dw.saturating_sub(sw) / 2;
    let oy = dh.saturating_sub(sh) / 2;
    let cw = sw.min(dw.saturating_sub(ox));
    let ch = sh.min(dh.saturating_sub(oy));

    for j in 0..ch {
        let row = j * pitch;          // HONOR pitch (512), not sw*2
        let dy = oy + j;
        for i in 0..cw {              // cw <= 160, so we never read the padding
            let p = row + i * 2;      // 2 bytes per pixel
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

### 2b. Thread `f.fmt` into the pipeline (`src/pipeline.rs`)

`Frame.fmt` already exists, so the only change is the call site. Update the import and the step-4 call.

Import (line 14) — replace `xrgb_to_i420` with `frame_to_i420`:
```rust
use crate::video::{frame_to_i420, i420_len, make_vp8_encoder};
```

Step 4 video block (currently lines 186-190) — pass `f.fmt`:
```rust
        // 4. VIDEO: latest framebuffer (fmt-aware) -> I420 -> VP8 -> broadcast.
        emu.with_frame(|f| {
            if !f.bytes.is_empty() {
                frame_to_i420(
                    &f.bytes, f.w as usize, f.h as usize, f.pitch, f.fmt,
                    &mut i420, cw as usize, ch as usize,
                );
            }
        });
```

That's the entire video change. XRGB8888 cores are unaffected (their `f.fmt == 1` → falls through to
`xrgb_to_i420`).

---

## 3. Dimensions — 160×144 "just works"

Confirmed: gambatte's `geometry.base = 160×144` and **never resizes** (base == max == 160×144 for
gambatte; `video_refresh` reports 160×144 every frame).

The pipeline's warm-up loop sizes the VP8 canvas from the first real frame
(`emu.with_frame(|f| (f.w, f.h))`, lines 104-114), forcing even dims via `(d & !1).max(2)`. 160 and 144
are already even, so the canvas becomes **160×144** with no change. The mid-stream resolution-change
re-init (lines 174-183) simply never fires for GB (dims are constant). **No pipeline dimension changes
needed.**

VP8 at 160×144 is fine — well above libvpx's minimum, and the bitrate in `make_vp8_encoder` (3500 kbps)
is far more than 160×144 needs, so quality is effectively lossless for this tiny frame. You *may* lower
the bitrate for GB to save bandwidth, but it is not required and 3500 does no harm.

**Optional server-side integer upscale (NOT worth it):** the CRT CSS already stretches the video to the
4:3 `.screen` box (`object-fit: fill`), so the browser upscales for free on the GPU. Doing a 2×/3×
nearest-neighbor upscale server-side (e.g. 480×432) would only *triple* the encoder/network cost to feed
the same browser stretch, and would bake in a scaling choice. **Skip it.** Encode native 160×144 and let
CSS present it. (If a sharper look is ever wanted, prefer `image-rendering: pixelated` on `#video` in CSS
over server-side scaling.)

---

## 4. Input — GB keymap for `static/index.html`

GB has **no analog stick**. Use the **digital** wire names `"Up"/"Down"/"Left"/"Right"` (the existing
`map_button` already maps these to `Btn(ID_UP..ID_RIGHT)`, lines 508-511) — **not** the `Stick*` names
(those fold into the analog control stick, which gambatte ignores). `A`, `B`, `Start`, `Select` are
already handled by `map_button`. `map_button` already returns `Some(Btn(ID_SELECT))` for `"Select"`
(line in the table) — **verify it's present; if not, add `"Select" => Btn(ID_SELECT),`**.

### 4a. `src/libretro.rs` — ensure `Select` is mapped

If the current `map_button` (lines 496-514) does **not** list `"Select"`, add it:
```rust
        "Start" => Btn(ID_START),
        "Select" => Btn(ID_SELECT),
```
(`ID_SELECT = 2` is already declared, line 68.)

### 4b. `static/index.html` — new KEYMAP block (lines 235-253)

```javascript
    // Physical-key (e.code) -> { player, wire button }. Game Boy has no analog:
    // the D-pad uses the DIGITAL Up/Down/Left/Right names (map_button -> ID_UP..ID_RIGHT),
    // NOT the Stick* names.
    const KEYMAP = {
      // D-pad on the arrows (DIGITAL)
      ArrowUp:    { p: 1, b: "Up" },
      ArrowDown:  { p: 1, b: "Down" },
      ArrowLeft:  { p: 1, b: "Left" },
      ArrowRight: { p: 1, b: "Right" },
      // face buttons + start/select
      KeyX:      { p: 1, b: "A" },       // A
      KeyZ:      { p: 1, b: "B" },       // B
      Enter:     { p: 1, b: "Start" },   // Start
      ShiftRight:{ p: 1, b: "Select" },  // Select (right Shift)
      Backspace: { p: 1, b: "Select" },  // Select (alias)
    };
```

### 4c. On-screen hint (replace lines 226-230)

```html
  <p class="hint">
    <b>D-pad</b> <kbd>←↑↓→</kbd> · <b>A</b> <kbd>X</kbd> · <b>B</b> <kbd>Z</kbd> ·
    <b>Start</b> <kbd>Enter</kbd> · <b>Select</b> <kbd>⇧</kbd>/<kbd>⌫</kbd>
  </p>
```

The keydown/keyup handlers, `pressed` Set de-dupe, and `sendInput` JSON wire format are all
button-name-agnostic and need **no change** — they already forward `{type, button, player}` and the
server's `map_button` does the rest.

---

## 5. Core options

**None required.** gambatte auto-detects DMG vs GBC from ROM header byte **0x143** (verified on disk:
`Pokemon Red.gb` = `0x00` → DMG/grayscale; `Pokemon Red Color.gbc` = `0xC0` → GBC/color). The frontend's
`forced_option` returns `None` for every gambatte key, so the core uses its own defaults — and both ROMs
render correctly. **No DMG/GBC option, no color-correction option needed for color to appear.**

`forced_option` in `src/libretro.rs` can be left exactly as-is: every key gambatte queries falls to the
`_ => return None` arm. Nothing to change for a correct GB stream.

**Optional polish (all default-off, NOT needed):** if you ever want to tune the GBC look, add a gambatte
arm to `forced_option`:
```rust
    // optional GBC look tuning (NOT required for correct color):
    "gambatte_gbc_color_correction" => "disabled".into(), // brighter web stream (default warms/darkens for real-LCD gamma)
    // "gambatte_gb_colorization" => "auto".into(),       // would fake-colorize DMG (.gb) games; leave default
```
Recommendation: leave `forced_option` untouched and ship defaults. The probe got correct color with zero
forcing.

---

## 6. Defaults + running the `.gbc` + IPS reproduce

### 6a. `src/main.rs` defaults (replace lines 20-22)

```rust
const DEFAULT_ROM: &str = "Pokemon Red.gb";
const DEFAULT_CORE: &str =
    "cores/gambatte_libretro.dylib";
```

`main.rs` already reads `argv[1]=ROM`, `argv[2]=core` with these as fallbacks (lines 33-34) — no
arg-parsing change needed.

### 6b. Running the color `.gbc` via argv

```sh
# default (grayscale DMG):
cargo run --release

# color GBC (header 0x143 = 0xC0) via argv[1]; core defaults to gambatte:
cargo run --release -- "Pokemon Red Color.gbc"

# explicit core too:
cargo run --release -- \
  "Pokemon Red Color.gbc" \
  "cores/gambatte_libretro.dylib"
```

### 6c. Recreating `Pokemon Red Color.gbc` from the IPS patch

The colorized `.gbc` is `Pokemon Red.gb` with `pokered_color/pokered_color_vanilla.ips` applied
(already present at `pokered_color/pokered_color_vanilla.ips`;
the patched `.gbc` is already on disk). To (re)create it deterministically, here is a self-contained IPS
applier. IPS format: magic `PATCH`, then records `[3B big-endian offset][2B big-endian size]` followed by
`size` bytes of data; **if `size == 0` it is an RLE run: `[2B big-endian run length][1B value]`**;
terminated by the 3-byte literal `EOF`.

Save as `scripts/apply_ips.py` and run it:

```python
#!/usr/bin/env python3
# apply_ips.py BASE.gb PATCH.ips OUT.gbc  — minimal IPS (with RLE) applier.
import sys

def apply_ips(base_path, ips_path, out_path):
    with open(base_path, "rb") as f:
        rom = bytearray(f.read())
    with open(ips_path, "rb") as f:
        p = f.read()
    assert p[:5] == b"PATCH", "not an IPS file (missing PATCH magic)"
    i = 5
    while True:
        if p[i:i+3] == b"EOF":
            break
        off = int.from_bytes(p[i:i+3], "big"); i += 3
        size = int.from_bytes(p[i:i+2], "big"); i += 2
        if size == 0:  # RLE run
            rlen = int.from_bytes(p[i:i+2], "big"); i += 2
            val = p[i]; i += 1
            if off + rlen > len(rom):
                rom.extend(b"\x00" * (off + rlen - len(rom)))
            rom[off:off+rlen] = bytes([val]) * rlen
        else:          # literal copy
            data = p[i:i+size]; i += size
            if off + size > len(rom):
                rom.extend(b"\x00" * (off + size - len(rom)))
            rom[off:off+size] = data
    with open(out_path, "wb") as f:
        f.write(rom)
    print(f"wrote {out_path} ({len(rom)} bytes); header 0x143 = 0x{rom[0x143]:02x}")

if __name__ == "__main__":
    apply_ips(sys.argv[1], sys.argv[2], sys.argv[3])
```

```sh
python3 scripts/apply_ips.py \
  "Pokemon Red.gb" \
  "pokered_color/pokered_color_vanilla.ips" \
  "Pokemon Red Color.gbc"
# expect: ... header 0x143 = 0xc0   (proves GBC mode -> color)
```

The `0x143 = 0xc0` print is the proof the patch took (matches the on-disk `.gbc` header verified above).

---

## 7. Audio — confirmed, no change

gambatte emits **i16 stereo interleaved L,R @ 32 768 Hz** via `audio_sample_batch` (verified
~109 k–329 k stereo frames over the probe runs; ~549 frames/video-frame). The pipeline already constructs
the resampler from the *core-reported* rate:

```rust
let mut opus = OpusStreamer::new(emu.sample_rate)  // emu.sample_rate = av.timing.sample_rate = 32768.0 for gambatte
```

`OpusStreamer` resamples arbitrary `in_rate → 48 000` with a fractional read position carried across
calls. 32 768 → 48 000 is a gentle ~1.46:1 upsample (`step = 32768/48000 ≈ 0.683` input frames per output
frame), squarely in the regime the resampler was written for. The `cb_audio_batch` callback already
`extend_from_slice`s the interleaved i16 into `AUDIO`, and `audio_drain()` hands it to `push_i16_stereo`.
**No audio code changes.** (`cb_audio_sample`, the per-sample fallback, also works but gambatte uses the
batch path.)

---

## 8. Branding (`static/index.html`)

Pokémon Red branding. Replace the caption (line 225):
```html
  <p class="caption"><b>Pokémon Red</b> · 100% server-side emulation, streamed over WebRTC</p>
```

Optional cosmetic polish (not required to function):
- `<title>` (line 6): `RETRO·VISION — Game Boy over WebRTC`
- Brand glyph (line 214): `RETRO·VISION <span>GB</span>`
- Standby card (line 205) is generic ("NO SIGNAL — press POWER") — leave it.
- `#video` CSS uses `object-fit: fill` to stretch GB's 10:9 to the 4:3 `.screen` box (like a Super Game
  Boy). No CSS *rule* change needed.

---

## 9. Build / run + Risks

### Build & run
```sh
# build (pinned toolchain via rust-toolchain.toml)
cd ~/pokemon-pvp-red
cargo build --release

# run DMG (default):                 open http://localhost:3000 and click POWER
cargo run --release

# run color GBC:
cargo run --release -- "Pokemon Red Color.gbc"
```
No `Cargo.toml` change is required: gambatte loads through the existing `libloading` path; libvpx/libopus
are unchanged. (`vpx-encode` keeps `ffi-generate`; nothing GB-specific.)

### File changes summary
1. `src/video.rs` — add `PIXFMT_*` consts, `rgb565_to_i420`, and `frame_to_i420` dispatcher (§2a). Keep
   `xrgb_to_i420`.
2. `src/pipeline.rs` — import `frame_to_i420` instead of `xrgb_to_i420`; pass `f.fmt` in the step-4 call (§2b).
3. `src/libretro.rs` — add `"Select" => Btn(ID_SELECT),` to `map_button` (§4a). (Optional gambatte arm in
   `forced_option`, §5 — skip.)
4. `src/main.rs` — `DEFAULT_ROM` = `Pokemon Red.gb`, `DEFAULT_CORE` = gambatte dylib (§6a).
5. `static/index.html` — new `KEYMAP`, new hint, new caption (§4b/4c, §8).
6. (new) `scripts/apply_ips.py` — reproduce the `.gbc` from the IPS patch (§6c). Optional; the `.gbc`
   already exists.

### Risks
- **Pitch padding (512 vs 320).** The #1 footgun. gambatte pads each RGB565 row to 256 px (512 B); the
  visible image is the first 160 px. The converter MUST stride by `pitch` and read only `i < sw(160)` —
  the code in §2a does exactly this (`row = j*pitch`, inner loop `0..cw` with `cw <= 160`). If anyone
  "optimizes" to `j*sw*2` the image will skew/garble.
- **Endianness of RGB565.** Little-endian u16 (`lo | hi<<8`). The decode in §2a is correct; don't swap.
- **Audio rate is read from the core, but only at load.** Fine for gambatte (fixed 32 768 Hz). If you
  ever swap to SameBoy (2.097 MHz) you MUST replace the 2-tap linear resampler with a decimating/averaging
  one or audio will alias — this is the reason gambatte was chosen.
- **VP8 minimum size.** 160×144 is comfortably above libvpx minimums; no issue. The 3500 kbps bitrate is
  overkill for this size (harmless; lower it to ~800 kbps if you want to trim bandwidth).
- **Color-correction look.** gambatte's default `gambatte_gbc_color_correction` warms/darkens to mimic a
  real GBC LCD. The stream will look slightly muted vs. a PC emulator's raw palette. Set the option to
  `disabled` (§5) if a punchier web image is preferred — cosmetic only.
- **DMG vs GBC is ROM-driven, not a flag.** Running `Pokemon Red.gb` is intentionally grayscale (header
  0x143=0x00). Color requires the `.gbc` (0x143=0xC0). This is correct behavior, not a bug — document it
  so nobody files "GB is gray" as a defect.
```
