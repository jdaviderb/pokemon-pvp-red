# NES Emulator (headless, server-side) — Blueprint

**Component:** Headless NES emulator core for server-side emulation.
**Chosen crate:** `tetanes-core = "0.14.1"` (the engine behind the TetaNES emulator by lukexor).
**Verified on:** macOS arm64, rustc 1.86, in a throwaway project `/tmp/nes-emu-probe`.
**Verdict:** WORKS. Loads `MK1.nes` (mapper 4 / MMC3), renders a non-blank 256x240 RGBA framebuffer, produces ~800 mono f32 audio samples/frame at 48 kHz. One real header-parsing bug had to be worked around (see "CRITICAL GOTCHA").

---

## 1. Why tetanes-core (vs the alternatives)

| Crate | Headless lib API | MMC3 (mapper 4)? | Notes |
|---|---|---|---|
| **tetanes-core 0.14.1** | **Excellent** — `ControlDeck` is a clean, documented library entry-point with explicit `clock_frame`, `frame_buffer`, `audio_samples`, `set_sample_rate`, `joypad_mut`. Has an explicit `HeadlessMode`. | **YES** — mapper 4 → `Txrom` (MMC3). >30 mappers, ~90% of licensed games. | Pure Rust, no C deps, compiles clean. Pinned & published on crates.io. **PICKED.** |
| plastic_core | Good — `NES` struct you clock and read `pixel_buffer()` / `audio_buffer()`. | Yes | Viable fallback. API slightly less ergonomic; sample-rate config less explicit. |
| nes-rust | OK but more bare-bones, geared at wasm/SDL frontends. | Partial | Less complete mapper coverage. |
| rustynes / pinky | Hobby-grade, weaker/older library ergonomics. | Varies | Not recommended for a production server. |

tetanes-core wins on: documented `ControlDeck` API, explicit headless mode, settable APU sample rate, and confirmed MMC3 support with the exact ROM in question.

`docs.rs` failed to build 0.14.1's docs (a known docs.rs build issue), so **all API below was read directly from the v0.14.1 source on GitHub** (`lukexor/tetanes`, `tetanes-core/src/`), not from docs.rs guesswork.

### Cargo.toml

```toml
[dependencies]
tetanes-core = "0.14.1"
```

- `tetanes-core` 0.14.1 is `edition = "2024"`, `rust-version = "1.85.0"`. rustc 1.86 satisfies this.
- Pure Rust. **No system libraries needed** (no libvpx/opus/ffmpeg linkage from this crate). Compiles with no `PKG_CONFIG_PATH`/`LIBRARY_PATH` fiddling (those are only needed by the encoder crates, not by the emulator).
- Transitive deps pulled: serde, bincode 2, bitflags 2, flate2, rand 0.10, dirs, thiserror 2, tracing, cfg-if. All build cleanly.

---

## 2. The exact API (read from v0.14.1 source)

All of the following live in `tetanes_core::control_deck::ControlDeck` unless noted.

### Imports you actually need

```rust
use tetanes_core::control_deck::{ControlDeck, Config, HeadlessMode};
use tetanes_core::input::{JoypadBtn, Player};        // NOTE: JoypadBtn is NOT in the prelude
use tetanes_core::common::NesRegion;                  // optional
// Prelude alternative (re-exports most of the above except JoypadBtn):
// use tetanes_core::prelude::*;   // gives ControlDeck, Config, HeadlessMode, Player, NesRegion, ...
```

The prelude (`tetanes_core::prelude`) re-exports: `Action`, `Apu`, `Channel`, `Cart`, `Clock`, `NesRegion`, `Regional`, `Reset`, `ResetKind`, `Sample`, `Config`, `ControlDeck`, `HeadlessMode`, `Cpu`, `GenieCode`, `FourPlayer`, `Input`, `Player`, `Map`, `Mapper`, `MapperRevision`, `RamState`, `Mirroring`, `Ppu`, `Frame`. **`JoypadBtn` and `JoypadBtnState` are NOT in the prelude — import them from `tetanes_core::input`.**

### Construction

