//! Headless libretro frontend for N64. Loads a software-rendering libretro core
//! (parallel_n64 or mupen64plus-next, both with the angrylion software RDP), drives it
//! with NO window and NO GL context, and exposes the shape the pipeline needs.
//!
//! VERIFIED (research/n64-*.md): Super Smash Bros. (U) boots headless, emitting an XRGB8888
//! framebuffer (memory byte order B,G,R,X) via video_refresh, i16 stereo audio via
//! audio_sample_batch, hundreds of fps on Apple Silicon. The key trick: REFUSE
//! RETRO_ENVIRONMENT_SET_HW_RENDER so the core stays in pure software mode.
//!
//! libretro callbacks are bare `extern "C" fn` with no user-data, so per-instance state lives
//! in process globals. We run exactly ONE emulator on ONE dedicated OS thread; the core's own
//! worker threads (angrylion "all threads") also reach these globals. Locks are held only for a
//! memcpy/extend, never across retro_run().

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

use libloading::{Library, Symbol};

// ---------- libretro environment command ids ----------
const ENV_SET_ROTATION: c_uint = 1;
const ENV_GET_OVERSCAN: c_uint = 2;
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
const ENV_GET_INPUT_DEVICE_CAPABILITIES: c_uint = 24;
const ENV_GET_LOG_INTERFACE: c_uint = 27; // give a REAL variadic log fn or mupen SIGSEGVs
const ENV_GET_CORE_ASSETS_DIRECTORY: c_uint = 30;
const ENV_GET_SAVE_DIRECTORY: c_uint = 31;
const ENV_SET_SYSTEM_AV_INFO: c_uint = 32;
const ENV_SET_SUBSYSTEM_INFO: c_uint = 34;
const ENV_SET_CONTROLLER_INFO: c_uint = 35;
const ENV_SET_GEOMETRY: c_uint = 37;
const ENV_GET_LANGUAGE: c_uint = 39;
const ENV_GET_CORE_OPTIONS_VERSION: c_uint = 52;
const ENV_SET_CORE_OPTIONS: c_uint = 53;
const ENV_SET_CORE_OPTIONS_INTL: c_uint = 54;
const ENV_SET_CORE_OPTIONS_DISPLAY: c_uint = 55;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_GET_INPUT_BITMASKS: c_uint = 51 | 0x10000;

const PIXFMT_0RGB1555: c_uint = 0;
const PIXFMT_XRGB8888: c_uint = 1;
const PIXFMT_RGB565: c_uint = 2;

// ---------- libretro device + button/axis ids ----------
const DEV_JOYPAD: c_uint = 1;
const DEV_ANALOG: c_uint = 5;
const ANALOG_LEFT: c_uint = 0;
const ANALOG_RIGHT: c_uint = 1;
const ANALOG_X: c_uint = 0;
const ANALOG_Y: c_uint = 1;

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
pub const ID_L2: usize = 12; // N64 Z trigger on both cores
pub const ID_R2: usize = 13;

pub const AXIS_MAX: i16 = 0x7FFF;

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

// ---------- shared globals (single emulator, single thread) ----------
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
    pub lx: i16, // control stick X (+right)
    pub ly: i16, // control stick Y (+down)
    pub cx: i16, // C-stick X (right analog)
    pub cy: i16, // C-stick Y
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

// ---------- forced core options (covers BOTH cores; a core only queries its own keys) ----------
// RDP stays "angrylion" (software, required for headless). The RSP plugin is the speed/accuracy
// knob: env N64_RSP overrides it ("cxd4" = accurate LLE default; "hle" = much faster, may glitch
// on demanding games like Pokémon Stadium).
fn forced_option(key: &str) -> Option<String> {
    let rsp = std::env::var("N64_RSP").unwrap_or_else(|_| "cxd4".into());
    Some(match key {
        // ParaLLEl N64
        "parallel-n64-gfxplugin" => "angrylion".into(),
        "parallel-n64-rspplugin" => rsp,
        "parallel-n64-screensize" => "320x240".into(),
        "parallel-n64-angrylion-vioverlay" => "Filtered".into(),
        "parallel-n64-cpucore" => "dynamic_recompiler".into(),
        // mupen64plus-next (fallback core)
        "mupen64plus-rdp-plugin" => "angrylion".into(),
        "mupen64plus-rsp-plugin" => rsp,
        "mupen64plus-angrylion-multithread" => "all threads".into(),
        "mupen64plus-angrylion-sync" => "Low".into(),
        "mupen64plus-EnableFBEmulation" => "True".into(),
        "mupen64plus-cpucore" => "dynamic_recompiler".into(),
        "mupen64plus-43screensize" => "320x240".into(),
        "mupen64plus-aspect" => "4:3".into(),
        _ => return None,
    })
}

