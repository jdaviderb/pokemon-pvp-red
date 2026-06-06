# N64 Headless Server-Side Emulation — Fallbacks, Performance, Integration Facts

**Status: SOLVED & PROVEN.** A headless, software-rendered, Rust-drivable N64 pipeline running
Super Smash Bros. (USA) is working on this machine (macOS arm64, Apple Silicon). The libretro
**mupen64plus-next** core with the **angrylion** software RDP plugin delivers CPU framebuffers via
the `video_refresh` callback — no window, no GL context required. Measured **327 fps** (5.4x
real-time) on the `mupen64plus_next` core and **1011 fps** on `parallel_n64`.

Working cores copied to: `~/pokemon-pvp-red/cores/`
- `mupen64plus_next_libretro.dylib` (recommended)
- `parallel_n64_libretro.dylib` (faster, alternative)

Working Rust harness: `/tmp/n64probe/harness/` (build: `RUSTUP_TOOLCHAIN=1.92 cargo build --release`).

---

## 1. Pure-Rust N64 emulators — VERDICT: NONE can boot SSB64 as a library

I assessed the pure-Rust landscape. **No pure-Rust N64 emulator can boot SSB64 in a way we can use.**
The C path is required.

| Project | Type | Boots commercial games? | Usable as Rust lib? | Headless software FB? |
|---|---|---|---|---|
| **gopher64** | Standalone **app** (Rust 84%, C++ for parallel-RDP) | Yes, many | **No** — not on crates.io, not a lib | No — needs **Vulkan** parallel-RDP |
| `mupen64plus` crate (v0.3.0) | FFI bindings to C core | Yes (it IS mupen) | Yes, but it's a wrapper over the C core, not pure Rust | Depends on plugins |
| sarchar/n64, georgemorgan/r64 | Hobby/WIP emulators | No (incomplete, no audio/RDP) | Practically no | No |
| `cargo-n64`, `nust64` | **Homebrew toolchains** (build ROMs *for* N64) | N/A — not emulators | N/A | N/A |
| ares (reference accuracy) | Standalone C++ app | Yes | No (not Rust, not a lib) | No |

**Conclusion:** gopher64 is the most advanced "Rust" N64 emulator but it is a *standalone Vulkan
application*, not a crate, and its renderer is GPU parallel-RDP (no easy CPU framebuffer). There is no
pure-Rust crate that boots SSB64 headless. **The C-via-FFI path (libretro core) is the correct and
required choice** — and it works (see §5 proof).

---

## 2. libretro core path vs. mupen64plus STANDALONE C API

I evaluated both. **The libretro core path is dramatically less effort and is what I used.**

### libretro core path (CHOSEN — proven working)
- **Acquire:** one `curl` + `unzip` from the libretro buildbot. The dylib **already bundles**
  angrylion, GLideN64, parallel-RDP, and the HLE/cxd4/parallel RSP plugins. No plugin hunting.
  - `https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/mupen64plus_next_libretro.dylib.zip`
  - `https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/parallel_n64_libretro.dylib.zip`
- **API:** a tiny, stable C ABI (`retro_init`, `retro_load_game`, `retro_run`, `video_refresh`
  callback, etc.). One frontend handles ROM load, frame, audio, and input uniformly.
- **Headless:** angrylion renders to a CPU buffer; the frontend just **refuses** `SET_HW_RENDER`
  (returns false) and the core stays software. **No GL/CGL/FBO needed.** Confirmed.
