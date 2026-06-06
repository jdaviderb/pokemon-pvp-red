# DESIGN-N64 — Headless Super Smash Bros. 64 over WebRTC (server-side)

Concrete, copy-pasteable plan to swap the NES (tetanes) backend for a **headless N64**
backend that streams **Super Smash Bros. (U)** as VP8 + Opus over the existing WebRTC
pipeline. Everything below reuses code the three probes **actually ran and verified**.

> **Status: FEASIBLE — proven.** All three probes booted SSB64 headless (no window, no GL
> context), captured a non-blank, input-responsive XRGB8888 framebuffer via the libretro
> `video_refresh` callback, and ran multiple times real-time. Proof images:
> `research/ssb64-boot-headless.png` (parallel_n64) and `research/ssb64-titlescreen.png`
> (mupen64plus-next). Working cores are already in `cores/`.

---

## 0. Reconciling the three probes (why this design picks what it picks)

| Probe doc | Core it championed | SSB64 headless? | fps | Friction |
|---|---|---|---|---|
| `n64-parallel-gl.md` | **parallel_n64 + angrylion** | YES, non-blank, input works | **~1400** | **NONE** — pure `dlopen`, no C shim, no GL |
| `n64-mupen-next.md` | mupen64plus-next + angrylion | YES, title screen w/ legible logo | ~384 | needs `logshim.c` (C-variadic log shim) or SIGSEGV |
| `n64-fallbacks-perf.md` | mupen64plus-next + angrylion | YES, non-blank, input works | ~327 | needs system/save dirs; dynarec is a no-op on this build |

The `n64-parallel-gl.md` harness **crashed** mupen-next — but `n64-mupen-next.md` found the
root cause: mupen-next calls a stored log function pointer during `retro_load_game`; if you
decline `GET_LOG_INTERFACE` that pointer is NULL → SIGSEGV. With a real C-variadic log shim,
mupen-next works fine. So **both cores work**; the difference is purely integration cost.

**Decision: ship `parallel_n64` as the primary core.** Rationale:
- **Simplest integration** (the explicit tie-breaker in the brief): it `dlopen`s with plain
  `libc`/`libloading`, needs **no `logshim.c`, no build.rs C step**, and never hits the
  null-log-ptr crash class. Its env callback can decline `GET_LOG_INTERFACE` safely.
- **Fastest** (~1400 fps / ~23× real-time vs ~384): the Ari64 dynarec engages on arm64,
  giving the most headroom for VP8+Opus encode on the same box.
- **No GL/Cocoa/Vulkan load commands** (`otool -L` shows none) → unambiguously software-only.
- Verified non-blank + input-responsive in `n64-parallel-gl.md` (boot logo, 1036 colors;
  Start press changed the checksum and nonzero-byte count).

**mupen64plus-next + `logshim.c` is the documented fallback** (Plan B in §10): it is the more
actively-maintained core and gave the crispest visual proof (51,191-color title screen). The
`n64.rs` module is written so switching cores is a 3-line change (core path + the
`forced_option` table + add the log-interface arm). Both cores share the identical libretro
ABI, callback shapes, XRGB8888 640×240 output, and 44100 Hz i16 stereo audio.

---

## 1. Final approach

- **Core:** `cores/parallel_n64_libretro.dylib` (ParaLLEl N64 v2.14.6, arm64, adhoc-signed),
  already downloaded. Forced to its **angrylion software RDP** + **cxd4 (or hle) software RSP**
  plugins via libretro core-option forcing. **No OpenGL/Vulkan/Metal context is created** — the
  frontend refuses `RETRO_ENVIRONMENT_SET_HW_RENDER`, so the core stays in pure-software mode and
  emits CPU framebuffers through `video_refresh`.
- **Renderer:** angrylion (software RDP). It outputs the VI signal as **XRGB8888, 640×240,
  pitch 2560** for SSB64 (memory byte order **B, G, R, X** little-endian). No `glReadPixels`,
  no FBO, no CGL.
- **Dylib bundling:** the core lives in `cores/` (committed/copied). At runtime `n64.rs`
  `dlopen`s it by absolute path (`std::env::current_dir()` + `cores/...` or a CLI override).
  The buildbot zips set no quarantine via `curl`; if a `com.apple.quarantine` xattr ever
  appears (Safari download, app bundle), strip it: `xattr -d com.apple.quarantine cores/*.dylib`.
  The only present xattr is `com.apple.provenance`, which does **not** block `dlopen`.
- **GL-context setup:** **none.** This is the whole point of choosing angrylion. (The CGL
  offscreen recipe in `n64-parallel-gl.md` §"CGL fallback" is kept only for the GL-core Plan B.)
- **Acquire (already done; reproduce if needed):**
  ```bash
  BB=https://buildbot.libretro.com/nightly/apple/osx/arm64/latest
  curl -sL -o parallel_n64.dylib.zip "$BB/parallel_n64_libretro.dylib.zip"
  unzip -o parallel_n64.dylib.zip -d cores/      # -> cores/parallel_n64_libretro.dylib
  # fallback core:
  curl -sL -o mupen.zip "$BB/mupen64plus_next_libretro.dylib.zip"
  unzip -o mupen.zip -d cores/                   # -> cores/mupen64plus_next_libretro.dylib
  ```

---

## 2. New module: `src/n64.rs`

Drop-in replacement for `src/emu.rs`. It exposes the **same shape the pipeline needs**:

- `N64::new(core_path, rom_path) -> Result<Self>` — load core, wire 6 callbacks, force
  angrylion, load the .z64, return `(fps, sample_rate)` to the caller via fields.
- `clock_frame()` — one `retro_run()` (one video frame; fires video/audio/input callbacks).
- `frame_buffer() -> Frame` — latest XRGB8888 bytes + `w/h/pitch` (read every frame; they can
  change). The pipeline converts to I420.
- `audio_drain() -> Vec<i16>` — interleaved L,R @ core rate (44100), drained each frame.
- `set_button(player, N64Button, pressed)` / `set_axis(player, axis, value)` — input.

### Global-callback strategy

libretro callbacks are bare `extern "C" fn` (no user-data pointer), so per-instance buffers
must live in **process globals**. We run exactly **one** emulator on **one** dedicated OS
thread (same model as today), so a single set of `static Mutex<...>` globals is correct and
race-free in practice. We use `Mutex` (not `thread_local!`) because the WebRTC writer/timer
logic and the emu thread are distinct, and because the core spawns its own worker threads
(angrylion "all threads") that call `video_refresh`/`audio_batch` — those must reach the same
buffers. Locks are held only for a `memcpy`/`extend`, never across `retro_run()`.

Directory C-strings (`SYS_DIR`/`SAVE_DIR`) and forced-option values are leaked `CString`s
(valid for process lifetime) exactly as the probes did.