// ---------- callbacks ----------
unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        ENV_GET_CAN_DUPE | ENV_GET_OVERSCAN => {
            if !data.is_null() {
                *(data as *mut bool) = cmd == ENV_GET_CAN_DUPE;
            }
            true
        }
        ENV_SET_PIXEL_FORMAT => {
            let f = *(data as *const c_uint);
            FRAME.lock().unwrap().fmt = f;
            f == PIXFMT_0RGB1555 || f == PIXFMT_XRGB8888 || f == PIXFMT_RGB565
        }
        ENV_SET_HW_RENDER => false, // <-- REFUSE: keep the core in software (angrylion)
        ENV_GET_PREFERRED_HW_RENDER => {
            if !data.is_null() {
                *(data as *mut c_uint) = 0; // RETRO_HW_CONTEXT_NONE
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
        ENV_GET_VARIABLE => {
            let v = &mut *(data as *mut RetroVariable);
            if v.key.is_null() {
                return false;
            }
            let key = CStr::from_ptr(v.key).to_string_lossy();
            if let Some(val) = forced_option(&key) {
                // leak a CString; the core keeps polling the pointer after we return
                v.value = CString::new(val).unwrap().into_raw();
                true
            } else {
                v.value = ptr::null();
                false // core uses its own default for unforced keys
            }
        }
        ENV_GET_VARIABLE_UPDATE => {
            if !data.is_null() {
                *(data as *mut bool) = false;
            }
            true
        }
        ENV_GET_CORE_OPTIONS_VERSION => {
            if !data.is_null() {
                *(data as *mut c_uint) = 2;
            }
            true
        }
        ENV_GET_LOG_INTERFACE => {
            // Provide a REAL C-variadic log fn (logshim.c); declining leaves a NULL ptr the core
            // blindly calls -> SIGSEGV in retro_load_game (mupen64plus-next).
            extern "C" {
                fn n64_core_log();
            }
            #[repr(C)]
            struct LogCb {
                log: *const c_void,
            }
            if !data.is_null() {
                (*(data as *mut LogCb)).log = n64_core_log as *const c_void;
            }
            true
        }
        ENV_GET_INPUT_DEVICE_CAPABILITIES => {
            if !data.is_null() {
                *(data as *mut u64) = (1u64 << DEV_JOYPAD) | (1u64 << DEV_ANALOG);
            }
            true
        }
        ENV_GET_LANGUAGE => {
            if !data.is_null() {
                *(data as *mut c_uint) = 0;
            }
            true
        }
        ENV_GET_INPUT_BITMASKS => false, // make the core poll individual ids
        // acknowledge option-registration; our GET_VARIABLE does the forcing
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
        _ => false,
    }
}

unsafe extern "C" fn cb_video_refresh(data: *const c_void, w: c_uint, h: c_uint, pitch: usize) {
    let mut f = FRAME.lock().unwrap();
    f.w = w;
    f.h = h;
    f.pitch = pitch;
    if data.is_null() {
        return; // dupe frame (GET_CAN_DUPE) -> keep last buffer
    }
    let n = pitch * h as usize;
    let src = std::slice::from_raw_parts(data as *const u8, n);
    f.bytes.clear();
    f.bytes.extend_from_slice(src);
}

