# N64 headless on macOS arm64 — mupen64plus-next + angrylion (SOFTWARE RDP), driven from Rust

**Status: WORKING. SSB64 boots headless, software-rendered, non-blank, input-driven, ~384 fps.**

This is the PRIMARY/decider path. It produces a real CPU framebuffer with **no window, no display,
no GL/Vulkan context** — exactly what a server-side WebRTC streamer needs. Drop-in alongside the
existing tetanes NES path.

---

## TL;DR / Decision

- Core: **`mupen64plus_next_libretro.dylib`** (arm64), libretro frontend via **`libloading`** + hand-rolled
  `extern "C"` callbacks. No HW context needed.
- Force core options: **`mupen64plus-rdp-plugin = angrylion`** (software RDP) +
  **`mupen64plus-rsp-plugin = cxd4`** (or `hle`). Pixel format **XRGB8888**.
- **CRITICAL GOTCHA (root cause of a SIGSEGV):** you MUST satisfy
  `RETRO_ENVIRONMENT_GET_LOG_INTERFACE` with a **real C-variadic** log function pointer. If you decline
  it (return false), the core stores a NULL fn-ptr and later `blr`s through it → instant SIGSEGV inside
  `retro_load_game`. Rust can't write C-variadic fns on stable, so compile a tiny `logshim.c`.
- angrylion does **NOT** request `SET_HW_RENDER`. `parallel` and `gliden64` DO (and we refuse → load
  returns false cleanly). So angrylion is the only headless-viable RDP in this build — and it works.
- Working core copied to `~/pokemon-pvp-red/cores/mupen64plus_next_libretro.dylib`.

---

## REAL PROOF OUTPUT (canonical clean run)

Command:
```
PROBE_RDP=angrylion PROBE_RSP=cxd4 PROBE_MT="all threads" \
  RUSTUP_TOOLCHAIN=1.92 ./target/release/n64probe
```

```
=== N64 mupen64plus-next headless probe ===
core: /tmp/n64probe/mupen64plus_next_libretro.dylib
rom:  ~/pokemon-pvp-red/Super Smash Bros. (U) [!].z64
retro_api_version = 1
library: Mupen64Plus-Next 2.8-Vulkan 98c1b0d | exts: n64|v64|z64|bin|u1 | need_fullpath: false
rom size = 16777216 bytes
retro_load_game -> true
av_info: geom base 640x480 max 640x480 aspect 1.333 | fps 60.0000 sample_rate 44100.0
--------------------------------------------------
HW_RENDER requested by core: false
FRAMEBUFFER after 150 frames: 640x240 pitch=2560 fmt=1 (XRGB8888)
  bytes captured: 614400 | checksum(FNV1a)=0x54a8ab43f4ab244b | nonzero bytes: 214980 / 614400
AUDIO: 108168 stereo frames seen | rate 44100.0 Hz | channels 2 (i16)
PERF: 150 frames in 0.390s => 384.3 fps (target 60.00)
--------------------------------------------------
AFTER pressing START (+120 frames): checksum=0x205a61e4ee08bfbc | nonzero 562865 | changed_vs_before: true
AFTER analog-right + A (+60 frames): checksum=0xa6336622d0ed5cb3 | changed_vs_B: true
=== done ===
```

Interpretation of the proof:
- `retro_load_game -> true` — SSB64 (USA, .z64 native, MD5 F7C52568A31AADF26E14DC2B6416B2ED, CRC 916B8B5B)
  accepted directly, no byte-swap.
- **Framebuffer is non-blank**: 214,980 / 614,400 bytes nonzero on the title screen; after Start+A it
  jumps to 562,865 nonzero (menu/VS screen fills more of the frame).
- **Input works / emulation advances**: pressing Start changes the FNV-1a checksum
  (`0x54a8ab43f4ab244b` → `0x205a61e4ee08bfbc`); analog-right + A changes it again
  (`→ 0xa6336622d0ed5cb3`).