```rust
// Default config (NTSC auto-detect region, NTSC video filter, all APU channels on):
let mut deck = ControlDeck::new();

// Or with a custom Config (recommended for a server — see headless note below):
let cfg = Config::default();   // fields are all `pub`; tweak as needed
let mut deck = ControlDeck::with_config(cfg);
```

`Config` notable public fields (struct is `#[serde(default)]`, all `pub`):
`filter: VideoFilter` (default `Ntsc`), `region: NesRegion` (default `Auto`), `ram_state: RamState`,
`four_player: FourPlayer`, `zapper: bool`, `genie_codes: Vec<GenieCode>`, `concurrent_dpad: bool`,
`channels_enabled: [bool; 6]`, `headless_mode: HeadlessMode`, `data_dir: PathBuf`,
`mapper_revisions: MapperRevisionsConfig`, `emulate_ppu_warmup: bool`.

> **Headless tip:** `HeadlessMode` has bitflags `NO_AUDIO (0x01)` and `NO_VIDEO (0x02)`. For our use case we WANT both audio and video, so leave `headless_mode` empty (the default). The flags exist only to *disable* output for perf. Do **not** set them for the streaming server.

### 1) Load a ROM — mapper 4 / MMC3 confirmed

Two methods:

```rust
// (a) From a path (opens the file itself, BufReader internally):
pub fn load_rom_path(&mut self, path: impl AsRef<std::path::Path>) -> control_deck::Result<LoadedRom>;

// (b) From any reader / &[u8] — this is what a server uses (ROM bytes from a "room"):
pub fn load_rom<S: ToString, F: std::io::Read>(
    &mut self, name: S, rom: &mut F,
) -> control_deck::Result<LoadedRom>;
```

Use `std::io::Cursor::new(&bytes)` as the `&mut F` to load from a `&[u8]`:

```rust
let mut reader = std::io::Cursor::new(rom_bytes.as_slice());
let loaded = deck.load_rom("MK1.nes", &mut reader)?;   // LoadedRom { name, battery_backed, region }
```

`load_rom` internally:
- parses the iNES/NES2.0 header (`Cart::from_rom`),
- maps the mapper number → concrete mapper. **Mapper 4 (and 76, 88, 95, 154, 206) → `Txrom` = MMC3.** (Source: `cart.rs:211` `4 | 76 | 88 | 95 | 154 | 206 => Txrom::load(...)`.)
- returns `Error::UnimplementedMapper(n)` if the mapper isn't supported,
- **calls `reset(ResetKind::Hard)` which sets `running = true`.** This matters: `clock_frame()` returns `Err(RomNotLoaded)` if `running == false`, so you MUST load a ROM before clocking. After a successful `load_rom`, `deck.is_running() == true`.

`LoadedRom` (returned & also via `deck.loaded_rom()`):
```rust
pub struct LoadedRom { pub name: String, pub battery_backed: bool, pub region: NesRegion }
```

Confirm the mapper at runtime:
```rust
let mapper_name = format!("{:?}", deck.mapper());  // -> "Txrom(...)" for MMC3
// deck.mapper() -> &Mapper ; deck.cart_region() -> Option<NesRegion> ; deck.cart_battery_backed() -> Option<bool>
```
For MK1.nes the probe printed: `mapper = Txrom`, `region = Ntsc`, `battery_backed = true`.

### 2) Step exactly ONE video frame

```rust
pub fn clock_frame(&mut self) -> control_deck::Result<()>;
```
Runs the CPU/PPU/APU until the PPU frame number advances by one (respecting the configured frame-speed; at default 1.0x speed this is exactly one NES frame). Returns `Err(RomNotLoaded)` if no ROM is loaded, `Err(CpuCorrupted)` on a bad opcode.

Convenience combined variants (handy for the streaming loop):
```rust
// Clock a frame and hand you (frame_rgba, audio_f32) in one call; auto-clears audio after:
pub fn clock_frame_output<T>(&mut self, handle: impl FnOnce(&[u8], &[f32]) -> T)
    -> control_deck::Result<T>;

// Clock a frame and COPY into your pre-allocated buffers; auto-clears audio:
pub fn clock_frame_into(&mut self, frame_buffer: &mut [u8], audio_samples: &mut [f32])
    -> control_deck::Result<()>;

// Run-ahead variants exist too (clock_frame_ahead / _into) to reduce input latency.
```