unsafe extern "C" fn cb_audio_sample(l: i16, r: i16) {
    let mut a = AUDIO.lock().unwrap();
    a.push(l);
    a.push(r);
}
unsafe extern "C" fn cb_audio_batch(data: *const i16, frames: usize) -> usize {
    let s = std::slice::from_raw_parts(data, frames * 2);
    AUDIO.lock().unwrap().extend_from_slice(s);
    frames
}
unsafe extern "C" fn cb_input_poll() {}
unsafe extern "C" fn cb_input_state(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16 {
    if port != 0 {
        return 0; // P1 only for now
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

// ---------- symbol types ----------
type EnvFn = unsafe extern "C" fn(c_uint, *mut c_void) -> bool;
type VideoFn = unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize);
type AudioFn = unsafe extern "C" fn(i16, i16);
type AudioBatchFn = unsafe extern "C" fn(*const i16, usize) -> usize;
type PollFn = unsafe extern "C" fn();
type InputFn = unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;
type RunFn = unsafe extern "C" fn();

pub struct N64 {
    _lib: Library, // keep the dylib mapped for the process lifetime
    run: RunFn,    // raw fn pointer (valid while _lib lives)
    pub fps: f64,
    pub sample_rate: f64,
    pub width: u32,
    pub height: u32,
}

impl N64 {
    pub fn new(core_path: &str, rom_path: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all("/tmp/n64sys").ok();
        std::fs::create_dir_all("/tmp/n64save").ok();
        unsafe {
            SYS_DIR = CString::new("/tmp/n64sys")?.into_raw();
            SAVE_DIR = CString::new("/tmp/n64save")?.into_raw();
        }

        let lib =
            unsafe { Library::new(core_path) }.map_err(|e| anyhow::anyhow!("dlopen {core_path}: {e}"))?;

        // Read the 16 MiB .z64 (native big-endian, NO byteswap). The core reads from this buffer
        // after load_game when need_fullpath==false, so we mem::forget it to keep it alive.
        let rom = std::fs::read(rom_path)?;
        let path_c = CString::new(rom_path)?;

        let (run, fps, sample_rate, width, height) = unsafe {
            let set_environment: Symbol<unsafe extern "C" fn(EnvFn)> =
                lib.get(b"retro_set_environment")?;
            let set_video: Symbol<unsafe extern "C" fn(VideoFn)> =
                lib.get(b"retro_set_video_refresh")?;
            let set_audio: Symbol<unsafe extern "C" fn(AudioFn)> =
                lib.get(b"retro_set_audio_sample")?;
            let set_audio_batch: Symbol<unsafe extern "C" fn(AudioBatchFn)> =
                lib.get(b"retro_set_audio_sample_batch")?;
            let set_poll: Symbol<unsafe extern "C" fn(PollFn)> = lib.get(b"retro_set_input_poll")?;
            let set_input: Symbol<unsafe extern "C" fn(InputFn)> =
                lib.get(b"retro_set_input_state")?;
            let init: Symbol<unsafe extern "C" fn()> = lib.get(b"retro_init")?;
            let get_info: Symbol<unsafe extern "C" fn(*mut RetroSystemInfo)> =
                lib.get(b"retro_get_system_info")?;
            let load_game: Symbol<unsafe extern "C" fn(*const RetroGameInfo) -> bool> =
                lib.get(b"retro_load_game")?;
            let get_av: Symbol<unsafe extern "C" fn(*mut RetroSystemAvInfo)> =
                lib.get(b"retro_get_system_av_info")?;
            let set_ctrl: Symbol<unsafe extern "C" fn(c_uint, c_uint)> =
                lib.get(b"retro_set_controller_port_device")?;
            // raw fn pointer for run (outlives the borrow; valid while `lib` is alive)
            let run: RunFn = *lib.get::<RunFn>(b"retro_run")?;

            // ORDER MATTERS: environment first, then media callbacks, then init.
            set_environment(cb_environment);
            set_video(cb_video_refresh);
            set_audio(cb_audio_sample);
            set_audio_batch(cb_audio_batch);
            set_poll(cb_input_poll);
            set_input(cb_input_state);
            init();

            let mut info: RetroSystemInfo = std::mem::zeroed();
            get_info(&mut info);
            let need_fullpath = info.need_fullpath;

            let gi = RetroGameInfo {
                path: path_c.as_ptr(),
                data: rom.as_ptr() as *const c_void,
                size: rom.len(),
                meta: ptr::null(),
            };
            if !load_game(&gi) {
                anyhow::bail!("retro_load_game failed (core refused the ROM)");
            }
            if !need_fullpath {
                std::mem::forget(rom);
            }
            std::mem::forget(path_c);

            let mut av: RetroSystemAvInfo = std::mem::zeroed();
            get_av(&mut av);
            set_ctrl(0, DEV_JOYPAD);

            (
                run,
                av.timing.fps,
                av.timing.sample_rate,
                av.geometry.base_width,
                av.geometry.base_height,
            )
        };

        tracing::info!(
            "N64 core loaded: fps={fps:.4} sample_rate={sample_rate:.0} base={width}x{height}"
        );

        Ok(N64 {
            _lib: lib,
            run,
            fps,
            sample_rate,
            width,
            height,
        })
    }

    /// Advance exactly one video frame (fires video/audio/input callbacks).
    pub fn clock_frame(&mut self) {
        unsafe { (self.run)() }
    }

    /// Inspect the latest framebuffer (XRGB8888 / BGRX) under lock.
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

/// Browser wire button name -> N64 action. Analog directions fold into the control/C stick so
/// a keyboard can drive the N64 pad. Returns None for unknown names (ignored).
pub enum N64Action {
    Btn(usize),
    Stick,  // a control-stick direction key (vector recomputed from held set in the pipeline)
    CStick, // a C-stick direction key
}
pub fn map_button(b: &str) -> Option<N64Action> {
    use N64Action::*;
    Some(match b {
        "A" => Btn(ID_A),
        "B" => Btn(ID_B),
        "Start" => Btn(ID_START),
        "Select" => Btn(ID_SELECT), // N64 has no Select; Game Boy does
        "Z" => Btn(ID_L2), // N64 Z trigger
        "L" => Btn(ID_L),
        "R" => Btn(ID_R),
        "StickUp" | "StickDown" | "StickLeft" | "StickRight" => Stick,
        "CUp" | "CDown" | "CLeft" | "CRight" => CStick,
        // optional digital D-pad
        "Up" => Btn(ID_UP),
        "Down" => Btn(ID_DOWN),
        "Left" => Btn(ID_LEFT),
        "Right" => Btn(ID_RIGHT),
        _ => return None,
    })
}