- **Audio**: 108,168 stereo frames over 150 video frames @ 44100 Hz (= ~720 samples/frame, correct for
  60fps×44.1kHz), i16 stereo via `audio_sample_batch`.
- **Real-time feasibility**: 384 fps single-threaded harness on this M-series machine — ~6.4× real time
  with angrylion "all threads". Massive headroom for the encode+WebRTC pipeline.

### Visual confirmation (decisive)
The captured XRGB8888 buffer was dumped to PPM/PNG and is the **Super Smash Bros. title screen** — the
"SUPER SMASH BROS." logo with legible "©1999 NINTENDO/HAL Laboratory" copyright text. Saved at:
`~/pokemon-pvp-red/research/ssb64-titlescreen.png`. This is unmistakably a correctly
emulated, software-rendered N64 frame, not noise or a test pattern (51,191 distinct colors in the frame).

---

## Framebuffer / audio / input contract (what to wire into the streamer)

| Item | Value |
|---|---|
| Pixel format | **XRGB8888** (RETRO_PIXEL_FORMAT = 1). In memory little-endian: bytes are `B,G,R,X` per pixel. |
| video_refresh dims | width=**640**, height=**240**, **pitch=2560** bytes (= 640×4). Note height 240, NOT 480 — angrylion outputs a half-height (non-interlaced) VI frame for SSB64; `av_info.geometry` reports 640×480 max but the actual `video_refresh` is 640×240. Always trust the callback's `width/height/pitch`, not av_info. |
| fps (timing) | `av_info.timing.fps = 60.0` (NTSC). Pace `retro_run()` at this. |
| Audio | `av_info.timing.sample_rate = 44100.0`, **i16 interleaved stereo** via `audio_sample_batch(const int16_t*, frames)`. ~720 frames per video frame. |
| Dupe frames | Core uses GET_CAN_DUPE: `video_refresh` may be called with `data == NULL` meaning "same as last frame" — keep your previous buffer. |

### Input mapping (N64 controller → libretro)
Port 0, device `RETRO_DEVICE_JOYPAD` (1) for buttons + `RETRO_DEVICE_ANALOG` (5) for the stick.
Call `retro_set_controller_port_device(0, RETRO_DEVICE_JOYPAD)` after load. Your `input_state`
callback returns per-id state:

| N64 button | libretro id (JOYPAD) |
|---|---|
| A | `RETRO_DEVICE_ID_JOYPAD_A` = 8 |
| B | `RETRO_DEVICE_ID_JOYPAD_B` = 0 |
| Start | `RETRO_DEVICE_ID_JOYPAD_START` = 3 |
| Z (trigger) | mapped to `RETRO_DEVICE_ID_JOYPAD_L2` = 12 (mupen-next default) — also commonly `L`=10. Test both; default core map: Z=L2. |
| L | `RETRO_DEVICE_ID_JOYPAD_L` = 10 |
| R | `RETRO_DEVICE_ID_JOYPAD_R` = 11 |
| D-pad U/D/L/R | 4 / 5 / 6 / 7 |
| C-up/down/left/right | core options `mupen64plus-{u,d,l,r}-cbutton` map these onto the **right analog stick** by default (RETRO_DEVICE_ANALOG index RIGHT=1), OR you can use the joypad face buttons depending on `alt-map`. Simplest: set `mupen64plus-u/d/l/r-cbutton` to face buttons, OR drive C via the right analog stick (ANALOG index 1). |

Analog stick: device `RETRO_DEVICE_ANALOG`, index `RETRO_DEVICE_INDEX_ANALOG_LEFT` (0), ids
`RETRO_DEVICE_ID_ANALOG_X` (0) / `_Y` (1), value range **-32768..32767** (i16). We verified analog-X=30000
moves things on screen.

---

## How the core was acquired