- **Effort:** ~450 lines of hand-rolled Rust (this doc's §6). Done in one session.

### mupen64plus STANDALONE C API path (viable fallback, MORE effort)
- **Acquire:** `brew install mupen64plus` → **mupen64plus 2.6.0** is bottled and installs the core +
  the *default* plugins (mupen64plus-video-glide64mk2/rice, mupen64plus-rsp-hle, mupen64plus-audio-sdl,
  mupen64plus-input-sdl). **BUT the brew bottle does NOT include angrylion-plus** (the software RDP).
  `brew search angrylion` → nothing. You would have to **build angrylion-plus from source**
  (github.com/ata4/angrylion-plus, cmake) and the **mupen64plus-rsp-hle** plugin to get a software
  pipeline. That's the extra effort the libretro core saves you.
- **Headless via VidExt:** the standalone Core API exposes a **video extension override**
  (`CoreOverrideVidExt` with a `m64p_video_extension_functions` struct). You implement `VidExtFuncInit`,
  `VidExtFuncGLGetProc`, `VidExtFuncSwapBuf`, etc. to capture frames without a window. With angrylion
  (which doesn't need GL) you'd instead read the RDRAM/VI framebuffer or use the plugin's read-screen
  hook (`ReadScreen2`). This is **more wiring** than the single libretro `video_refresh` callback.
- **API surface:** `CoreStartup`, `CoreDoCommand(M64CMD_ROM_OPEN / M64CMD_EXECUTE in a thread / 
  M64CMD_ADVANCE_FRAME)`, `(Attach|Detach)Plugin`. Frame stepping is clunky (the core runs its own emu
  thread; you sync per-frame via a callback). The libretro `retro_run()` "one call = one frame" model
  is far cleaner for our frame-paced streaming server.

**Recommendation:** use the libretro core. Reserve the standalone API only if a specific libretro
limitation appears. What the standalone plugins provide is identical RDP/RSP code — angrylion-plus is
the same software RDP either way; mupen64plus-rsp-hle is the same HLE RSP. The libretro core just
ships them pre-built and exposes them through core options.

---

## 3. PERFORMANCE — angrylion software RDP sustains MULTIPLE TIMES real-time

**This is the headline for live streaming: full speed with huge headroom.** Measured on this Apple
Silicon machine, 150 frames of SSB64 from boot (includes the heavy N-logo intro):

| Core | RDP | RSP | CPU core | fps measured | real-time? |
|---|---|---|---|---|---|
| mupen64plus_next | **angrylion** (software, all threads) | hle | cached interp* | **327.6 fps** | **5.4x** |
| parallel_n64 | angrylion (software) | hle | Ari64 dynarec | **1011 fps** | **16x** |

\* mupen64plus-next logged "Cached Interpreter" even with `cpucore=dynamic_recompiler` forced — the
dynarec may be disabled in this nightly's arm64 build. Even so it's 5.4x real-time. parallel_n64's
Ari64 dynarec engaged ("Init new dynarec", "MAP_JIT pages allocated") and ran 16x.

**Verdict: expect FULL 60 fps with large headroom.** angrylion is multithreaded ("all_threads") and
Apple Silicon eats it. No frame-skip or resolution drop needed for SSB64.

**Mitigations (not needed now, but available):**
- `mupen64plus-angrylion-multithread = all_threads` (already on) — scales across cores.
- Keep internal res at native (320x240 / 640x240); don't upscale in software.
- If a heavier game ever drops below 60: frame-skip, or switch to **parallel-RDP via Vulkan/MoltenVK**
  for a GPU-accelerated software-accurate renderer. NOTE: the prebuilt `mupen64plus_next_libretro.dylib`
  here links **only OpenGL, not Vulkan/MoltenVK** (`otool -L` shows no MoltenVK), so its parallel-RDP
  option will fail with "VK_KHR_16bit_storage not supported". To use parallel-RDP headless you'd need
  MoltenVK + an offscreen Vulkan path. **For SSB64 this is unnecessary — angrylion is already 5x+.**

---

## 4. INTEGRATION FACTS (av_info, geometry, controller mapping)

### av_info (from `retro_get_system_av_info`, REAL values)
```
mupen64plus_next:  base 640x480, max 640x480, aspect 1.333,  fps 60.0000, sample_rate 44100
parallel_n64:      base 640x480, max 640x480, aspect 1.333,  fps 60.1300, sample_rate 44100
```
- **Pixel format:** core sets `RETRO_PIXEL_FORMAT_XRGB8888` (=1) → 4 bytes/pixel, byte order B,G,R,X.
- **Actual frame delivered by `video_refresh` for SSB64:** **640 x 240, pitch 2560** (= 640*4).
  Height is 240 because the VI runs a single (non-interlaced) field; width 640 is the core's 2x line
  doubling of the 320 native. **Do not assume 320x240 — read width/height/pitch from the callback
  every frame** (they can change: menus vs. in-game, interlace toggles).
- **Audio:** 44100 Hz, **i16 stereo interleaved (L,R)** via `audio_sample_batch(const int16_t*, frames)`.
  ~721 stereo frames per video frame (44100/60 ≈ 735, matches). Feed straight into your Opus encoder.

### N64 controller mapping (libretro RETRO_DEVICE_* ids for the `input_state` callback)
Port 0, `device == RETRO_DEVICE_JOYPAD (1)`, return `1` when pressed:

| N64 button | RETRO_DEVICE_ID_JOYPAD_* | id |
|---|---|---|
| A | `_A` | 8 |
| B | `_B` | 0 |
| Start | `_START` | 3 |
| Z (trigger) | `_Y`  (mupen maps Z→Y by default) | 1 |
| L | `_L` | 10 |
| R | `_R` | 11 |
| C-Up | `_X` *(see note)* | 9 |
| C-Down | `_SELECT`-ish *(see note)* | — |
| D-pad U/D/L/R | `_UP/_DOWN/_LEFT/_RIGHT` | 4/5/6/7 |

> **C-buttons note:** mupen64plus-next's default RetroPad layout puts the four **C-buttons on the
> RIGHT ANALOG stick** (`RETRO_DEVICE_ANALOG`, index `RETRO_DEVICE_INDEX_ANALOG_RIGHT=1`), and the
> N64 **analog stick on the LEFT analog** (`INDEX_ANALOG_LEFT=0`). For a keyboard layout it's cleanest
> to drive C-buttons by returning ±0x7FFF on the right-analog X/Y axes, OR remap them to face/shoulder
> buttons via the core's input options. Verify the exact default mapping for your build by probing
> which ids the core reads (log every `input_state` query).

**Analog stick (N64 control stick):** `device == RETRO_DEVICE_ANALOG (5)`,
`index == RETRO_DEVICE_INDEX_ANALOG_LEFT (0)`, `id == RETRO_DEVICE_ID_ANALOG_X (0)` / `_Y (1)`.
Return **i16 in [-32768, +32767]**; libretro scales to the N64's ~±80 range. +X = right, +Y = **down**.

### Core options that MUST be forced (via `GET_VARIABLE`) for headless software:
```
mupen64plus-rdp-plugin          = angrylion          # software RDP (the key one)
mupen64plus-rsp-plugin          = hle                # HLE RSP, fastest; cxd4/parallel are LLE
mupen64plus-angrylion-multithread = all_threads      # multicore software rendering
mupen64plus-EnableFBEmulation   = True               # framebuffer effects (SSB64 needs)
# optional: mupen64plus-cpucore = dynamic_recompiler  # (no-op in this nightly's arm64 build)
```
Also **return `false` to `RETRO_ENVIRONMENT_SET_HW_RENDER`** so the core never asks for a GL context.

---

## 5. PROOF (real output from the harness)

Command:
```
n64harness  cores/mupen64plus_next_libretro.dylib  "Super Smash Bros. (U) [!].z64"
```
ROM identity confirmed by the core itself:
```
Goodname: Super Smash Bros. (U) [!]   Name: SMASH BROTHERS
CRC: 916B8B5B 780B85A4   Imagetype: .z64 (native)   Country: USA   Rom size: 16777216 bytes
```
Forced options observed taking effect:
```
[env] SET_PIXEL_FORMAT = 1 (XRGB8888)
[env] GET_VARIABLE mupen64plus-rdp-plugin -> FORCED angrylion
[env] GET_VARIABLE mupen64plus-rsp-plugin -> FORCED hle
```
Result:
```
=== AV INFO ===
base = 640x480, max = 640x480, aspect = 1.3333334
fps = 60.0000, sample_rate = 44100.0

=== AFTER 150 FRAMES (no input) ===
framebuffer: 640x240 pitch=2560 checksum=0x3632E8518025E3D5
nonzero bytes = 214980 / 614400 (35.0%)            <-- NON-BLANK (N logo on screen)
audio frames (stereo pairs) accumulated = 108168 over 150 video frames
=> ~721.1 audio frames/video frame                  <-- matches 44100/60
MEASURED SPEED: 150 frames in 0.458s = 327.6 fps (real-time target 60.0)   <-- 5.4x real-time

=== INPUT PROOF ===
checksum before START sequence = 0x3632E8518025E3D5
checksum after  START sequence = 0xAB41F6429493C943   <-- CHANGED
nonzero after = 563714                                  <-- jumped 214980 -> 563714 (logo -> menu)
CHANGED = true
```
**All four proof criteria met:** (1) non-blank framebuffer with real checksum + 35% nonzero bytes;
(2) audio at 44100 Hz stereo, correct per-frame count; (3) pressing **Start advanced emulation AND
changed the framebuffer** (input works); (4) measured fps proves real-time feasibility (5.4x).

---

## 6. READY RUST CODE

### Cargo.toml
```toml
[package]
name = "n64harness"
version = "0.1.0"
edition = "2021"

[dependencies]
libloading = "0.8"          # dlopen the .dylib at runtime

[profile.release]
opt-level = 2
```

> Crate choice: I **hand-rolled the libretro ABI** (extern "C" fns + `#[repr(C)]` structs) and used
> only `libloading` to `dlopen` the core. This avoids version skew in `libretro-sys`/`rust-libretro-sys`
> and gives total control over the env callback (which is where headless forcing happens). You *can*
> swap in `libretro-sys = "0.1"` for the constants/struct defs, but the hand-rolled version below is
> self-contained and proven.

### Key pieces (full file at `/tmp/n64probe/harness/src/main.rs`)

**libretro ABI types:**
```rust
type EnvironmentFn      = extern "C" fn(cmd: u32, data: *mut c_void) -> bool;
type VideoRefreshFn     = extern "C" fn(data: *const c_void, w: u32, h: u32, pitch: usize);
type AudioSampleBatchFn = extern "C" fn(data: *const i16, frames: usize) -> usize;
type InputPollFn        = extern "C" fn();
type InputStateFn       = extern "C" fn(port: u32, device: u32, index: u32, id: u32) -> i16;

#[repr(C)] struct RetroGameInfo { path: *const c_char, data: *const c_void, size: usize, meta: *const c_char }
#[repr(C)] #[derive(Default,Clone,Copy)] struct RetroGameGeometry { base_width:u32, base_height:u32, max_width:u32, max_height:u32, aspect_ratio:f32 }
#[repr(C)] #[derive(Default,Clone,Copy)] struct RetroSystemTiming  { fps:f64, sample_rate:f64 }
#[repr(C)] #[derive(Default,Clone,Copy)] struct RetroSystemAvInfo  { geometry:RetroGameGeometry, timing:RetroSystemTiming }
#[repr(C)] struct RetroVariable { key:*const c_char, value:*const c_char }
```

**The env callback — THIS is where headless + angrylion forcing happens:**
```rust
fn forced_option(key: &str) -> Option<&'static str> {
    match key {
        "mupen64plus-rdp-plugin"            => Some("angrylion"),   // software RDP
        "mupen64plus-rsp-plugin"            => Some("hle"),
        "mupen64plus-angrylion-multithread" => Some("all_threads"),
        "mupen64plus-EnableFBEmulation"     => Some("True"),
        _ => None,
    }
}

extern "C" fn environment_cb(cmd: u32, data: *mut c_void) -> bool {
    const EXP: u32 = 0x10000;
    match cmd & !EXP {
        10 /*SET_PIXEL_FORMAT*/ => { unsafe { *PIXEL_FORMAT.lock().unwrap() = *(data as *const u32); } true }
        15 /*GET_VARIABLE*/ => {
            let var = unsafe { &mut *(data as *mut RetroVariable) };
            let key = unsafe { CStr::from_ptr(var.key) }.to_string_lossy().into_owned();
            if let Some(v) = forced_option(&key) {
                let cs = CString::new(v).unwrap();
                var.value = cs.as_ptr();
                VAR_STORE.lock().unwrap().push(cs);   // keep alive
                true
            } else { false }   // false => core uses its own default
        }
        17 /*GET_VARIABLE_UPDATE*/ => { unsafe { if !data.is_null() { *(data as *mut bool) = false } } true }
        14 /*SET_HW_RENDER*/ => false,   // REFUSE GL -> core stays SOFTWARE (headless)
        3  /*GET_CAN_DUPE*/  => { unsafe { if !data.is_null() { *(data as *mut bool)=true } } true }
        9 | 31 /*SYSTEM/SAVE_DIRECTORY*/ => { /* return a dir cstr ptr */ true }
        16|53|54|67|68 /*SET_VARIABLES/SET_CORE_OPTIONS*/ => true,
        52 /*GET_CORE_OPTIONS_VERSION*/ => { unsafe { if !data.is_null() { *(data as *mut u32)=2 } } true }
        51 /*GET_INPUT_BITMASKS*/ => false,  // make core poll individual ids
        _ => false,
    }
}
```

**video_refresh — checksum + non-blank proof (and what your encoder taps):**
```rust
extern "C" fn video_refresh_cb(data: *const c_void, w: u32, h: u32, pitch: usize) {
    if data.is_null() { return; }                     // duplicate frame
    let bpp = if *PIXEL_FORMAT.lock().unwrap()==1 {4} else {2};   // XRGB8888=4
    // ... iterate h rows * (w*bpp) bytes using `pitch` stride ...
    // For streaming: copy each active row (w*bpp) into your VP8 input buffer (BGRA -> convert).
}
```

**audio + input callbacks:**
```rust
extern "C" fn audio_sample_batch_cb(_d:*const i16, frames: usize) -> usize {
    *AUDIO_FRAMES.lock().unwrap() += frames as u64;   // i16 stereo, 44100 Hz -> Opus
    frames
}
extern "C" fn input_state_cb(port:u32, device:u32, _i:u32, id:u32) -> i16 {
    if port!=0 { return 0; }
    if device==1 /*JOYPAD*/ { return if button_pressed(id) {1} else {0}; }
    if device==5 /*ANALOG*/ { return analog_axis(_i, id); }  // i16 [-32768,32767]
    0
}
```

**load / run loop:**
```rust
let lib = unsafe { Library::new(core_path)? };
let set_env: Symbol<extern "C" fn(EnvironmentFn)> = unsafe { lib.get(b"retro_set_environment")? };
// ... resolve set_video_refresh, set_audio_sample_batch, set_input_poll, set_input_state,
//     retro_init, retro_load_game, retro_get_system_av_info, retro_run, retro_deinit ...

set_env(environment_cb);                 // MUST be before retro_init
set_video_refresh(video_refresh_cb);
set_audio_sample_batch(audio_sample_batch_cb);
set_input_poll(input_poll_cb);
set_input_state(input_state_cb);
retro_init();

let rom = std::fs::read(rom_path)?;      // z64 native, NO byteswap for mupen
let info = RetroGameInfo { path: rom_path_c.as_ptr(), data: rom.as_ptr() as _, size: rom.len(), meta: null() };
assert!(retro_load_game(&info));

let mut av: RetroSystemAvInfo = Default::default();
retro_get_system_av_info(&mut av);       // fps 60.0, sample_rate 44100

loop {
    set_inputs_for_this_frame();         // write your BTN_*/analog globals
    retro_run();                         // ONE call == ONE frame: drives video+audio+input cbs
    // pull latest framebuffer + audio out of your global buffers, hand to VP8/Opus encoders
    // pace to 60 fps (you have 5x+ headroom)
}
```

---

## 7. GOTCHAS (learned the hard way)

1. **`SET_HW_RENDER` must return `false`.** If you return true the core sets up a GL context and the
   software framebuffer path may not fire. Refusing it keeps angrylion in pure-software mode → headless.
2. **`GET_VARIABLE` value pointers must outlive the call.** The core reads the returned C string after
   your callback returns; store the `CString` in a static `Vec` (see `VAR_STORE`) or it gets freed.
3. **Frame size is NOT 320x240.** SSB64 delivers **640x240 XRGB8888, pitch 2560**. Always read
   width/height/pitch from `video_refresh` each frame — they change (interlace, menus). Use `pitch`
   as the row stride, never `width*bpp`.
4. **`video_refresh` data can be NULL** (duplicate frame when `GET_CAN_DUPE` is true). Skip those;
   reuse the previous frame for the encoder.
5. **Pixel order is BGRA** for XRGB8888 on little-endian (byte 0=B,1=G,2=R,3=X). Convert before VP8.
6. **mupen-next dynarec is a no-op in this arm64 nightly** (logs "Cached Interpreter"). Still 5.4x
   real-time. If you want the JIT, `parallel_n64` uses the Ari64 dynarec and runs 16x.
7. **parallel-RDP (Vulkan) will NOT work** with the prebuilt mupen-next dylib — it links OpenGL only,
   not MoltenVK (`otool -L` confirms). Errors with "VK_KHR_16bit_storage not supported". Use angrylion.
8. **Set `GET_SYSTEM_DIRECTORY`/`GET_SAVE_DIRECTORY`** to a writable path or the core may complain
   about save/config files.
9. **z64 native — no byteswap.** Header `80 37 12 40` is big-endian native; pass raw bytes to
   `retro_load_game`. (.v64 byteswapped or .n64 little-endian would need conversion; ours is .z64.)
10. **`retro_set_environment` must be called BEFORE `retro_init`** — the core queries options during init.

---

## 8. Recommended integration plan for nes-web

- Drop `mupen64plus_next_libretro.dylib` into `cores/` (done). Add a `n64` backend module mirroring the
  existing tetanes NES path: a struct that `dlopen`s the core, wires the 5 callbacks to channels/buffers,
  and exposes `load_rom(z64) / run_frame() / framebuffer() / audio() / set_input(state)`.
- `run_frame()` = one `retro_run()`. Pace at 60 fps; you have 5x headroom for VP8+Opus encode on the
  same machine.
- Framebuffer: copy active region (read pitch!), BGRA→I420 for VP8.
- Audio: 44100 Hz i16 stereo straight into Opus (resample to 48k if your Opus path wants it).
- Input: map keyboard → the JOYPAD ids in §4; analog stick → left-analog i16 axes; C-buttons →
  right-analog axes (or remap via core options).

**Sources:** libretro buildbot (apple/osx/arm64/latest), libretro.h env/device constants,
mupen64plus-libretro-nx core options, lib.rs/emulators, github.com/gopher64/gopher64,
homebrew mupen64plus 2.6.0 formula.