```rust
//! src/n64.rs — headless libretro frontend for N64 (ParaLLEl N64 core + angrylion software RDP).
//!
//! VERIFIED (research/n64-parallel-gl.md, research/n64-fallbacks-perf.md): SSB64 (U) boots
//! headless with NO window/GL context, emits XRGB8888 640x240 (pitch 2560, byte order B,G,R,X)
//! via video_refresh, i16 stereo @44100 via audio_sample_batch, ~1400 fps on Apple Silicon.
//!
//! libretro callbacks are global extern "C" fns with no user-data; we run ONE emulator on ONE
//! dedicated thread, so per-instance buffers live in `static Mutex<...>` globals (FRAME/AUDIO/PAD).

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

use libloading::{Library, Symbol};

// ---------- libretro env command ids (only the ones we answer) ----------
const ENV_SET_ROTATION: c_uint = 1;
const ENV_GET_CAN_DUPE: c_uint = 3;
const ENV_SET_MESSAGE: c_uint = 6;
const ENV_SET_PERFORMANCE_LEVEL: c_uint = 8;
const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;
const ENV_SET_INPUT_DESCRIPTORS: c_uint = 11;
const ENV_SET_HW_RENDER: c_uint = 14; // REFUSE -> stay software (the key decision)
const ENV_GET_VARIABLE: c_uint = 15;
const ENV_SET_VARIABLES: c_uint = 16;
const ENV_GET_VARIABLE_UPDATE: c_uint = 17;
const ENV_SET_SUPPORT_NO_GAME: c_uint = 18;
const ENV_GET_LIBRETRO_PATH: c_uint = 19;
const ENV_GET_CORE_ASSETS_DIRECTORY: c_uint = 30;
const ENV_GET_SAVE_DIRECTORY: c_uint = 31;
const ENV_SET_SYSTEM_AV_INFO: c_uint = 32;
const ENV_SET_SUBSYSTEM_INFO: c_uint = 34;
const ENV_SET_CONTROLLER_INFO: c_uint = 35;
const ENV_SET_GEOMETRY: c_uint = 37;
const ENV_GET_CORE_OPTIONS_VERSION: c_uint = 52;
const ENV_SET_CORE_OPTIONS: c_uint = 53;
const ENV_SET_CORE_OPTIONS_INTL: c_uint = 54;
const ENV_SET_CORE_OPTIONS_DISPLAY: c_uint = 55;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_EXPERIMENTAL: c_uint = 0x10000;
const ENV_GET_INPUT_BITMASKS: c_uint = 51 | 0x10000;

const PIXFMT_0RGB1555: c_uint = 0;
const PIXFMT_XRGB8888: c_uint = 1;
const PIXFMT_RGB565: c_uint = 2;
const HW_CONTEXT_NONE: c_uint = 0;

// ---------- libretro devices + button/axis ids ----------
const DEV_JOYPAD: c_uint = 1;
const DEV_ANALOG: c_uint = 5;
const ANALOG_LEFT: c_uint = 0;
const ANALOG_RIGHT: c_uint = 1; // C-buttons map to the right stick on N64 cores
const ANALOG_X: c_uint = 0;
const ANALOG_Y: c_uint = 1;

// RETRO_DEVICE_ID_JOYPAD_*  (index into PAD.btn)
pub const ID_B: usize = 0;
pub const ID_Y: usize = 1;
pub const ID_SELECT: usize = 2;
pub const ID_START: usize = 3;
pub const ID_UP: usize = 4;
pub const ID_DOWN: usize = 5;
pub const ID_LEFT: usize = 6;
pub const ID_RIGHT: usize = 7;
pub const ID_A: usize = 8;
pub const ID_X: usize = 9;
pub const ID_L: usize = 10;
pub const ID_R: usize = 11;
pub const ID_L2: usize = 12; // mupen-next default Z
pub const ID_R2: usize = 13;

pub const AXIS_MAX: i16 = 0x7FFF; // full-deflection for analog stick / C-stick

// ---------- libretro structs (repr C) ----------
#[repr(C)]
struct RetroSystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RetroGameGeometry {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RetroSystemTiming {
    fps: f64,
    sample_rate: f64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RetroSystemAvInfo {
    geometry: RetroGameGeometry,
    timing: RetroSystemTiming,
}
#[repr(C)]
struct RetroGameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}
#[repr(C)]
struct RetroVariable {
    key: *const c_char,
    value: *const c_char,
}

// ---------- shared globals (single-emulator, single thread) ----------
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub pitch: usize,
    pub fmt: u32, // 1 == XRGB8888
    pub bytes: Vec<u8>,
}
static FRAME: Mutex<Frame> = Mutex::new(Frame {
    w: 0,
    h: 0,
    pitch: 0,
    fmt: PIXFMT_XRGB8888,
    bytes: Vec::new(),
});
// interleaved L,R i16 @ core sample rate; drained every frame by the pipeline.
static AUDIO: Mutex<Vec<i16>> = Mutex::new(Vec::new());

pub struct Pad {
    pub btn: [bool; 16],
    pub lx: i16, // control stick X (+ right)
    pub ly: i16, // control stick Y (+ down)
    pub cx: i16, // C-stick X (right analog)  -> C-left/right
    pub cy: i16, // C-stick Y (right analog)  -> C-up/down
}
static PAD: Mutex<Pad> = Mutex::new(Pad {
    btn: [false; 16],
    lx: 0,
    ly: 0,
    cx: 0,
    cy: 0,
});

// leaked CString pointers (valid for the whole process)
static mut SYS_DIR: *const c_char = ptr::null();
static mut SAVE_DIR: *const c_char = ptr::null();

// ---------- forced core options (ParaLLEl N64) ----------
// Switching to mupen64plus-next? Replace this table with the mupen keys in §10 AND add a
// GET_LOG_INTERFACE arm to `environment` (see §10), then point N64::new at the mupen dylib.
fn forced_option(key: &str) -> Option<&'static str> {
    match key {
        "parallel-n64-gfxplugin" => Some("angrylion"), // SOFTWARE RDP (the whole game)
        "parallel-n64-rspplugin" => Some("cxd4"),       // accurate LLE RSP; "hle" is faster
        "parallel-n64-screensize" => Some("320x240"),
        "parallel-n64-angrylion-vioverlay" => Some("Filtered"),
        "parallel-n64-cpucore" => Some("dynamic_recompiler"), // Ari64 dynarec -> ~16x real time
        _ => None,
    }
}

// ---------- callbacks ----------
extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    unsafe {
        match cmd {
            ENV_GET_CAN_DUPE => {
                if !data.is_null() {
                    *(data as *mut bool) = true;
                }
                true
            }
            ENV_SET_PIXEL_FORMAT => {
                let f = *(data as *const c_uint);
                FRAME.lock().unwrap().fmt = f;
                f == PIXFMT_0RGB1555 || f == PIXFMT_XRGB8888 || f == PIXFMT_RGB565
            }
            ENV_SET_HW_RENDER => false, // <-- REFUSE: forces software (angrylion). Never called by parallel_n64+angrylion anyway.
            ENV_GET_PREFERRED_HW_RENDER => {
                if !data.is_null() {
                    *(data as *mut c_uint) = HW_CONTEXT_NONE;
                }
                true
            }
            ENV_GET_VARIABLE => {
                let v = &mut *(data as *mut RetroVariable);
                if v.key.is_null() {
                    return false;
                }
                let key = CStr::from_ptr(v.key).to_string_lossy();
                if let Some(val) = forced_option(&key) {
                    // leak; the core reads it after we return and keeps polling it
                    v.value = CString::new(val).unwrap().into_raw();
                    true
                } else {
                    v.value = ptr::null();
                    false // false => core uses its own default for unforced keys
                }
            }
            ENV_GET_VARIABLE_UPDATE => {
                if !data.is_null() {
                    *(data as *mut bool) = false;
                }
                true
            }
            ENV_GET_SYSTEM_DIRECTORY | ENV_GET_CORE_ASSETS_DIRECTORY | ENV_GET_LIBRETRO_PATH => {
                *(data as *mut *const c_char) = SYS_DIR;
                true
            }
            ENV_GET_SAVE_DIRECTORY => {
                *(data as *mut *const c_char) = SAVE_DIR;
                true
            }
            ENV_GET_CORE_OPTIONS_VERSION => {
                if !data.is_null() {
                    *(data as *mut c_uint) = 2;
                }
                true
            }
            // ack option-registration cmds; our GET_VARIABLE does the forcing
            ENV_SET_VARIABLES
            | ENV_SET_CORE_OPTIONS
            | ENV_SET_CORE_OPTIONS_INTL
            | ENV_SET_CORE_OPTIONS_V2
            | ENV_SET_CORE_OPTIONS_V2_INTL
            | ENV_SET_CORE_OPTIONS_DISPLAY => true,
            // benign acks
            ENV_SET_SYSTEM_AV_INFO
            | ENV_SET_GEOMETRY
            | ENV_SET_PERFORMANCE_LEVEL
            | ENV_SET_SUPPORT_NO_GAME
            | ENV_SET_INPUT_DESCRIPTORS
            | ENV_SET_CONTROLLER_INFO
            | ENV_SET_SUBSYSTEM_INFO
            | ENV_SET_ROTATION
            | ENV_SET_MESSAGE => true,
            ENV_GET_INPUT_BITMASKS => false, // make the core poll individual ids
            // NOTE for the mupen-next fallback ONLY: add a GET_LOG_INTERFACE (27) arm here that
            // writes a real C-variadic fn ptr, or mupen SIGSEGVs in retro_load_game (see §10).
            _ => false,
        }
    }
}

extern "C" fn video_refresh(data: *const c_void, w: c_uint, h: c_uint, pitch: usize) {
    if data.is_null() {
        return; // dupe frame (GET_CAN_DUPE) -> keep last buffer
    }
    let mut f = FRAME.lock().unwrap();
    f.w = w;
    f.h = h;
    f.pitch = pitch;
    let n = pitch * h as usize;
    let src = unsafe { std::slice::from_raw_parts(data as *const u8, n) };
    f.bytes.clear();
    f.bytes.extend_from_slice(src);
}

extern "C" fn audio_sample(l: i16, r: i16) {
    let mut a = AUDIO.lock().unwrap();
    a.push(l);
    a.push(r);
}
extern "C" fn audio_batch(data: *const i16, frames: usize) -> usize {
    let s = unsafe { std::slice::from_raw_parts(data, frames * 2) };
    AUDIO.lock().unwrap().extend_from_slice(s);
    frames
}
extern "C" fn input_poll() {}
extern "C" fn input_state(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16 {
    if port != 0 {
        return 0; // P1 only for now (P2 == port 1; see §5)
    }
    let p = PAD.lock().unwrap();
    match device {
        DEV_JOYPAD => {
            if (id as usize) < 16 && p.btn[id as usize] {
                1
            } else {
                0
            }
        }
        DEV_ANALOG => match (index, id) {
            (ANALOG_LEFT, ANALOG_X) => p.lx,
            (ANALOG_LEFT, ANALOG_Y) => p.ly,
            (ANALOG_RIGHT, ANALOG_X) => p.cx,
            (ANALOG_RIGHT, ANALOG_Y) => p.cy,
            _ => 0,
        },
        _ => 0,
    }
}

// ---------- resolved core entry points ----------
type EnvFn = extern "C" fn(c_uint, *mut c_void) -> bool;
type VideoFn = extern "C" fn(*const c_void, c_uint, c_uint, usize);
type AudioFn = extern "C" fn(i16, i16);
type AudioBatchFn = extern "C" fn(*const i16, usize) -> usize;
type PollFn = extern "C" fn();
type InputFn = extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;

pub struct N64 {
    _lib: Library, // keep the dylib mapped for the process lifetime
    run: Symbol<'static, unsafe extern "C" fn()>,
    pub fps: f64,
    pub sample_rate: f64,
    pub width: u32,
    pub height: u32,
}

impl N64 {
    pub fn new(core_path: &str, rom_path: &str) -> anyhow::Result<Self> {
        // writable scratch dirs the core may read/derive save paths from
        std::fs::create_dir_all("/tmp/n64sys").ok();
        std::fs::create_dir_all("/tmp/n64save").ok();
        unsafe {
            SYS_DIR = CString::new("/tmp/n64sys")?.into_raw();
            SAVE_DIR = CString::new("/tmp/n64save")?.into_raw();
        }

        let lib = unsafe { Library::new(core_path) }
            .map_err(|e| anyhow::anyhow!("dlopen {core_path}: {e}"))?;
        // SAFETY: we keep `lib` alive in the returned struct; transmute the symbol lifetimes
        // to 'static so they can live alongside it. The Library is never dropped before them.
        unsafe {
            macro_rules! sym {
                ($t:ty, $n:expr) => {{
                    let s: Symbol<$t> = lib.get($n)?;
                    std::mem::transmute::<Symbol<$t>, Symbol<'static, _>>(s)
                }};
            }
            let set_environment = sym!(unsafe extern "C" fn(EnvFn), b"retro_set_environment");
            let set_video = sym!(unsafe extern "C" fn(VideoFn), b"retro_set_video_refresh");
            let set_audio = sym!(unsafe extern "C" fn(AudioFn), b"retro_set_audio_sample");
            let set_audio_batch =
                sym!(unsafe extern "C" fn(AudioBatchFn), b"retro_set_audio_sample_batch");
            let set_poll = sym!(unsafe extern "C" fn(PollFn), b"retro_set_input_poll");
            let set_input = sym!(unsafe extern "C" fn(InputFn), b"retro_set_input_state");
            let init = sym!(unsafe extern "C" fn(), b"retro_init");
            let load_game =
                sym!(unsafe extern "C" fn(*const RetroGameInfo) -> bool, b"retro_load_game");
            let get_av =
                sym!(unsafe extern "C" fn(*mut RetroSystemAvInfo), b"retro_get_system_av_info");
            let set_ctrl = sym!(unsafe extern "C" fn(c_uint, c_uint), b"retro_set_controller_port_device");
            let get_info = sym!(unsafe extern "C" fn(*mut RetroSystemInfo), b"retro_get_system_info");
            let run: Symbol<'static, unsafe extern "C" fn()> = sym!(unsafe extern "C" fn(), b"retro_run");

            // ORDER MATTERS: environment first, then media callbacks, then init.
            set_environment(environment);
            set_video(video_refresh);
            set_audio(audio_sample);
            set_audio_batch(audio_batch);
            set_poll(input_poll);
            set_input(input_state);
            init();

            let mut info: RetroSystemInfo = std::mem::zeroed();
            get_info(&mut info);
            let need_fullpath = info.need_fullpath; // false for both N64 cores

            // need_fullpath == false -> pass ROM bytes; also pass path so the core derives SRAM names.
            let rom = std::fs::read(rom_path)?; // 16 MiB .z64, native big-endian, NO byteswap
            let path_c = CString::new(rom_path)?;
            let gi = RetroGameInfo {
                path: path_c.as_ptr(),
                data: rom.as_ptr() as *const c_void,
                size: rom.len(),
                meta: ptr::null(),
            };
            if !load_game(&gi) {
                anyhow::bail!("retro_load_game failed (core refused the ROM)");
            }
            // The core reads from `rom` after load_game when need_fullpath==false; keep it alive.
            if !need_fullpath {
                std::mem::forget(rom);
            }
            std::mem::forget(path_c);

            let mut av: RetroSystemAvInfo = std::mem::zeroed();
            get_av(&mut av);
            set_ctrl(0, DEV_JOYPAD); // port 0 = standard pad (+analog via DEV_ANALOG queries)

            tracing::info!(
                "N64 core loaded: fps={:.4} sample_rate={:.0} base={}x{}",
                av.timing.fps,
                av.timing.sample_rate,
                av.geometry.base_width,
                av.geometry.base_height
            );

            Ok(N64 {
                _lib: lib,
                run,
                fps: av.timing.fps,           // ~60.13 (parallel) / 60.0 (mupen)
                sample_rate: av.timing.sample_rate, // 44100
                width: av.geometry.base_width,
                height: av.geometry.base_height,
            })
        }
    }

    /// Advance exactly one video frame (fires video/audio/input callbacks).
    pub fn clock_frame(&mut self) {
        unsafe { (self.run)() }
    }

    /// Latest framebuffer (XRGB8888 640x240, byte order B,G,R,X). Clone-free: returns a guard.
    pub fn with_frame<R>(&self, f: impl FnOnce(&Frame) -> R) -> R {
        f(&FRAME.lock().unwrap())
    }

    /// Drain all audio accumulated since the last call (interleaved L,R i16 @ self.sample_rate).
    pub fn audio_drain(&self) -> Vec<i16> {
        std::mem::take(&mut *AUDIO.lock().unwrap())
    }

    pub fn set_button(&self, id: usize, pressed: bool) {
        if id < 16 {
            PAD.lock().unwrap().btn[id] = pressed;
        }
    }
    pub fn set_stick(&self, x: i16, y: i16) {
        let mut p = PAD.lock().unwrap();
        p.lx = x;
        p.ly = y;
    }
    pub fn set_cstick(&self, x: i16, y: i16) {
        let mut p = PAD.lock().unwrap();
        p.cx = x;
        p.cy = y;
    }
}

/// Browser wire button name -> action. The pipeline calls this; analog directions are folded
/// into the control stick / C-stick so a keyboard can drive the N64 pad.
/// Returns None for unknown names (ignored).
pub enum N64Action {
    Btn(usize),         // a joypad id in PAD.btn
    Stick(i16, i16),    // control-stick deflection on press (0,0 on release)
    CStick(i16, i16),   // C-stick deflection
}
pub fn map_button(b: &str) -> Option<N64Action> {
    use N64Action::*;
    Some(match b {
        "A" => Btn(ID_A),
        "B" => Btn(ID_B),
        "Start" => Btn(ID_START),
        "Z" => Btn(ID_L2),  // ParaLLEl/mupen default: Z -> L2 (also try ID_L if a build differs)
        "L" => Btn(ID_L),
        "R" => Btn(ID_R),
        // control stick (analog) — keyboard cluster, full deflection
        "StickUp" => Stick(0, -AXIS_MAX),
        "StickDown" => Stick(0, AXIS_MAX),
        "StickLeft" => Stick(-AXIS_MAX, 0),
        "StickRight" => Stick(AXIS_MAX, 0),
        // C-buttons via the right analog stick
        "CUp" => CStick(0, -AXIS_MAX),
        "CDown" => CStick(0, AXIS_MAX),
        "CLeft" => CStick(-AXIS_MAX, 0),
        "CRight" => CStick(AXIS_MAX, 0),
        // D-pad (digital) — optional; SSB64 menus use the stick, but expose it anyway
        "Up" => Btn(ID_UP),
        "Down" => Btn(ID_DOWN),
        "Left" => Btn(ID_LEFT),
        "Right" => Btn(ID_RIGHT),
        _ => return None,
    })
}
```