```bash
curl -sL "https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/mupen64plus_next_libretro.dylib.zip" -o mupen.zip
unzip -o mupen.zip          # -> mupen64plus_next_libretro.dylib  (7.0 MB, Mach-O arm64)
# (parallel_n64_libretro.dylib.zip is also there as a backup core)
```
Verify it's arm64 and has the symbols:
```bash
file mupen64plus_next_libretro.dylib   # Mach-O 64-bit dynamically linked shared library arm64
otool -L mupen64plus_next_libretro.dylib
#   links: AudioToolbox, OpenGL.framework, libSystem, libc++  — all system, NO missing deps
nm -g mupen64plus_next_libretro.dylib | grep _retro_   # all retro_* symbols present
```
`retro_get_system_info` reports: **`Mupen64Plus-Next 2.8-Vulkan 98c1b0d`**, valid exts `n64|v64|z64|bin|u1`,
`need_fullpath = false` (so we pass ROM bytes in-memory; passing the path too is harmless).

The OpenGL.framework link is for the HW (parallel/gliden64) plugins; angrylion does not use it.

---

## The headless render path (WHAT ACTUALLY HAPPENS)

We tested 4 RDP configs by forcing `mupen64plus-rdp-plugin`:

| rdp-plugin | rsp-plugin | SET_HW_RENDER? | retro_load_game | Result |
|---|---|---|---|---|
| `angrylion` | `cxd4` | **No** | **true** | **WORKS — software framebuffer via video_refresh** ✅ |
| `angrylion` | `hle` | No | true | Works too (cxd4 is the accurate RSP; hle faster) |
| `parallel` (default) | `parallel` | **Yes (cmd 14)** | false (we refused) | Vulkan HW core — needs a GL/Vulkan context, NOT headless-friendly |
| `gliden64` | `hle` | **Yes (cmd 14)** | false (we refused) | GL HW core — needs a GL context |

Conclusion: **angrylion is pure software and never asks for a HW context** → `video_refresh` delivers a
CPU XRGB8888 buffer with `glReadPixels`/FBO/CGL NOT required. The CGL-offscreen fallback in the brief is
**unnecessary** for this path. (If you ever wanted GLideN64 perf, you'd need the CGL+FBO offscreen route;
but for a server that is the wrong trade — angrylion at 384fps is plenty.)

---

## GOTCHAS (the load-bearing ones)