> For the WebRTC server, prefer the **explicit** pattern (`clock_frame` then read `frame_buffer()` + `audio_samples()` then `clear_audio_samples()`) so you control buffer lifetimes, OR use `clock_frame_output(|frame, audio| { ... })` which clears audio for you.

### 3) Read the video framebuffer

```rust
// Filtered, ready-to-display RGBA bytes. THIS is what you feed an encoder.
pub fn frame_buffer(&mut self) -> &[u8];        // len = 256 * 240 * 4 = 245_760

// Raw PPU palette indices (one u16 per pixel) if you want to do your own coloring.
pub fn frame_buffer_raw(&mut self) -> &[u16];   // len = 256 * 240 = 61_440

// Zero-copy into your own buffer (takes &self, not &mut):
pub fn frame_buffer_into(&self, buffer: &mut [u8]);  // buffer.len() must be 245_760
```

**Exact pixel format (VERIFIED from `video.rs`):**
- **Layout: RGBA8888, 4 bytes per pixel, in `R, G, B, A` order.** `pixels[0]=R, pixels[1]=G, pixels[2]=B`, and **alpha is always 255** (the buffer is pre-filled with `[0,0,0,255]` and filters never touch the 4th byte).
- **Dimensions: 256 (W) × 240 (H).** Constants: `tetanes_core::ppu::size::{WIDTH=256, HEIGHT=240, FRAME=61440}`.
- **Byte length: `256 * 240 * 4 = 245_760`.** Also exposed as `tetanes_core::video::Frame::SIZE`.
- The default filter is `VideoFilter::Ntsc` (a Bisqwit NTSC filter applied on the CPU). You can switch to a cheaper nearest-neighbor palette decode with `deck.set_filter(VideoFilter::Pixellate)` — both produce the same 256x240 RGBA layout. For a streaming server, `Pixellate` is cheaper and crisper; `Ntsc` looks more authentic. Either is fine for libvpx/ffmpeg input (you'll convert RGBA→I420 anyway).

> **For VP8/VP9 (libvpx) you need I420/YUV420.** RGBA→I420 conversion is your job downstream (e.g. via the `yuvutils-rs`/`yuv` crate, or ffmpeg/swscale, or a hand-rolled BT.601 conversion). tetanes only gives you RGBA.

> The alpha channel is always 0xFF, so if your encoder wants RGB you can also stride-skip every 4th byte, or pass RGBA and let the converter ignore alpha.

### 4) Read audio samples + set the sample rate to 48000 Hz

```rust
// Configure sample rate. CALL THIS (any time; safe before or after load_rom). Default is 44_100.0.
pub fn set_sample_rate(&mut self, sample_rate: f32);   // pass 48_000.0 for WebRTC/Opus

// Read this frame's audio (accumulated since the last clear):
pub fn audio_samples(&self) -> &[f32];

// You MUST clear after consuming, or samples accumulate across frames:
pub fn clear_audio_samples(&mut self);
```

**Exact audio format (VERIFIED from `apu.rs` / `bus.rs`):**
- **Sample format: `f32`.** Values are the APU's mixed output, roughly in `[-1.0, 1.0]` range (post-filter-chain).
- **Channels: MONO (1 channel).** The APU mixes all 6 NES channels (Pulse1, Pulse2, Triangle, Noise, DMC, Mapper) down into a single f32 stream. `audio_samples()` is a flat, non-interleaved mono slice. **There is no stereo.** For Opus, encode as mono, or duplicate each sample L=R to make stereo.
- **Sample rate: whatever you set via `set_sample_rate`.** Default `Apu::DEFAULT_SAMPLE_RATE = 44_100.0`. Set `48_000.0`.
- **Count per frame:** at 48 kHz / 60 fps NTSC you get ~**800 samples/frame**, fluctuating 798–800 (the probe measured 798 avg, 799 last frame, 95775 over 120 frames; 48000/60 = 800). It is NOT a fixed constant per frame — buffer accordingly.

Standard per-frame loop:
```rust
deck.clock_frame()?;
let pcm: &[f32] = deck.audio_samples();   // mono, 48kHz, ~800 samples
// ... resample-to-stereo / feed Opus encoder ...
deck.clear_audio_samples();               // REQUIRED every frame
```

### 5) Controller / button input (player 1)

```rust
// Get a mutable Joypad for a slot:
pub fn joypad_mut(&mut self, slot: Player) -> &mut tetanes_core::input::Joypad;
pub fn joypad(&mut self, slot: Player) -> &tetanes_core::input::Joypad;  // read-only

// On the Joypad:
impl Joypad {
    // button: anything Into<JoypadBtnState>; JoypadBtn implements that.
    pub fn set_button(&mut self, button: impl Into<JoypadBtnState>, pressed: bool);
    pub const fn button(&self, button: JoypadBtnState) -> bool;  // query
}
```

`Player` enum: `One, Two, Three, Four`.
`JoypadBtn` enum (from `tetanes_core::input`): `A, B, Select, Start, Up, Down, Left, Right, TurboA, TurboB`.

Press / release example:
```rust
use tetanes_core::input::{JoypadBtn, Player};

deck.joypad_mut(Player::One).set_button(JoypadBtn::Start, true);   // press Start
deck.joypad_mut(Player::One).set_button(JoypadBtn::A, true);       // press A
deck.joypad_mut(Player::One).set_button(JoypadBtn::Up, true);      // hold Up
// ... clock frames while held ...
deck.joypad_mut(Player::One).set_button(JoypadBtn::Up, false);     // release Up
```

Button state is sticky — it stays pressed across frames until you set it `false`. So map a browser `keydown` → `set_button(.., true)` and `keyup` → `set_button(.., false)`.

> **D-pad caveat:** by default opposite D-pad directions cancel (real-NES behavior: pressing Left auto-clears Right). To allow simultaneous opposite directions call `deck.set_concurrent_dpad(true)` (or set `Config.concurrent_dpad = true`).

---

## 3. CRITICAL GOTCHA — header bug blocks MK1.nes out of the box

**Symptom:** `deck.load_rom(...)` fails with:
```
Cart(InvalidHeader { byte: 11, value: 112, message: "header ram size larger than 64" })
```

**Root cause (confirmed by reading `cart.rs` + hex-dumping the ROM):**
`MK1.nes` is a **NES 2.0** ROM (`header[7] & 0x0C == 0x08`). Its **header byte 10 = `0x70`**. Per the NES 2.0 spec, byte 10 is split into two nibbles: low nibble = PRG-RAM shift, **high nibble = PRG-NVRAM (battery EEPROM) shift**. High nibble `0x7` means 64<<7 = 8 KB of battery-backed save RAM (the MK1 hack's SRAM).

tetanes-core 0.14.1 sets `prg_ram_shift = header[10]` (the WHOLE byte, `0x70`) and then computes:
```rust
fn calculate_ram_size(value: u8) -> Result<usize> {
    if value > 0 {
        64usize.checked_shl(value.into())  // 64 << 112  ==> overflow ==> Err
            .ok_or_else(|| Error::InvalidHeader { byte: 11, value, message: "header ram size larger than 64" })
    } else { Ok(0) }
}
```
`64 << 112` overflows `usize`, so `checked_shl` returns `None` → load aborts. (The error hardcodes `byte: 11` in its message even though the offending value came from byte 10's `prg_ram_shift`.) This is a real upstream bug: tetanes feeds the full byte instead of masking the nibble.

**Hex of the MK1.nes header (first 16 bytes):**
```
4e 45 53 1a 08 20 42 08 00 00 70 00 00 00 00 01
                              ^^ byte10 = 0x70  (the trigger)
```

**Fix / workaround (what the probe does, verified working):** before handing the bytes to `load_rom`, if it's a NES 2.0 ROM, zero the PRG-RAM/NVRAM and CHR-RAM size bytes (10 and 11). Battery RAM bytes are irrelevant to headless video/audio rendering, and the ROM still loads as mapper 4 / NTSC / `battery_backed=true`:

```rust
fn sanitize_nes2_ram_header(bytes: &mut [u8]) {
    if bytes.len() >= 16 && &bytes[0..4] == b"NES\x1a" && (bytes[7] & 0x0C) == 0x08 {
        bytes[10] = 0x00; // PRG-RAM / PRG-NVRAM size (the 0x70 that overflows)
        bytes[11] = 0x00; // CHR-RAM / CHR-NVRAM size (defensive)
    }
}
```
Then:
```rust
let mut rom = std::fs::read(path)?;     // or bytes from the network
sanitize_nes2_ram_header(&mut rom);
let mut cur = std::io::Cursor::new(rom.as_slice());
deck.load_rom("MK1.nes", &mut cur)?;    // now succeeds
```

**Alternative workarounds** (not needed, but for completeness): downgrade the header to plain iNES by clearing the NES2.0 bits in `header[7]` (riskier — loses NES2.0 fields), or pre-process ROMs with an external tool. The nibble-zeroing approach above is the simplest and is what we verified.

> Other typical mapper-4 ROMs with a clean header (byte 10/11 == 0) load fine without this. Only ROMs that declare battery NVRAM via the high nibble of byte 10 hit this. Keep the sanitizer in the loader unconditionally — it's a no-op for clean headers.

---

## 4. Full, compile-ready reference loader + frame loop

This is the exact pattern the server should use (this is essentially the verified probe `main.rs`):

```rust
use tetanes_core::control_deck::ControlDeck;
use tetanes_core::input::{JoypadBtn, Player};

/// Zero the NES 2.0 PRG-RAM/CHR-RAM size header bytes to dodge the
/// tetanes-core 0.14.1 `64 << n` overflow on ROMs that declare battery NVRAM.
fn sanitize_nes2_ram_header(bytes: &mut [u8]) {
    if bytes.len() >= 16 && &bytes[0..4] == b"NES\x1a" && (bytes[7] & 0x0C) == 0x08 {
        bytes[10] = 0x00;
        bytes[11] = 0x00;
    }
}

pub struct Emu {
    deck: ControlDeck,
}

impl Emu {
    pub fn new(rom_bytes: &[u8], name: &str) -> anyhow::Result<Self> {
        let mut deck = ControlDeck::new();
        deck.set_sample_rate(48_000.0);                 // WebRTC/Opus target
        deck.set_concurrent_dpad(true);                 // optional: allow L+R / U+D

        let mut bytes = rom_bytes.to_vec();
        sanitize_nes2_ram_header(&mut bytes);
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        deck.load_rom(name, &mut cur)
            .map_err(|e| anyhow::anyhow!("load_rom: {e:?}"))?;
        debug_assert!(deck.is_running());
        Ok(Self { deck })
    }

    /// Clock one NES frame and return (rgba_256x240, mono_f32_48k).
    pub fn step_frame(&mut self) -> anyhow::Result<(&[u8], Vec<f32>)> {
        self.deck.clock_frame().map_err(|e| anyhow::anyhow!("clock_frame: {e:?}"))?;
        let audio = self.deck.audio_samples().to_vec();   // mono f32 @48k, ~800 samples
        self.deck.clear_audio_samples();
        let frame = self.deck.frame_buffer();             // RGBA, 245_760 bytes
        Ok((frame, audio))
    }

    pub fn set_button(&mut self, btn: JoypadBtn, pressed: bool) {
        self.deck.joypad_mut(Player::One).set_button(btn, pressed);
    }
}
```

(If the simultaneous-borrow of `audio` + `frame` is awkward in your real loop, use
`deck.clock_frame_output(|frame, audio| { /* copy out both */ })` which also clears audio for you.)

---

## 5. VERIFICATION — real probe output

Throwaway project at `/tmp/nes-emu-probe` (own `target/`, did NOT touch the main project). It loads
`~/projects-2026/nes-MK1/out/MK1.nes`, sets 48 kHz, presses Start (then A), runs 120 frames,
and dumps framebuffer length/dims/checksum and audio sample counts.

**Build:** `cargo build --release` compiled `tetanes-core v0.14.1` and the probe with **no errors and no system-lib linkage** (pure Rust; no PKG_CONFIG/LIBRARY_PATH needed).

**`cargo run --release` output:**

```
loaded_rom name        = MK1.nes
battery_backed         = true
region                 = Ntsc
is_running             = true
mapper variant         = Txrom  (mapper 4 MMC3 == Txrom)
---- VIDEO ----
frame_buffer() len     = 245760 bytes (expect 256*240*4 = 245760)
frame_buffer_raw() len = 61440 u16 (expect 256*240 = 61440)
dimensions             = 256 x 240 (RGBA, 4 bytes/pixel, alpha=255)
pixel byte sum         = 22104637
non-zero bytes         = 117091 / 245760
fnv1a checksum         = 0x122921b7ef2b0cf2
first 16 bytes         = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
---- AUDIO ----
sample_rate set        = 48000 Hz
audio fmt              = f32, mono (single mixed stream)
samples last frame     = 799
total samples / 120 fr = 95775
avg samples per frame  = 798
expected ~ 48000/60    = 800

ALL CHECKS PASSED
```

**What this proves:**
- ROM loads as **mapper 4 / MMC3 (`Txrom`)**, NTSC, battery-backed — MMC3 support confirmed with the real target ROM.
- Framebuffer is exactly **245760 bytes = 256×240×4 (RGBA)**; raw is **61440 u16**.
- Screen is **NOT blank**: pixel byte sum = 22,104,637, with 117,091 of 245,760 bytes non-zero (a real rendered MK1 screen after 120 frames). The leading pixels are black `[0,0,0,255]` (top border) which is expected; non-zero content lower in the buffer.
- Audio: **f32 mono at 48 kHz**, ~**798–800 samples/frame** (95,775 over 120 frames ≈ 798/frame), matching the theoretical 48000/60 = 800.

---

## 6. Quick-reference cheat sheet for the next engineer

```text
CRATE:        tetanes-core = "0.14.1"   (pure Rust, no C deps)
ENTRY TYPE:   tetanes_core::control_deck::ControlDeck
CONSTRUCT:    ControlDeck::new()  /  ControlDeck::with_config(Config::default())
SAMPLE RATE:  deck.set_sample_rate(48_000.0)            // default 44_100; mono f32 out
LOAD (bytes): deck.load_rom("name", &mut Cursor::new(&bytes))  -> LoadedRom
LOAD (path):  deck.load_rom_path("MK1.nes")
  !! PRE-PATCH header bytes 10 & 11 to 0x00 for NES2.0 ROMs (overflow bug) !!
RUNNING:      load_rom auto-resets + sets running=true; clock_frame errs if not running
STEP 1 FRAME: deck.clock_frame()?  (or clock_frame_output(|fb,audio| ...))
VIDEO:        deck.frame_buffer() -> &[u8] RGBA, 256x240, 245_760 bytes, alpha always 255
              deck.frame_buffer_raw() -> &[u16] palette indices, 61_440
              deck.set_filter(VideoFilter::Pixellate | Ntsc)
AUDIO:        deck.audio_samples() -> &[f32] MONO @ set rate (~800/frame @48k)
              deck.clear_audio_samples()   // call EVERY frame
INPUT:        deck.joypad_mut(Player::One).set_button(JoypadBtn::Start, true/false)
              JoypadBtn: A B Select Start Up Down Left Right TurboA TurboB
              deck.set_concurrent_dpad(true)  // allow opposite dirs
DIMS CONSTS:  tetanes_core::ppu::size::{WIDTH=256, HEIGHT=240, FRAME=61440}
              tetanes_core::video::Frame::SIZE = 245_760
DOWNSTREAM:   You must convert RGBA -> I420/YUV420 yourself for VP8/VP9 (libvpx),
              and duplicate mono->stereo (or encode mono) for Opus.
```

### Sources
- tetanes-core 0.14.1 source (read directly): https://github.com/lukexor/tetanes — `tetanes-core/src/{control_deck.rs, input.rs, video.rs, apu.rs, bus.rs, cart.rs, ppu.rs}`
- crates.io: https://crates.io/crates/tetanes-core
- docs.rs (0.14.1 build failed; 0.12.2 last good): https://docs.rs/tetanes-core