> **Stick/C-stick on a keyboard, combining opposite/diagonal presses:** the simple
> `Stick(±MAX,0)` mapping above overwrites the axis per key, so it cannot express diagonals or
> hold-two-keys nicely. The pipeline (§5) instead tracks a small `held: HashSet<String>` and
> recomputes the stick vector from the currently-held direction keys each input event — that
> gives clean 8-direction control. `map_button` stays the source of truth for *which* axis a
> key touches; the pipeline owns the accumulation. Keep both; it is a few lines.

---

## 3. `src/video.rs` — 640×240, XRGB8888 (BGRX) → I420

The N64 base differs from NES (256×240). angrylion delivers **640×240, pitch 2560,
XRGB8888** (`B,G,R,X` in memory). The frame dims can in principle change (interlace/menus),
so we **fix the encoder canvas to 640×240** and copy/letterbox the live frame into it using
the callback's real `pitch` (never `width*4`). 640×240 keeps the VP8 encoder dimensions
constant (required — VP8 can't resize mid-stream without a re-init). If the core ever delivers
a smaller frame, we center it on a black 640×240 canvas; larger, we clip.

```rust
//! src/video.rs — XRGB8888 (BGRX) -> I420 + realtime VP8 encoder for the N64 output (640x240).
//!
//! angrylion delivers 640x240 XRGB8888 (pitch 2560, memory byte order B,G,R,X). We fix the
//! encoder canvas to 640x240 and copy each live frame in using its real pitch (dims can change).

use vpx_encode::{Config, Encoder, VideoCodecId};

pub const W: usize = 640; // N64 VI width (angrylion line-doubles 320 -> 640)
pub const H: usize = 240; // single (non-interlaced) field for SSB64
pub const I420_LEN: usize = W * H + 2 * ((W / 2) * (H / 2)); // 640*240 + 2*(320*120) = 230_400

/// Realtime VP8 encoder for the N64 output (640x240). timebase [1,1000] => pts in ms.
/// A fresh encoder emits a keyframe on its first encode() — pipeline.rs uses this for new viewers.
pub fn make_vp8_encoder() -> vpx_encode::Result<Encoder> {
    Encoder::new(Config {
        width: W as u32,
        height: H as u32,
        timebase: [1, 1000],
        bitrate: 3500, // kbps. 640x240 @60 with motion (SSB64) wants more than NES; 2500..=5000 sane.
        codec: VideoCodecId::VP8,
    })
}

/// XRGB8888 (memory bytes B,G,R,X), src dims `sw x sh` with row stride `pitch`, ->
/// packed I420 on a fixed W x H (640x240) canvas, BT.601 limited range. Centers/clips if the
/// source differs from the canvas. `dst.len()` must be >= I420_LEN; reuse it across frames.
pub fn xrgb_to_i420(src: &[u8], sw: usize, sh: usize, pitch: usize, dst: &mut [u8]) {
    let y_size = W * H;
    let c_w = W / 2;
    let c_h = H / 2;
    let (y_plane, uv) = dst.split_at_mut(y_size);
    let (u_plane, v_plane) = uv.split_at_mut(c_w * c_h);

    // letterbox offsets (centered) if the live frame is smaller than the canvas
    let ox = (W.saturating_sub(sw)) / 2;
    let oy = (H.saturating_sub(sh)) / 2;
    // clip the copied region to the canvas
    let cw = sw.min(W - ox.min(W));
    let ch = sh.min(H - oy.min(H));

    // black canvas (Y=16, U=V=128) baseline, then paint the active region.
    for b in y_plane.iter_mut() {
        *b = 16;
    }
    for b in u_plane.iter_mut() {
        *b = 128;
    }
    for b in v_plane.iter_mut() {
        *b = 128;
    }

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
            // BT.601 limited: Y in [16,235], U/V centered at 128. <<8 fixed point.
            let y = (66 * r + 129 * g + 25 * b + 128) >> 8;
            y_plane[dy * W + dx] = (y + 16) as u8;
            if (j & 1) == 0 && (i & 1) == 0 {
                let u = (-38 * r - 74 * g + 112 * b + 128) >> 8;
                let v = (112 * r - 94 * g - 18 * b + 128) >> 8;
                let ci = ((dy / 2) * c_w) + (dx / 2);
                u_plane[ci] = (u + 128) as u8;
                v_plane[ci] = (v + 128) as u8;
            }
        }
    }
}
```

> **Note on aspect / scaling:** 640×240 has a 8:3 pixel grid but the N64 displays 4:3
> (aspect_ratio = 1.333). The browser `<video>` is given an explicit 4:3 box in `index.html`
> (§7), so the stretch happens client-side — no server-side rescale needed. If you later want a
> square-pixel stream, swap in `libyuv`/`fast_image_resize` to scale 640×240 → 640×480, but for
> SSB64 the client-side stretch is fine and saves CPU.

---

## 4. `src/audio.rs` — STEREO Opus + 44100 → 48000 resample

The N64 core delivers **i16 stereo @ 44100 Hz**; Opus/WebRTC wants **48000 Hz**. We add a
simple per-channel linear resampler (44100→48000) feeding a **stereo** Opus encoder
(interleaved L,R, 960 frames = 20 ms per packet). Linear resampling is adequate for game
audio at this small ratio (48000/44100 ≈ 1.088); if you want pristine quality later, drop in
`rubato`, but the brief asks only for a working stereo 48k path.

```rust
//! src/audio.rs — i16 stereo @ core rate (44100) -> resample to 48000 -> STEREO Opus 960-frame packets.

use opus::{Application, Bitrate, Channels, Encoder as OpusEncoder};

pub const OPUS_SAMPLE_RATE: u32 = 48_000;
pub const OPUS_FRAME: usize = 960; // 20 ms @ 48 kHz, PER CHANNEL. Stereo packet = 960*2 i16.
pub const CHANNELS: usize = 2;

pub struct OpusPacket {
    pub data: Vec<u8>,
    pub samples: u32, // per-channel sample count (always OPUS_FRAME = 960)
}

pub struct OpusStreamer {
    enc: OpusEncoder,
    in_rate: f64,
    // fractional read position into a small input history, per resample step
    pos: f64,
    // last input sample per channel (for interpolation across drain calls)
    prev_l: i16,
    prev_r: i16,
    have_prev: bool,
    // resampled, interleaved L,R i16 awaiting packetization
    pcm: Vec<i16>,
    out: Vec<u8>,
}

impl OpusStreamer {
    pub fn new(in_rate: f64) -> opus::Result<Self> {
        let mut enc = OpusEncoder::new(OPUS_SAMPLE_RATE, Channels::Stereo, Application::Audio)?;
        enc.set_bitrate(Bitrate::Bits(128_000))?; // stereo game audio
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        Ok(Self {
            enc,
            in_rate,
            pos: 0.0,
            prev_l: 0,
            prev_r: 0,
            have_prev: false,
            pcm: Vec::with_capacity(8192),
            out: vec![0u8; 4000],
        })
    }

    /// Feed interleaved L,R i16 @ in_rate; linear-resample to 48000 and buffer.
    pub fn push_i16_stereo(&mut self, input: &[i16]) {
        if input.is_empty() {
            return;
        }
        let n = input.len() / 2; // input frames
        let step = self.in_rate / OPUS_SAMPLE_RATE as f64; // input frames per output frame (<1)
        // Walk output positions across [0, n) using `pos` carried between calls.
        // Sample s = idx0 plus fractional `frac` between idx0 and idx0+1.
        let mut p = self.pos;
        while p < n as f64 {
            let i0 = p.floor() as usize;
            let frac = p - i0 as f64;
            let (l0, r0, l1, r1);
            if i0 == 0 && self.have_prev {
                l0 = self.prev_l as f64;
                r0 = self.prev_r as f64;
                l1 = input[0] as f64;
                r1 = input[1] as f64;
            } else {
                let a = i0.min(n - 1);
                let b = (i0 + 1).min(n - 1);
                l0 = input[a * 2] as f64;
                r0 = input[a * 2 + 1] as f64;
                l1 = input[b * 2] as f64;
                r1 = input[b * 2 + 1] as f64;
            }
            let l = l0 + (l1 - l0) * frac;
            let r = r0 + (r1 - r0) * frac;
            self.pcm.push(l.round().clamp(-32768.0, 32767.0) as i16);
            self.pcm.push(r.round().clamp(-32768.0, 32767.0) as i16);
            p += step;
        }
        // carry the leftover fractional position into the next call's coordinate frame
        self.pos = p - n as f64;
        self.prev_l = input[(n - 1) * 2];
        self.prev_r = input[(n - 1) * 2 + 1];
        self.have_prev = true;
    }

    /// Drain every full stereo 960-frame chunk, encoding each into an Opus packet.
    pub fn take_packets(&mut self) -> opus::Result<Vec<OpusPacket>> {
        let mut packets = Vec::new();
        let chunk = OPUS_FRAME * CHANNELS; // 1920 interleaved i16
        while self.pcm.len() >= chunk {
            let frame: Vec<i16> = self.pcm.drain(..chunk).collect();
            let n = self.enc.encode(&frame, &mut self.out)?;
            packets.push(OpusPacket {
                data: self.out[..n].to_vec(),
                samples: OPUS_FRAME as u32,
            });
        }
        Ok(packets)
    }
}
```

> Audio sanity: 44100 Hz → ~735 input frames per 60 fps video frame → after resample ~800
> output frames/video frame; 960-frame packets emit at exactly 50/s (20 ms each). The probe
> measured ~721–735 stereo frames/video-frame at 44100, matching.

---

## 5. `src/pipeline.rs` — core-fps timing, stereo plumbing, N64 input

Changes vs the NES pipeline:
1. `Emu` → `N64`; `clock_frame()` is `retro_run()`.
2. **Frame period from the core fps** (~60.13 for parallel_n64), not the NES constant.
3. **Stereo audio**: drain `Vec<i16>` (interleaved) → `OpusStreamer::push_i16_stereo`.
4. **N64 input**: track held direction keys → recompute analog stick / C-stick vectors;
   buttons set/cleared directly.
5. **Video**: `xrgb_to_i420` with the frame's real `w/h/pitch`.

```rust
//! src/pipeline.rs — one dedicated OS thread runs the N64 core at real-time core-fps,
//! encodes each frame to VP8 + stereo Opus, fans media out over broadcast channels.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use crate::audio::OpusStreamer;
use crate::n64::{map_button, N64Action, N64, AXIS_MAX};
use crate::video::{make_vp8_encoder, xrgb_to_i420, I420_LEN};

// Default core path; overridable. Resolved to an absolute path in main.rs.
pub const DEFAULT_CORE: &str = "cores/parallel_n64_libretro.dylib";

#[derive(Clone)]
pub struct EncodedVideo {
    pub data: Bytes,
}
#[derive(Clone)]
pub struct EncodedAudio {
    pub data: Bytes,
    pub samples: u32, // per-channel (960)
}

/// Browser input event: {"type":"down"|"up","button":"A","player":1}.
#[derive(serde::Deserialize)]
pub struct InputEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub button: String,
    #[serde(default = "default_player")]
    pub player: u8,
}
fn default_player() -> u8 {
    1
}

pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>,
    pub audio_tx: broadcast::Sender<EncodedAudio>,
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
    pub keyframe_req: Arc<AtomicBool>,
}

pub fn start(core_path: String, rom_path: String) -> Arc<AppInner> {
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(16);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(64); // stereo => ~50 pkt/s, give headroom
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let keyframe_req = Arc::new(AtomicBool::new(false));

    let v = video_tx.clone();
    let a = audio_tx.clone();
    let kf = keyframe_req.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_loop(core_path, rom_path, v, a, input_rx, kf) {
            tracing::error!("emulator loop ended: {e:?}");
        }
    });

    Arc::new(AppInner {
        video_tx,
        audio_tx,
        input_tx,
        keyframe_req,
    })
}

/// Recompute the analog stick vector from currently-held direction keys (8-way).
fn stick_vec(held: &HashSet<String>, up: &str, down: &str, left: &str, right: &str) -> (i16, i16) {
    let mut x = 0i32;
    let mut y = 0i32;
    if held.contains(left) {
        x -= AXIS_MAX as i32;
    }
    if held.contains(right) {
        x += AXIS_MAX as i32;
    }
    if held.contains(up) {
        y -= AXIS_MAX as i32;
    }
    if held.contains(down) {
        y += AXIS_MAX as i32;
    }
    (x.clamp(-32768, 32767) as i16, y.clamp(-32768, 32767) as i16)
}

fn run_loop(
    core_path: String,
    rom_path: String,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    keyframe_req: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut emu = N64::new(&core_path, &rom_path)?;
    let mut vpx = make_vp8_encoder().map_err(|e| anyhow::anyhow!("vpx init: {e:?}"))?;
    let mut opus = OpusStreamer::new(emu.sample_rate).map_err(|e| anyhow::anyhow!("opus init: {e:?}"))?;

    let mut i420 = vec![0u8; I420_LEN];
    // Frame period from the CORE fps (parallel_n64 ~60.13 Hz). 1e9 / fps.
    let frame_period = Duration::from_nanos((1_000_000_000.0 / emu.fps) as u64);
    let mut next = Instant::now();
    let mut frame_idx: u64 = 0;

    // Held direction keys -> analog vectors (8-way stick + C-stick on a keyboard).
    let mut held: HashSet<String> = HashSet::new();

    let mut stat_t = Instant::now();
    let mut stat_frames: u64 = 0;

    loop {
        // 1. Apply all pending input (P1 only for now; P2 == port 1, see note).
        let mut stick_dirty = false;
        while let Ok(ev) = input_rx.try_recv() {
            let pressed = ev.kind == "down";
            match map_button(&ev.button) {
                Some(N64Action::Btn(id)) => emu.set_button(id, pressed),
                Some(N64Action::Stick(_, _)) | Some(N64Action::CStick(_, _)) => {
                    // direction key: track held set, recompute vectors below
                    if pressed {
                        held.insert(ev.button.clone());
                    } else {
                        held.remove(&ev.button);
                    }
                    stick_dirty = true;
                }
                None => {}
            }
        }
        if stick_dirty {
            let (sx, sy) = stick_vec(&held, "StickUp", "StickDown", "StickLeft", "StickRight");
            emu.set_stick(sx, sy);
            let (cx, cy) = stick_vec(&held, "CUp", "CDown", "CLeft", "CRight");
            emu.set_cstick(cx, cy);
        }

        // 2. New viewer -> reset encoder for a fresh keyframe.
        if keyframe_req.swap(false, Ordering::Relaxed) {
            if let Ok(e) = make_vp8_encoder() {
                vpx = e;
                tracing::info!("vp8 encoder reset -> keyframe for new viewer");
            }
        }

        // 3. Advance one frame.
        emu.clock_frame();

        // 4. VIDEO: latest XRGB8888 -> I420 -> VP8 -> broadcast.
        emu.with_frame(|f| {
            if !f.bytes.is_empty() {
                xrgb_to_i420(&f.bytes, f.w as usize, f.h as usize, f.pitch, &mut i420);
            }
        });
        let pts_ms = (frame_idx as f64 * 1000.0 / emu.fps) as i64;
        match vpx.encode(pts_ms, &i420) {
            Ok(packets) => {
                for frame in packets {
                    let _ = video_tx.send(EncodedVideo {
                        data: Bytes::copy_from_slice(frame.data),
                    });
                }
            }
            Err(e) => tracing::warn!("vpx encode: {e:?}"),
        }

        // 5. AUDIO: drain i16 stereo @ core rate -> resample 48k -> stereo Opus -> broadcast.
        let pcm = emu.audio_drain();
        opus.push_i16_stereo(&pcm);
        match opus.take_packets() {
            Ok(pkts) => {
                for p in pkts {
                    let _ = audio_tx.send(EncodedAudio {
                        data: Bytes::from(p.data),
                        samples: p.samples,
                    });
                }
            }
            Err(e) => tracing::warn!("opus encode: {e:?}"),
        }

        // 6. Stats.
        stat_frames += 1;
        if stat_t.elapsed() >= Duration::from_secs(5) {
            let secs = stat_t.elapsed().as_secs_f64();
            tracing::info!(
                "n64: {:.1} fps | viewers v={} a={}",
                stat_frames as f64 / secs,
                video_tx.receiver_count(),
                audio_tx.receiver_count(),
            );
            stat_t = Instant::now();
            stat_frames = 0;
        }

        // 7. Drift-compensated pacing to the next core-fps deadline.
        frame_idx += 1;
        next += frame_period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
```

> **P2 (optional):** the `input_state` callback already keys on `port`. To enable P2, add a
> second `Pad` global keyed by port, call `retro_set_controller_port_device(1, DEV_JOYPAD)` in
> `N64::new`, and route `ev.player == 2` to it. SSB64 is a 4-player game; for the first cut,
> ship P1 and leave P2 as a documented follow-up (the keymap in §7 lists only P1).

---

## 6. `src/webrtc.rs` — Opus stereo

`TrackLocalStaticSample` for Opus advertises channel count via the codec capability. Set
`channels: 2` on the audio `RTCRtpCodecCapability` so the SDP offers stereo Opus
(`a=fmtp:111 ... stereo=1;sprop-stereo=1`). The audio writer's duration math is **per-channel**
(960 samples / 48000 = 20 ms), which is already correct — `p.samples` stays 960.

Change only the audio track capability:

```rust
// --- AUDIO track (Opus, STEREO) ---
let audio_track = Arc::new(TrackLocalStaticSample::new(
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,                                  // <-- stereo
        sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1".to_owned(),
        ..Default::default()
    },
    "audio".to_owned(),
    "n64".to_owned(),                                 // stream id (was "nes")
));
```

The duration line stays:
```rust
let dur = Duration::from_millis((p.samples as u64 * 1000) / 48_000); // 960 -> 20 ms
```

> `build_api()` calls `m.register_default_codecs()`, which registers Opus at 2 channels already,
> so the negotiation accepts the stereo fmtp. No other webrtc.rs change is required. (If a
> browser still negotiates mono, it will downmix our stereo packets harmlessly — but the fmtp
> above makes Chrome/Firefox keep stereo.) Optionally rename the `"nes"` video stream id to
> `"n64"` for tidiness (cosmetic).

---

## 7. `static/index.html` — N64 keyboard layout (P1)

Map a keyboard to the N64 pad: **analog control stick on the arrow keys**, A/B/Z/L/R/Start on
a key cluster, and the **C-buttons on IJKL**. The 4:3 display box stays (now fed by a 640×240
stream stretched to 4:3 client-side).

Replace the `<style>` `#video` size, the hint, and the `KEYMAP`:

```html
<!-- video box: 4:3 (N64 aspect). Stream is 640x240 stretched to fill. -->
<style> /* ...unchanged... */
  #video {
    width: 512px; height: 384px; background: #000;   /* 4:3 */
    image-rendering: pixelated; border: 1px solid #23232e; border-radius: 6px;
  }
</style>

<h1>Super Smash Bros. 64 over WebRTC · server-side emulation</h1>

<p class="hint">
  <b>P1 control stick:</b> <kbd>←↑↓→</kbd> arrows ·
  <b>A</b> <kbd>X</kbd> · <b>B</b> <kbd>Z</kbd> · <b>Z-trig</b> <kbd>C</kbd> ·
  <b>L</b> <kbd>Q</kbd> · <b>R</b> <kbd>E</kbd> · <b>Start</b> <kbd>Enter</kbd>.
  <br /><b>C-buttons:</b> <kbd>I</kbd><kbd>J</kbd><kbd>K</kbd><kbd>L</kbd> (up/left/down/right).
  <br />El audio/vídeo se generan en el servidor; este navegador solo recibe el stream.
</p>

<script>
  // Physical-key (e.code) -> { player, N64 wire button }. The server folds the
  // Stick*/C* names into the analog axes; A/B/Z/L/R/Start are digital.
  const KEYMAP = {
    // control stick (analog) on the arrows
    ArrowUp:    { p: 1, b: "StickUp" },
    ArrowDown:  { p: 1, b: "StickDown" },
    ArrowLeft:  { p: 1, b: "StickLeft" },
    ArrowRight: { p: 1, b: "StickRight" },
    // face / shoulder / start
    KeyX:     { p: 1, b: "A" },
    KeyZ:     { p: 1, b: "B" },
    KeyC:     { p: 1, b: "Z" },     // N64 Z trigger
    KeyQ:     { p: 1, b: "L" },
    KeyE:     { p: 1, b: "R" },
    Enter:    { p: 1, b: "Start" },
    // C-buttons on IJKL
    KeyI:     { p: 1, b: "CUp" },
    KeyK:     { p: 1, b: "CDown" },
    KeyJ:     { p: 1, b: "CLeft" },
    KeyL:     { p: 1, b: "CRight" },
  };
  // ...the rest of the connect()/keydown/keyup logic is UNCHANGED (it already sends
  //    {type, button, player} JSON and de-dupes key-repeat with a Set).
</script>
```

Everything else in `index.html` (signaling, ICE wait, data channel, keydown/keyup handlers)
is unchanged — they already POST `{type, button, player}`.

---

## 8. `src/main.rs` — DEFAULT_ROM + core path

Point the default ROM at the SSB64 .z64 and add a default core path. Resolve both to absolute
paths (the dylib path matters for `dlopen`; CWD is the project root when run via `cargo run`).

```rust
mod audio;
mod n64;        // was: mod emu;
mod pipeline;
mod signaling;
mod video;
mod webrtc;

const DEFAULT_ROM: &str =
    "~/pokemon-pvp-red/Super Smash Bros. (U) [!].z64";
const DEFAULT_CORE: &str =
    "~/pokemon-pvp-red/cores/parallel_n64_libretro.dylib";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nes_web=info,webrtc=warn".into()),
        )
        .init();

    // arg1 = ROM (.z64), arg2 = core (.dylib); both optional.
    let rom_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_ROM.to_string());
    let core_path = std::env::args().nth(2).unwrap_or_else(|| DEFAULT_CORE.to_string());
    tracing::info!("ROM:  {rom_path}");
    tracing::info!("core: {core_path}");

    // The N64 core is loaded on the emulator thread inside pipeline::start.
    let inner = pipeline::start(core_path, rom_path);

    let api = crate::webrtc::build_api()?;
    let state = signaling::AppState { api, inner };
    let app = signaling::router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  open http://localhost:3000  (click Connect)");
    axum::serve(listener, app).await?;
    Ok(())
}
```

> `pipeline::start` now takes `(core_path, rom_path)` and reads the ROM **on the emu thread**
> inside `N64::new` (so the 16 MiB buffer stays owned by the emulator and is `mem::forget`-kept
> alive for the core, per `need_fullpath=false`). Delete `src/emu.rs` (or keep it for an NES
> mode behind a flag).

---

## 9. Build / run

```bash
# 0. cores already present:
ls -l cores/parallel_n64_libretro.dylib cores/mupen64plus_next_libretro.dylib
# strip quarantine only if Safari/app-bundle ever set it (curl downloads are clean):
xattr -d com.apple.quarantine cores/*.dylib 2>/dev/null || true

# 1. Cargo.toml deps: ADD libloading; tetanes-core becomes optional (keep for NES mode or drop).
#    [dependencies]
#    libloading = "0.8"          # dlopen the N64 core
#    # tetanes-core = "0.14.1"   # only if you keep an NES path
#    (vpx-encode, opus, webrtc, axum, tokio, etc. all UNCHANGED)

# 2. Build with the pinned toolchain (webrtc 0.17 + this repo require 1.92).
RUSTUP_TOOLCHAIN=1.92 cargo build --release

# 3. Run (defaults to SSB64 + parallel_n64):
RUSTUP_TOOLCHAIN=1.92 cargo run --release
#   or explicit:
RUSTUP_TOOLCHAIN=1.92 ./target/release/nes-web \
  "~/pokemon-pvp-red/Super Smash Bros. (U) [!].z64" \
  "~/pokemon-pvp-red/cores/parallel_n64_libretro.dylib"

# 4. Open http://localhost:3000 and click Connect.
```

**Link / framework flags:**
- **parallel_n64 (primary): NONE.** It has no GL/Vulkan/Cocoa load commands; `dlopen` pulls in
  only `libSystem`/`libc++` (already linked by Rust). No `cargo:rustc-link-*`, no `build.rs`,
  no `OpenGL.framework`. This is the entire reason it's the primary.
- **mupen64plus-next (fallback): add the `logshim.c` build step.** That core links
  `OpenGL.framework` (loaded lazily, unused by angrylion) and requires the C-variadic log shim.
  See §10 for the exact `build.rs` + `logshim.c` (verified in `research/n64-harness/`).

**System & content dirs the core needs:** `N64::new` creates `/tmp/n64sys` and `/tmp/n64save`
and returns them for `GET_SYSTEM_DIRECTORY`/`GET_SAVE_DIRECTORY`/`GET_CORE_ASSETS_DIRECTORY`.
**No external BIOS or data file is required** for angrylion + the SSB64 cart (stock cartridge,
self-contained). The .z64 is native big-endian (`80 37 12 40`) — **no byteswap**.

---

## 10. Risks & fallbacks

### A. Real-time performance for SSB64 — LOW RISK
Measured headless: **parallel_n64 ~1400 fps (~23×)**, mupen-next ~327–384 fps (~5–6×) on this
Apple Silicon box, all with angrylion multithreaded. 60 fps streaming + VP8/Opus encode on the
same machine fits with huge headroom. The probes measured the **boot/logo + early menu**, which
is light; **actual 4-player melee with particles is heavier** and the VI may toggle to a fuller
frame. Mitigations if a busy match dips:
- Keep angrylion multithreaded (`all threads`) — already on; scales across P-cores.
- Stay at native internal res (320/640×240); never software-upscale.
- **Frame-skip the encoder, not the emulation:** if encode falls behind, drop the I420 convert
  + VP8 encode for a frame while still calling `retro_run()` (the pacing loop already resyncs
  `next` when it overruns). This degrades visual fps under load without desyncing audio/game.
- Drop `rspplugin` from `cxd4` (LLE) to `hle` for a further speedup (renders SSB64 identically
  per the probes).

### B. GL-context headless reliability — NOT APPLICABLE to the chosen path
parallel_n64 + angrylion **never requests `SET_HW_RENDER`** (verified: env cmd 14 never fires),
so there is no GL context to be unreliable. We also `return false` to `SET_HW_RENDER`
defensively. The whole class of "headless GL on a server / no display" risk is sidestepped.

### C. Plan B — if we must use mupen64plus-next (or a HW-GL core)
Two escalation tiers, both with verified recipes in the research:

1. **mupen64plus-next + angrylion (software), with the log shim.** Fully verified
   (`research/n64-mupen-next.md`, title-screen proof at `research/ssb64-titlescreen.png`,
   ~384 fps). To switch:
   - Point `N64::new` at `cores/mupen64plus_next_libretro.dylib`.
   - Swap `forced_option` to the mupen keys:
     ```
     mupen64plus-rdp-plugin            = angrylion
     mupen64plus-rsp-plugin            = hle        (or cxd4)
     mupen64plus-angrylion-multithread = all threads
     mupen64plus-angrylion-sync        = Low
     mupen64plus-EnableFBEmulation     = True
     mupen64plus-cpucore               = dynamic_recompiler
     mupen64plus-43screensize          = 320x240
     mupen64plus-aspect                = 4:3
     ```
   - **Add a `GET_LOG_INTERFACE` (cmd 27) arm** to `environment` that writes a **real
     C-variadic** log fn pointer, or `retro_load_game` SIGSEGVs calling a NULL log ptr. Rust
     stable can't define C-variadics, so compile `logshim.c` and link it via `build.rs`
     (both verified, lifted from `research/n64-harness/`):
     ```c
     // logshim.c
     #include <stdarg.h>
     #include <stdio.h>
     void n64_core_log(int level, const char *fmt, ...) {
         va_list ap; va_start(ap, fmt);
         vfprintf(stderr, fmt, ap); va_end(ap); (void)level;
     }
     ```
     ```rust
     // build.rs
     fn main() {
         let out = std::env::var("OUT_DIR").unwrap();
         assert!(std::process::Command::new("clang")
             .args(["-O2","-arch","arm64","-c","logshim.c","-o"])
             .arg(format!("{out}/logshim.o")).status().unwrap().success());
         assert!(std::process::Command::new("ar").args(["crs"])
             .arg(format!("{out}/liblogshim.a")).arg(format!("{out}/logshim.o"))
             .status().unwrap().success());
         println!("cargo:rustc-link-search=native={out}");
         println!("cargo:rustc-link-lib=static=logshim");
         println!("cargo:rerun-if-changed=logshim.c");
     }
     ```
     ```rust
     // in environment(): cmd 27 == GET_LOG_INTERFACE
     27 => {
         extern "C" { fn n64_core_log(); }
         #[repr(C)] struct LogCb { log: *const c_void }
         (*(data as *mut LogCb)).log = n64_core_log as *const c_void;
         true
     }
     ```

2. **HW-GL core (GLideN64 / parallel-RDP) via offscreen CGL** — only if a future game needs a
   HW renderer. macOS gives a windowless GL context via raw CGL (no display server), verified
   in `research/n64-parallel-gl.md`: `CGLChoosePixelFormat` (3.2 core, no window) →
   `CGLCreateContext` → `CGLSetCurrentContext` → FBO (RGBA8+depth24) → `glReadPixels`. Then in
   `environment`, **accept** `SET_HW_RENDER` (return true), provide `get_proc_address`
   (`dlsym(RTLD_DEFAULT, name)`) and `get_current_framebuffer` (your FBO name), call
   `context_reset()` once, and each frame `retro_run()` → `glReadPixels` your FBO → encode. The
   box reports GL 4.1 core (Metal-backed), sufficient for GLideN64. **This is more wiring and
   buys nothing for SSB64** — reserve it for a Vulkan/parallel-RDP path (would also need
   MoltenVK; the prebuilt mupen dylib links OpenGL only, so its parallel-RDP option errors with
   "VK_KHR_16bit_storage not supported").

### D. Other operational risks
- **Frame dims can change** (interlace/menu): always read `w/h/pitch` from `video_refresh`
  (handled — `xrgb_to_i420` letterboxes onto the fixed 640×240 canvas). VP8 dims stay constant.
- **`video_refresh(NULL,...)` dupe frames:** keep the last buffer (handled).
- **ROM buffer lifetime:** with `need_fullpath=false` the core reads our ROM bytes after
  `load_game`; we `mem::forget` the 16 MiB buffer (handled in `N64::new`).
- **Codesigning when bundling:** the dylib is adhoc-signed; `dlopen` works as a loose file. If
  you ship inside a notarized .app, add the
  `com.apple.security.cs.disable-library-validation` entitlement or re-sign the dylib.
- **Z-trigger id ambiguity:** mapped to `ID_L2` (mupen/parallel default). If Z feels wrong in a
  given nightly, try `ID_L` — the only thing to flip is one line in `map_button`.

---

## File-change summary

| File | Change |
|---|---|
| `src/n64.rs` | **NEW** — full libretro frontend (above). Replaces `src/emu.rs`. |
| `src/emu.rs` | Delete (or keep behind an NES flag). |
| `src/video.rs` | `W=640 H=240`; `rgba_to_i420` → `xrgb_to_i420` (BGRX, pitch-aware, letterbox); bitrate ↑3500. |
| `src/audio.rs` | Stereo Opus + 44100→48000 linear resampler; `push_f32` → `push_i16_stereo`; `OpusStreamer::new(in_rate)`. |
| `src/pipeline.rs` | `Emu`→`N64`; period from core fps; stereo drain; N64 input (held-keys → analog vectors); `start(core,rom)`. |
| `src/webrtc.rs` | Opus capability `channels:2` + stereo fmtp; (optional) stream id `"n64"`. |
| `static/index.html` | N64 keymap (arrows=stick, X/Z/C=A/B/Z, Q/E=L/R, Enter=Start, IJKL=C-buttons); 4:3 box; hint. |
| `src/main.rs` | `mod n64`; `DEFAULT_ROM`=SSB64 .z64; `DEFAULT_CORE`=parallel_n64 dylib; `start(core,rom)`. |
| `Cargo.toml` | Add `libloading = "0.8"`; `tetanes-core` optional/removed. (fallback core adds `build.rs`+`logshim.c`.) |
| `cores/` | `parallel_n64_libretro.dylib` (primary), `mupen64plus_next_libretro.dylib` (fallback) — already present. |