1. **GET_LOG_INTERFACE must return a real variadic fn-ptr.** This is THE bug that cost a SIGSEGV.
   `retro_load_game` ends with `ldr x8,[ctx,#0xc88]; mov w0,#0; blr x8` — that's the core calling its
   stored `retro_log_printf_t(level, fmt, ...)`. If you declined the log interface, `[ctx+0xc88]` is NULL
   and you crash at `0x0`. Rust stable can't define C-variadic fns (`error[E0658]: C-variadic functions
   are unstable`), so we compile a 6-line `logshim.c` (`vfprintf`) and hand the core its address. After
   this, load succeeds and you see the core's own log (ROM header, "M64CMD_EXECUTE", "plugin_start_gfx").

2. **angrylion-multithread valid values** are `"all threads"` or a thread-count string — NOT `"1"`
   blindly. Use `"all threads"`. `angrylion-sync` ∈ {`Low`,`Medium`,`High`}.

3. **Option keys & values** (mupen64plus-next 2.8): `mupen64plus-rdp-plugin` ∈
   {`parallel`(default),`angrylion`,`gliden64`}; `mupen64plus-rsp-plugin` ∈ {`hle`,`cxd4`,`parallel`}.
   The default rdp is `parallel` (Vulkan) — you MUST override to `angrylion` for headless.

4. **video_refresh height ≠ av_info height.** av_info says 640×480; the actual callback is 640×240 for
   SSB64. Use the callback's dims/pitch for your encoder; if your VP8 encoder wants 480 lines, either
   line-double or just encode the native 640×240.

5. **GET_VARIABLE is polled in a flood** (hundreds of calls during load + once per `retro_run`). Answer
   from a static map; don't allocate per call in hot paths in production (we leak a CString per forced
   key during load — fine for load, but cache it for steady-state).

6. **need_fullpath=false** → pass ROM bytes via `retro_game_info.data/size`. We also set `.path` (helps
   the core derive save/SRAM filenames). ROM has spaces in its name; quote it.

7. **Provide system + save directories** (GET_SYSTEM_DIRECTORY=9, GET_SAVE_DIRECTORY=31). mupen-next reads
   them; returning valid existing dirs avoids surprises (we used `/tmp/n64probe/{systemdir,savedir}`).
   No external BIOS/data file is required for angrylion + SSB64 (stock cart).

8. **Refuse SET_HW_RENDER (return false).** With angrylion it's never called; but returning false is the
   correct "I am a software-only frontend" signal and makes parallel/gliden64 fail fast instead of
   crashing.

9. **Toolchain:** build with the project's pinned `1.92` (`RUSTUP_TOOLCHAIN=1.92`). `libloading 0.8` and
   a C build via `clang` are the only build deps.

---

## READY RUST CODE

The complete, runnable harness is preserved at
`~/pokemon-pvp-red/research/n64-harness/` (Cargo.toml, build.rs, logshim.c,
src/main.rs). Build/run:
```bash
cd ~/pokemon-pvp-red/research/n64-harness
RUSTUP_TOOLCHAIN=1.92 cargo build --release
PROBE_RDP=angrylion PROBE_RSP=cxd4 PROBE_MT="all threads" \
  RUSTUP_TOOLCHAIN=1.92 ./target/release/n64probe \
  ../../cores/mupen64plus_next_libretro.dylib \
  "../../Super Smash Bros. (U) [!].z64"
```

### Crates / deps
```toml
[dependencies]
libloading = "0.8"   # only crate needed (no rust-libretro-sys required; hand-rolled is cleaner here)
# build.rs compiles logshim.c via clang and links it statically.
```
(`libretro-sys` / `rust-libretro-sys` also work for the struct/const definitions, but they do NOT solve
the C-variadic log-callback problem — you still need the C shim. Hand-rolling the ~30 structs/consts we
actually touch is less friction than pulling a crate and fighting its env-callback signature.)

### logshim.c (THE fix for the SIGSEGV)
```c
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
// retro_log_printf_t : void (*)(enum retro_log_level level, const char *fmt, ...)
void n64probe_log(int level, const char *fmt, ...) {
    if (!getenv("PROBE_VERBOSE")) return;     // silence unless debugging
    va_list ap; va_start(ap, fmt);
    fprintf(stderr, "[core log %d] ", level);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
}
```

### build.rs
```rust
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let ok = std::process::Command::new("clang")
        .args(["-O2","-arch","arm64","-c","logshim.c","-o"])
        .arg(format!("{}/logshim.o", out)).status().unwrap();
    assert!(ok.success());
    let ok = std::process::Command::new("ar")
        .args(["crs"]).arg(format!("{}/liblogshim.a", out))
        .arg(format!("{}/logshim.o", out)).status().unwrap();
    assert!(ok.success());
    println!("cargo:rustc-link-search=native={}", out);
    println!("cargo:rustc-link-lib=static=logshim");
    println!("cargo:rerun-if-changed=logshim.c");
}
```

### Environment callback (the heart of it — abridged; full version in src/main.rs)
```rust
unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        RETRO_ENVIRONMENT_GET_CAN_DUPE => { *(data as *mut bool) = true; true }   // 3

        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {                                    // 10
            let fmt = *(data as *const c_uint);            // core sends XRGB8888=1
            STATE.lock().unwrap().pixfmt = fmt;
            fmt == 0 || fmt == 1 || fmt == 2               // accept known formats
        }

        RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY |                                   // 9
        RETRO_ENVIRONMENT_GET_CORE_ASSETS_DIRECTORY |                             // 30
        RETRO_ENVIRONMENT_GET_LIBRETRO_PATH    => { *(data as *mut *const c_char)=SYS_DIR; true }
        RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY   => { *(data as *mut *const c_char)=SAVE_DIR; true } // 31

        RETRO_ENVIRONMENT_GET_VARIABLE => {                                        // 15
            let var = &mut *(data as *mut retro_variable);
            let key = CStr::from_ptr(var.key).to_string_lossy().to_string();
            match forced_option(&key) {                    // forces rdp=angrylion, rsp=cxd4, ...
                Some(v) => { var.value = CString::new(v).unwrap().into_raw(); true }
                None    => { var.value = ptr::null(); false }
            }
        }
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => { *(data as *mut bool)=false; true } // 17
        RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => { *(data as *mut c_uint)=2; true } // 52
        // ack the option-registration cmds; our GET_VARIABLE does the forcing:
        16 | 53 | 54 | 67 | 68 | 55 => true,

        RETRO_ENVIRONMENT_SET_HW_RENDER => {               // 14  -> REFUSE (force software)
            STATE.lock().unwrap().hw_render_requested = true; false
        }
        56 /*GET_PREFERRED_HW_RENDER*/ => { *(data as *mut c_uint)=0 /*NONE*/; true }

        RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {           // 27  *** MUST satisfy ***
            extern "C" { fn n64probe_log(); }
            #[repr(C)] struct LogCb { log: *const c_void }
            (*(data as *mut LogCb)).log = n64probe_log as *const c_void;
            true
        }

        RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => {          // 32
            let av = &*(data as *const retro_system_av_info);
            STATE.lock().unwrap().sample_rate = av.timing.sample_rate; true
        }
        // benign acks:
        32|37|8|18|11|35|34|1|6 => true,
        _ => false,
    }
}

fn forced_option(key: &str) -> Option<String> {
    Some(match key {
        "mupen64plus-rdp-plugin"            => "angrylion",            // SOFTWARE RDP
        "mupen64plus-rsp-plugin"            => "cxd4",                 // or "hle"
        "mupen64plus-angrylion-multithread" => "all threads",
        "mupen64plus-angrylion-sync"        => "Low",
        "mupen64plus-EnableFBEmulation"     => "True",
        "mupen64plus-43screensize"          => "320x240",
        "mupen64plus-aspect"                => "4:3",
        "mupen64plus-cpucore"               => "dynamic_recompiler",
        _ => return None,
    }.into())
}
```

### video_refresh / audio / input callbacks
```rust
unsafe extern "C" fn cb_video_refresh(data:*const c_void, w:c_uint, h:c_uint, pitch:usize) {
    let mut st = STATE.lock().unwrap();
    st.width=w; st.height=h; st.pitch=pitch;
    if data.is_null() { return; }                          // dupe frame: keep last
    let bytes = pitch * h as usize;
    let slice = std::slice::from_raw_parts(data as *const u8, bytes);
    st.last_frame.clear(); st.last_frame.extend_from_slice(slice);
    // --> in the streamer: convert XRGB8888 (B,G,R,X) -> I420 and feed vpx-encode here.
}

unsafe extern "C" fn cb_audio_sample_batch(data:*const i16, frames:usize) -> usize {
    // `data` is interleaved i16 L,R,L,R...  length = frames*2 samples.
    // --> in the streamer: push to the Opus encoder ring buffer.
    STATE.lock().unwrap().audio_samples += frames as u64;
    frames
}

unsafe extern "C" fn cb_input_state(_port:c_uint, device:c_uint, index:c_uint, id:c_uint) -> i16 {
    let inp = INPUT.lock().unwrap();
    match device {
        1 /*JOYPAD*/ => (id < 16 && inp.joypad[id as usize]) as i16,
        5 /*ANALOG*/ if index==0 /*LEFT*/ => match id { 0=>inp.analog_x, 1=>inp.analog_y, _=>0 },
        _ => 0,
    }
}
```

### Load / run loop
```rust
let lib = libloading::Library::new(core_path)?;
let retro_set_environment: Symbol<unsafe extern "C" fn(EnvCb)>      = lib.get(b"retro_set_environment")?;
// ... resolve set_video_refresh / set_audio_sample / set_audio_sample_batch /
//     set_input_poll / set_input_state / init / load_game / get_system_av_info /
//     run / set_controller_port_device / unload_game / deinit ...

retro_set_environment(cb_environment);     // FIRST, before init
retro_set_video_refresh(cb_video_refresh);
retro_set_audio_sample(cb_audio_sample);
retro_set_audio_sample_batch(cb_audio_sample_batch);
retro_set_input_poll(cb_input_poll);
retro_set_input_state(cb_input_state);
retro_init();

let rom = std::fs::read(rom_path)?;        // need_fullpath=false -> pass bytes
let path_c = CString::new(rom_path)?;
let gi = retro_game_info { path: path_c.as_ptr(), data: rom.as_ptr().cast(),
                           size: rom.len(), meta: ptr::null() };
assert!(retro_load_game(&gi));             // -> true

let mut av: retro_system_av_info = std::mem::zeroed();
retro_get_system_av_info(&mut av);         // fps=60, sample_rate=44100
retro_set_controller_port_device(0, 1 /*JOYPAD*/);

loop {                                      // pace at av.timing.fps (60)
    // set INPUT.* from your data-channel keymap here
    retro_run();                            // fires video_refresh + audio_sample_batch
    // grab STATE.last_frame -> VP8;  audio -> Opus
}
```

### libretro structs used (repr C)
```rust
#[repr(C)] struct retro_game_geometry { base_width:u32, base_height:u32, max_width:u32, max_height:u32, aspect_ratio:f32 }
#[repr(C)] struct retro_system_timing  { fps:f64, sample_rate:f64 }
#[repr(C)] struct retro_system_av_info { geometry:retro_game_geometry, timing:retro_system_timing }
#[repr(C)] struct retro_game_info      { path:*const c_char, data:*const c_void, size:usize, meta:*const c_char }
#[repr(C)] struct retro_variable       { key:*const c_char, value:*const c_char }
#[repr(C)] struct retro_system_info    { library_name:*const c_char, library_version:*const c_char,
                                         valid_extensions:*const c_char, need_fullpath:bool, block_extract:bool }
```

---

## Integration notes for the WebRTC streamer

- **Pixel conversion:** XRGB8888 here is little-endian `B,G,R,X`. Your existing tetanes path produces RGBA
  for VP8; write a tiny BGRX→I420 (or BGRX→RGBA) shim. Frame is 640×240; either encode native or
  line-double to 640×480.
- **Threading:** run the core on a dedicated blocking thread (it spawns its own EmuThread + angrylion
  worker threads). Pace with the 60 Hz clock; push frames/audio over channels to the encode tasks. At
  ~384 fps capacity you can comfortably hit 60 fps real-time and still encode.
- **Audio rate:** 44100 Hz stereo i16 — matches Opus nicely (resample to 48k if your Opus encoder is
  fixed at 48k; the NES path already does Opus so reuse that).
- **Single static core, two emulators:** keep tetanes for NES; load this dylib for N64. The harness is
  self-contained (one extra crate `libloading` + the clang-compiled `logshim`).

## Files produced
- Working core: `~/pokemon-pvp-red/cores/mupen64plus_next_libretro.dylib`
- Backup core:  `~/pokemon-pvp-red/cores/parallel_n64_libretro.dylib` (Vulkan HW — needs GL/Vulkan context, NOT used)
- Harness source: `~/pokemon-pvp-red/research/n64-harness/` (Cargo.toml, build.rs, logshim.c, src/main.rs)
- Proof image: `~/pokemon-pvp-red/research/ssb64-titlescreen.png`
