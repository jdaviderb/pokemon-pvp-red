// Headless libretro frontend for Pokemon Red battle-reach + RAM-map + CCDD experiment.
// Based on the verified gb-harness template. Adds: memory accessors, savestate
// serialize/unserialize, a scriptable input runner, battle detection (D057),
// RAM-map dump, and the CCDD enemy-move-write experiment.
use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

// ---------- libretro constants ----------
const RETRO_ENVIRONMENT_GET_OVERSCAN: c_uint = 2;
const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
const RETRO_ENVIRONMENT_GET_CORE_ASSETS_DIRECTORY: c_uint = 30;
const RETRO_ENVIRONMENT_GET_LIBRETRO_PATH: c_uint = 19;
const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS: c_uint = 53;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL: c_uint = 54;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY: c_uint = 55;
const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
const RETRO_ENVIRONMENT_SET_HW_RENDER: c_uint = 14;
const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: c_uint = 32;
const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
const RETRO_ENVIRONMENT_GET_INPUT_DEVICE_CAPABILITIES: c_uint = 24;
const RETRO_ENVIRONMENT_GET_LANGUAGE: c_uint = 39;
const RETRO_ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;
const RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: c_uint = 51 | 0x10000;
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;

const RETRO_PIXEL_FORMAT_0RGB1555: c_uint = 0;
const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;
const RETRO_PIXEL_FORMAT_RGB565: c_uint = 2;

const RETRO_DEVICE_JOYPAD: c_uint = 1;

// Joypad button ids (libretro): B=0,Y=1,Select=2,Start=3,Up=4,Down=5,Left=6,Right=7,A=8
const B_B: usize = 0;
const B_SELECT: usize = 2;
const B_START: usize = 3;
const B_UP: usize = 4;
const B_DOWN: usize = 5;
const B_LEFT: usize = 6;
const B_RIGHT: usize = 7;
const B_A: usize = 8;

const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
const RETRO_MEMORY_VIDEO_RAM: c_uint = 3;

#[repr(C)]
struct retro_system_info {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}
#[repr(C)]
struct retro_game_geometry {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
}
#[repr(C)]
struct retro_system_timing {
    fps: f64,
    sample_rate: f64,
}
#[repr(C)]
struct retro_system_av_info {
    geometry: retro_game_geometry,
    timing: retro_system_timing,
}
#[repr(C)]
struct retro_game_info {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}
#[repr(C)]
struct retro_variable {
    key: *const c_char,
    value: *const c_char,
}

struct InputState {
    joypad: [bool; 16],
}
static INPUT: Mutex<InputState> = Mutex::new(InputState { joypad: [false; 16] });

struct Frame {
    w: u32,
    h: u32,
    pitch: usize,
    data: Vec<u8>,
}
static FRAME: Mutex<Frame> = Mutex::new(Frame { w: 0, h: 0, pitch: 0, data: Vec::new() });

static mut SYS_DIR: *const c_char = ptr::null();
static mut SAVE_DIR: *const c_char = ptr::null();
static mut PIXFMT: c_uint = RETRO_PIXEL_FORMAT_0RGB1555;
static FRAMES: Mutex<u64> = Mutex::new(0);

unsafe extern "C" fn cb_video_refresh(d: *const c_void, w: c_uint, h: c_uint, p: usize) {
    *FRAMES.lock().unwrap() += 1;
    if d.is_null() {
        return;
    }
    let mut f = FRAME.lock().unwrap();
    f.w = w;
    f.h = h;
    f.pitch = p;
    let bytes = p * h as usize;
    let slice = std::slice::from_raw_parts(d as *const u8, bytes);
    f.data.clear();
    f.data.extend_from_slice(slice);
}

fn dump_ppm(path: &str) {
    let f = FRAME.lock().unwrap();
    if f.w == 0 || f.h == 0 || f.data.is_empty() {
        return;
    }
    let pixfmt = unsafe { PIXFMT };
    let mut out = Vec::with_capacity((f.w * f.h * 3) as usize + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", f.w, f.h).as_bytes());
    for y in 0..f.h as usize {
        let row = y * f.pitch;
        for x in 0..f.w as usize {
            let (r, g, b) = match pixfmt {
                1 => {
                    let off = row + x * 4;
                    if off + 3 < f.data.len() {
                        (f.data[off + 2], f.data[off + 1], f.data[off])
                    } else {
                        (0, 0, 0)
                    }
                }
                2 => {
                    let off = row + x * 2;
                    if off + 1 < f.data.len() {
                        let v = u16::from_le_bytes([f.data[off], f.data[off + 1]]);
                        let r = ((v >> 11) & 0x1f) as u8;
                        let g = ((v >> 5) & 0x3f) as u8;
                        let b = (v & 0x1f) as u8;
                        ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
                    } else {
                        (0, 0, 0)
                    }
                }
                _ => {
                    let off = row + x * 2;
                    if off + 1 < f.data.len() {
                        let v = u16::from_le_bytes([f.data[off], f.data[off + 1]]);
                        let r = ((v >> 10) & 0x1f) as u8;
                        let g = ((v >> 5) & 0x1f) as u8;
                        let b = (v & 0x1f) as u8;
                        ((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2))
                    } else {
                        (0, 0, 0)
                    }
                }
            };
            out.push(r);
            out.push(g);
            out.push(b);
        }
    }
    use std::io::Write;
    if let Ok(mut fh) = std::fs::File::create(path) {
        let _ = fh.write_all(&out);
    }
}
unsafe extern "C" fn cb_audio_sample(_l: i16, _r: i16) {}
unsafe extern "C" fn cb_audio_sample_batch(_d: *const i16, frames: usize) -> usize {
    frames
}
unsafe extern "C" fn cb_input_poll() {}
unsafe extern "C" fn cb_input_state(_port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    let inp = INPUT.lock().unwrap();
    if device == RETRO_DEVICE_JOYPAD && (id as usize) < 16 && inp.joypad[id as usize] {
        1
    } else {
        0
    }
}

fn forced_option(_key: &str) -> Option<String> {
    None
}

unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        RETRO_ENVIRONMENT_GET_CAN_DUPE => {
            if !data.is_null() {
                *(data as *mut bool) = true;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            let fmt = *(data as *const c_uint);
            PIXFMT = fmt;
            fmt == RETRO_PIXEL_FORMAT_0RGB1555
                || fmt == RETRO_PIXEL_FORMAT_XRGB8888
                || fmt == RETRO_PIXEL_FORMAT_RGB565
        }
        RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY => {
            *(data as *mut *const c_char) = SYS_DIR;
            true
        }
        RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
            *(data as *mut *const c_char) = SAVE_DIR;
            true
        }
        RETRO_ENVIRONMENT_GET_CORE_ASSETS_DIRECTORY => {
            *(data as *mut *const c_char) = SYS_DIR;
            true
        }
        RETRO_ENVIRONMENT_GET_LIBRETRO_PATH => {
            *(data as *mut *const c_char) = SYS_DIR;
            true
        }
        RETRO_ENVIRONMENT_GET_VARIABLE => {
            let var = &mut *(data as *mut retro_variable);
            if var.key.is_null() {
                return false;
            }
            let key = CStr::from_ptr(var.key).to_string_lossy().to_string();
            if let Some(val) = forced_option(&key) {
                let cs = CString::new(val).unwrap();
                var.value = cs.into_raw();
                true
            } else {
                var.value = ptr::null();
                false
            }
        }
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
            if !data.is_null() {
                *(data as *mut bool) = false;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_VARIABLES
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL
        | ENV_SET_CORE_OPTIONS_V2
        | ENV_SET_CORE_OPTIONS_V2_INTL
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY => true,
        RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
            if !data.is_null() {
                *(data as *mut c_uint) = 2;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_HW_RENDER => false,
        ENV_GET_PREFERRED_HW_RENDER => {
            if !data.is_null() {
                *(data as *mut c_uint) = 0;
            }
            true
        }
        RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {
            extern "C" {
                fn n64probe_log();
            }
            #[repr(C)]
            struct LogCb {
                log: *const c_void,
            }
            (*(data as *mut LogCb)).log = n64probe_log as *const c_void;
            true
        }
        RETRO_ENVIRONMENT_GET_OVERSCAN => {
            if !data.is_null() {
                *(data as *mut bool) = false;
            }
            true
        }
        RETRO_ENVIRONMENT_GET_INPUT_BITMASKS => false,
        RETRO_ENVIRONMENT_GET_INPUT_DEVICE_CAPABILITIES => {
            if !data.is_null() {
                *(data as *mut u64) = 1u64 << RETRO_DEVICE_JOYPAD;
            }
            true
        }
        RETRO_ENVIRONMENT_GET_LANGUAGE => {
            if !data.is_null() {
                *(data as *mut c_uint) = 0;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => true,
        _ => false,
    }
}

// ---------- core handle ----------
// Raw fn pointers extracted from a leaked 'static Library; sound for process lifetime.
struct Core {
    run: unsafe extern "C" fn(),
    get_mem_data: unsafe extern "C" fn(c_uint) -> *mut c_void,
    get_mem_size: unsafe extern "C" fn(c_uint) -> usize,
    serialize_size: unsafe extern "C" fn() -> usize,
    serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    unserialize: unsafe extern "C" fn(*const c_void, usize) -> bool,
}

impl Core {
    fn frame(&self) {
        unsafe { (self.run)() }
    }
    fn wram_ptr_len(&self) -> (*mut u8, usize) {
        unsafe {
            let p = (self.get_mem_data)(RETRO_MEMORY_SYSTEM_RAM) as *mut u8;
            let n = (self.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
            (p, n)
        }
    }
    // Read a CPU address in the 0xC000..0xE000 WRAM window.
    fn rb(&self, addr: u16) -> u8 {
        let (p, n) = self.wram_ptr_len();
        let off = (addr as usize).wrapping_sub(0xC000);
        if p.is_null() || off >= n {
            return 0;
        }
        unsafe { *p.add(off) }
    }
    fn wb(&self, addr: u16, val: u8) {
        let (p, n) = self.wram_ptr_len();
        let off = (addr as usize).wrapping_sub(0xC000);
        if p.is_null() || off >= n {
            return;
        }
        unsafe { *p.add(off) = val };
    }
    fn rw_le(&self, addr: u16) -> u16 {
        // Pokemon stores HP big-endian (hi, lo) in some fields; we read both ways at call sites.
        u16::from_le_bytes([self.rb(addr), self.rb(addr.wrapping_add(1))])
    }
    fn rw_be(&self, addr: u16) -> u16 {
        u16::from_be_bytes([self.rb(addr), self.rb(addr.wrapping_add(1))])
    }
}

fn set_btn(id: usize, on: bool) {
    INPUT.lock().unwrap().joypad[id] = on;
}
fn clear_all() {
    INPUT.lock().unwrap().joypad = [false; 16];
}

// Press a button: hold `on` frames, then release for `off` frames.
fn tap(core: &Core, id: usize, on: u32, off: u32) {
    clear_all();
    set_btn(id, true);
    for _ in 0..on {
        core.frame();
    }
    clear_all();
    for _ in 0..off {
        core.frame();
    }
}

fn idle(core: &Core, n: u32) {
    clear_all();
    for _ in 0..n {
        core.frame();
    }
}

fn pos(core: &Core) -> (u8, u8, u8) {
    (core.rb(0xD35E), core.rb(0xD361), core.rb(0xD362)) // map, Y, X
}

// Take one tile-step in a direction: in Pokemon you must hold the dpad; the first
// press may just turn the character, so we hold for enough frames to commit a step.
// Returns true if (Y,X,map) changed.
fn step(core: &Core, dir: usize) -> bool {
    let before = pos(core);
    clear_all();
    set_btn(dir, true);
    for _ in 0..16 {
        core.frame();
        if pos(core) != before {
            // keep holding a few more frames to finish the tile slide
        }
    }
    clear_all();
    for _ in 0..4 {
        core.frame();
    }
    pos(core) != before
}

// Mash A to clear text boxes; returns when several A-taps produce no map/pos change
// or battle starts.
fn clear_text(core: &Core, max_taps: u32) {
    for _ in 0..max_taps {
        if battle_active(core) != 0 {
            return;
        }
        tap(core, B_A, 4, 8);
    }
}

// Walk in a given direction until we stop moving (hit a wall) or map changes or
// battle starts. Returns (steps_taken, map_changed, battle).
fn walk_until_blocked(core: &Core, dir: usize, max_steps: u32) -> (u32, bool, bool) {
    let start_map = core.rb(0xD35E);
    let mut steps = 0;
    let mut stuck = 0;
    for _ in 0..max_steps {
        if battle_active(core) != 0 {
            return (steps, false, true);
        }
        let moved = step(core, dir);
        if core.rb(0xD35E) != start_map {
            return (steps + 1, true, battle_active(core) != 0);
        }
        if moved {
            steps += 1;
            stuck = 0;
        } else {
            // could be blocked by text; tap A once then retry
            tap(core, B_A, 3, 4);
            stuck += 1;
            if stuck >= 3 {
                break;
            }
        }
    }
    (steps, false, battle_active(core) != 0)
}

fn battle_active(core: &Core) -> u8 {
    core.rb(0xD057)
}

fn main() {
    let core_path = std::env::args().nth(1).expect("usage: probe <core> <rom> <mode>");
    let rom_path = std::env::args().nth(2).expect("usage: probe <core> <rom> <mode>");
    let mode = std::env::args().nth(3).unwrap_or_else(|| "reach".to_string());

    unsafe {
        SYS_DIR = CString::new("/tmp/ccdd-probe/systemdir").unwrap().into_raw();
        SAVE_DIR = CString::new("/tmp/ccdd-probe/savedir").unwrap().into_raw();
    }

    let lib = unsafe { libloading::Library::new(&core_path).expect("load core") };
    // leak the lib so 'static symbols are sound for the process lifetime
    let lib: &'static libloading::Library = Box::leak(Box::new(lib));

    let core = unsafe {
        let set_environment: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool)> =
            lib.get(b"retro_set_environment").unwrap();
        let set_video: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize))> =
            lib.get(b"retro_set_video_refresh").unwrap();
        let set_audio: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn(i16, i16))> =
            lib.get(b"retro_set_audio_sample").unwrap();
        let set_audio_batch: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn(*const i16, usize) -> usize)> =
            lib.get(b"retro_set_audio_sample_batch").unwrap();
        let set_input_poll: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn())> =
            lib.get(b"retro_set_input_poll").unwrap();
        let set_input_state: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16)> =
            lib.get(b"retro_set_input_state").unwrap();
        let init: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_init").unwrap();
        let load_game: libloading::Symbol<unsafe extern "C" fn(*const retro_game_info) -> bool> =
            lib.get(b"retro_load_game").unwrap();
        let get_av: libloading::Symbol<unsafe extern "C" fn(*mut retro_system_av_info)> =
            lib.get(b"retro_get_system_av_info").unwrap();
        let set_ctrl: libloading::Symbol<unsafe extern "C" fn(c_uint, c_uint)> =
            lib.get(b"retro_set_controller_port_device").unwrap();
        let get_sys: libloading::Symbol<unsafe extern "C" fn(*mut retro_system_info)> =
            lib.get(b"retro_get_system_info").unwrap();

        set_environment(cb_environment);
        set_video(cb_video_refresh);
        set_audio(cb_audio_sample);
        set_audio_batch(cb_audio_sample_batch);
        set_input_poll(cb_input_poll);
        set_input_state(cb_input_state);
        init();

        let mut sinfo: retro_system_info = std::mem::zeroed();
        get_sys(&mut sinfo);

        let rom_bytes = std::fs::read(&rom_path).expect("read rom");
        let path_c = CString::new(rom_path.clone()).unwrap();
        let gi = retro_game_info {
            path: path_c.as_ptr(),
            data: rom_bytes.as_ptr() as *const c_void,
            size: rom_bytes.len(),
            meta: ptr::null(),
        };
        // leak rom bytes so the data ptr stays valid (some cores keep referencing it)
        let _ = Box::leak(Box::new(rom_bytes));
        let ok = load_game(&gi);
        println!("retro_load_game -> {}", ok);
        assert!(ok, "load failed");

        let mut av: retro_system_av_info = std::mem::zeroed();
        get_av(&mut av);
        set_ctrl(0, RETRO_DEVICE_JOYPAD);

        let run: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_run").unwrap();
        let get_mem_data: libloading::Symbol<unsafe extern "C" fn(c_uint) -> *mut c_void> =
            lib.get(b"retro_get_memory_data").unwrap();
        let get_mem_size: libloading::Symbol<unsafe extern "C" fn(c_uint) -> usize> =
            lib.get(b"retro_get_memory_size").unwrap();
        let serialize_size: libloading::Symbol<unsafe extern "C" fn() -> usize> =
            lib.get(b"retro_serialize_size").unwrap();
        let serialize: libloading::Symbol<unsafe extern "C" fn(*mut c_void, usize) -> bool> =
            lib.get(b"retro_serialize").unwrap();
        let unserialize: libloading::Symbol<unsafe extern "C" fn(*const c_void, usize) -> bool> =
            lib.get(b"retro_unserialize").unwrap();

        Core {
            run: *run.into_raw(),
            get_mem_data: *get_mem_data.into_raw(),
            get_mem_size: *get_mem_size.into_raw(),
            serialize_size: *serialize_size.into_raw(),
            serialize: *serialize.into_raw(),
            unserialize: *unserialize.into_raw(),
        }
    };

    // report memory regions
    unsafe {
        let s_sys = (core.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
        let s_save = (core.get_mem_size)(RETRO_MEMORY_SAVE_RAM);
        let s_vram = (core.get_mem_size)(RETRO_MEMORY_VIDEO_RAM);
        let ser = (core.serialize_size)();
        println!(
            "MEM REGIONS: SYSTEM_RAM={} (0x{:x}) SAVE_RAM={} VIDEO_RAM={} | serialize_size={}",
            s_sys, s_sys, s_save, s_vram, ser
        );
    }

    match mode.as_str() {
        "reach" => run_reach(&core, false),
        "reach-save" => run_reach(&core, true),
        "fromstate" => run_fromstate(&core),
        "ccdd" => run_ccdd_from_state(&core),
        "ccdd2" => run_ccdd_definitive(&core),
        "ccddobs" => run_ccdd_observe(&core),
        "escape" => run_escape_room(&core),
        "introscan" => run_introscan(&core),
        "stairs" => run_stairs(&core),
        "warptest" => run_warptest(&core),
        "stairs2" => run_stairs2(&core),
        "flood" => run_flood(&core),
        "edgewarp" => run_edgewarp(&core),
        "bedstate" => run_bedstate(&core),
        "labwalk" => run_labwalk(&core),
        "randwalk" => run_randwalk(&core),
        "fullreach" => run_fullreach(&core),
        "introcap" => run_introcap(&core),
        "injtest" => run_injtest(&core),
        _ => {
            eprintln!("unknown mode {}", mode);
        }
    }
}

// Print the verified RAM map block.
fn dump_battle_ram(core: &Core, label: &str) {
    println!("---- BATTLE RAM DUMP [{}] ----", label);
    let d057 = core.rb(0xD057);
    let d05a = core.rb(0xD05A);
    let ccd5 = core.rb(0xCCD5);
    let ccdc = core.rb(0xCCDC);
    let ccdd = core.rb(0xCCDD);
    println!(
        "D057 wIsInBattle = {} | D05A battleType = {} | CCD5 turns = {} | CCDC playerMove = {} | CCDD enemyMove = {}",
        d057 as i8, d05a, ccd5, ccdc, ccdd
    );
    // enemy active mon CFE5..D008
    println!("ENEMY active mon (CFE5-D008):");
    println!("  CFE5 species = {}", core.rb(0xCFE5));
    println!("  CFE6-CFE7 HP (be) = {} (le={})", core.rw_be(0xCFE6), core.rw_le(0xCFE6));
    println!("  CFE8 level = {}", core.rb(0xCFE8));
    println!("  CFEA status = {}", core.rb(0xCFEA));
    println!(
        "  CFED-CFF0 moves = [{}, {}, {}, {}]",
        core.rb(0xCFED), core.rb(0xCFEE), core.rb(0xCFEF), core.rb(0xCFF0)
    );
    println!(
        "  CFF4-CFFD stats: maxHP(be)={} atk={} def={} spd={} spc={}",
        core.rw_be(0xCFF4),
        core.rw_be(0xCFF6),
        core.rw_be(0xCFF8),
        core.rw_be(0xCFFA),
        core.rw_be(0xCFFC)
    );
    println!(
        "  CFFE-D001 PP = [{}, {}, {}, {}]",
        core.rb(0xCFFE), core.rb(0xCFFF), core.rb(0xD000), core.rb(0xD001)
    );
    // player active mon D009..D030
    println!("PLAYER active mon (D009-D030):");
    println!("  D009 species = {}", core.rb(0xD009));
    println!("  D015-D016 HP (be) = {} (le={})", core.rw_be(0xD015), core.rw_le(0xD015));
    println!(
        "  D01C-D01F moves = [{}, {}, {}, {}]",
        core.rb(0xD01C), core.rb(0xD01D), core.rb(0xD01E), core.rb(0xD01F)
    );
    println!("  D022 level = {}", core.rb(0xD022));
    println!(
        "  D023-D02C stats: maxHP(be)={} atk={} def={} spd={} spc={}",
        core.rw_be(0xD023),
        core.rw_be(0xD025),
        core.rw_be(0xD027),
        core.rw_be(0xD029),
        core.rw_be(0xD02B)
    );
    println!(
        "  D02D-D030 PP = [{}, {}, {}, {}]",
        core.rb(0xD02D), core.rb(0xD02E), core.rb(0xD02F), core.rb(0xD030)
    );
    // party/enemy counts
    println!(
        "PARTY: D163 count = {} | species D164-D169 = [{},{},{},{},{},{}]",
        core.rb(0xD163),
        core.rb(0xD164), core.rb(0xD165), core.rb(0xD166),
        core.rb(0xD167), core.rb(0xD168), core.rb(0xD169)
    );
    println!(
        "ENEMY PARTY: D89C count = {} | species D89D-D8A2 = [{},{},{},{},{},{}]",
        core.rb(0xD89C),
        core.rb(0xD89D), core.rb(0xD89E), core.rb(0xD89F),
        core.rb(0xD8A0), core.rb(0xD8A1), core.rb(0xD8A2)
    );
    println!("--------------------------------");
}

fn save_state(core: &Core, path: &str) -> bool {
    unsafe {
        let n = (core.serialize_size)();
        let mut buf = vec![0u8; n];
        let ok = (core.serialize)(buf.as_mut_ptr() as *mut c_void, n);
        if ok {
            std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).ok();
            std::fs::write(path, &buf).expect("write state");
            println!("SAVESTATE: wrote {} bytes -> {}", n, path);
        } else {
            println!("SAVESTATE: retro_serialize FAILED");
        }
        ok
    }
}

fn load_state(core: &Core, path: &str) -> bool {
    let buf = std::fs::read(path).expect("read state");
    unsafe {
        let ok = (core.unserialize)(buf.as_ptr() as *const c_void, buf.len());
        println!("LOADSTATE: {} bytes from {} -> unserialize {}", buf.len(), path, ok);
        ok
    }
}

// Navigation: try hard to reach the rival battle, polling D057.
fn run_reach(core: &Core, do_save: bool) {
    let frames_env = |k: &str, d: u32| -> u32 {
        std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
    };

    // Phase 0: warm up
    idle(core, 60);

    // Phase 1: boot through logos + title -> NEW GAME. Mash START then A.
    let boot_taps = frames_env("BOOT_TAPS", 90);
    println!("PHASE1 boot-mash: {} taps of START/A", boot_taps);
    for i in 0..boot_taps {
        if battle_active(core) != 0 {
            println!("  battle during boot at tap {}!", i);
            break;
        }
        // alternate start and A to get through copyright, title, menu
        let btn = if i % 3 == 0 { B_START } else { B_A };
        tap(core, btn, 4, 8);
    }
    println!(
        "  after boot: map=D35E={} Y=D361={} X=D362={} D057={}",
        core.rb(0xD35E), core.rb(0xD361), core.rb(0xD362), battle_active(core)
    );
    dump_ppm("/tmp/ccdd-probe/after_boot.ppm");

    // Phase 2: Oak intro + naming. Mash A until the player gains overworld control
    // in the bedroom. We detect control by probing a DOWN step and seeing Y change.
    let intro_taps = frames_env("INTRO_TAPS", 400);
    println!("PHASE2 intro/naming-mash until control: up to {} A taps", intro_taps);
    let mut got_control = false;
    for i in 0..intro_taps {
        if battle_active(core) != 0 {
            println!("  battle during intro at tap {}!", i);
            break;
        }
        // A with a cadence the text engine likes: hold 6, release 6.
        tap(core, B_A, 6, 6);
        if i % 10 == 0 {
            // probe for control: try a quick UP/DOWN wiggle
            let before = pos(core);
            clear_all();
            set_btn(B_DOWN, true);
            for _ in 0..10 { core.frame(); }
            clear_all();
            for _ in 0..4 { core.frame(); }
            let moved_down = pos(core) != before;
            // step back up to keep position stable
            if moved_down {
                got_control = true;
                println!("   CONTROL GAINED at intro tap {} pos={:?}", i, pos(core));
                break;
            }
        }
        if i % 40 == 0 {
            println!(
                "   intro tap {} : map={} Y={} X={}",
                i, core.rb(0xD35E), core.rb(0xD361), core.rb(0xD362)
            );
        }
    }
    println!(
        "  after intro (control={}): map=D35E={} Y={} X={} D057={}",
        got_control, core.rb(0xD35E), core.rb(0xD361), core.rb(0xD362), battle_active(core)
    );
    dump_ppm("/tmp/ccdd-probe/after_intro.ppm");

    // Close any open menu (defensive).
    println!("  closing menus with B...");
    for _ in 0..4 {
        tap(core, B_B, 4, 8);
    }
    dump_ppm("/tmp/ccdd-probe/after_close.ppm");

    // Phase 3: navigate bedroom -> downstairs -> outside -> Oak cutscene -> lab.
    // Map ids: PLAYERS_HOUSE_2F=38, 1F=37, PALLET_TOWN=0, OAKS_LAB=40.
    let _nav_loops = frames_env("NAV_LOOPS", 400);
    println!("PHASE3 navigate (directed): start {:?}", pos(core));
    let mut maps_seen: Vec<u8> = vec![core.rb(0xD35E)];
    let note_map = |core: &Core, seen: &mut Vec<u8>| {
        let m = core.rb(0xD35E);
        if !seen.contains(&m) {
            seen.push(m);
            println!("  >>> NEW MAP {} at {:?}", m, pos(core));
        }
    };

    // The directed plan: a sequence of (direction, max_steps, label).
    // We do NOT mash A inside the room (avoids opening PC/menus). We only mash A
    // once a map change indicates a cutscene/door (handled after the plan).
    let plan: &[(usize, u32, &str)] = &[
        (B_LEFT, 4, "2F left toward stairs col"),
        (B_DOWN, 8, "2F down to/through stairs"),
        (B_DOWN, 8, "1F down toward door"),
        (B_LEFT, 4, "1F left toward door"),
        (B_DOWN, 6, "1F down out the door"),
        (B_DOWN, 8, "Pallet down (Oak intercept)"),
    ];
    for (di, (dir, maxs, label)) in plan.iter().enumerate() {
        if battle_active(core) != 0 {
            break;
        }
        let dname = match *dir { B_UP=>"UP",B_DOWN=>"DOWN",B_LEFT=>"LEFT",B_RIGHT=>"RIGHT",_=>"?" };
        let before_map = core.rb(0xD35E);
        let (steps, mapchg, batt) = walk_until_blocked(core, *dir, *maxs);
        println!(
            "  [{}] {} ({}): steps={} mapchg={} batt={} now={:?}",
            di, label, dname, steps, mapchg, batt, pos(core)
        );
        dump_ppm(&format!("/tmp/ccdd-probe/nav_leg{}.ppm", di));
        note_map(core, &mut maps_seen);
        // If a map change happened, a cutscene/text may be up: clear it with A.
        if core.rb(0xD35E) != before_map {
            clear_text(core, 20);
            note_map(core, &mut maps_seen);
        }
        if battle_active(core) != 0 {
            break;
        }
    }

    // Broader wander: out of the house, Oak's cutscene fires when stepping into the
    // grass north of the player's house. Mash A through it; ends in the lab.
    if battle_active(core) == 0 {
        println!("PHASE3b wander to provoke Oak cutscene / reach lab");
        let dirs = [B_UP, B_UP, B_UP, B_LEFT, B_DOWN, B_RIGHT];
        for i in 0..300u32 {
            if battle_active(core) != 0 {
                println!("  *** BATTLE at wander {} ***", i);
                break;
            }
            let before_map = core.rb(0xD35E);
            let d = dirs[(i as usize) % dirs.len()];
            step(core, d);
            // only mash A if we changed map (entered cutscene/building)
            if core.rb(0xD35E) != before_map {
                println!("   wander {}: MAP CHANGE -> {:?}, clearing text", i, pos(core));
                clear_text(core, 25);
            }
            note_map(core, &mut maps_seen);
            if i % 30 == 0 {
                println!("   wander {} : {:?} party={}", i, pos(core), core.rb(0xD163));
            }
        }
    }
    println!("  maps seen during nav: {:?}", maps_seen);
    println!(
        "  final: map={} Y={} X={} party_count={} D057={}",
        core.rb(0xD35E), core.rb(0xD361), core.rb(0xD362), core.rb(0xD163), battle_active(core)
    );
    dump_ppm("/tmp/ccdd-probe/nav_final.ppm");

    if battle_active(core) != 0 {
        // settle a few frames so battle structs are populated
        idle(core, 30);
        dump_battle_ram(core, "reach-battle");
        if do_save {
            save_state(core, "~/pokemon-pvp-red/states/battle.state");
        }
    } else {
        println!("NO BATTLE REACHED via blind nav.");
    }
}

// Scan the intro: mash A and dump a screenshot every N taps so we can SEE where
// the NEW GAME menu, the rival/own name menus, and the keyboard appear.
fn run_introscan(core: &Core) {
    idle(core, 60);
    // boot to title + NEW GAME
    for i in 0..90u32 {
        let btn = if i % 3 == 0 { B_START } else { B_A };
        tap(core, btn, 4, 8);
    }
    dump_ppm("/tmp/ccdd-probe/scan_000.ppm");
    println!("scan 0 (post-boot): {:?}", pos(core));
    for i in 1..=60u32 {
        // 5 A-taps per checkpoint
        for _ in 0..5 { tap(core, B_A, 6, 6); }
        dump_ppm(&format!("/tmp/ccdd-probe/scan_{:03}.ppm", i*5));
        println!("scan {} : pos={:?} map={}", i*5, pos(core), core.rb(0xD35E));
    }
}

// Boot + intro until overworld control gained in the bedroom. Returns true if control.
// Calibrated from introscan: bedroom control is reached ~250 pure-A taps after boot.
// We mash A (no directional input, which would corrupt the name keyboard), then
// probe by trying to walk LEFT (player spawns facing the SNES with open floor left).
fn boot_to_bedroom(core: &Core) -> bool {
    idle(core, 60);
    // boot mash: START + A to clear logos/title and pick NEW GAME
    for i in 0..90u32 {
        if battle_active(core) != 0 { break; }
        let btn = if i % 3 == 0 { B_START } else { B_A };
        tap(core, btn, 4, 8);
    }
    // intro/naming: PURE A only. After each batch, probe LEFT for control.
    let batch_taps = frames_env_u32("BOOT_A_TAPS", 260);
    let mut tapped = 0u32;
    while tapped < 600 {
        // mash A in small batches
        for _ in 0..10 { tap(core, B_A, 6, 6); }
        tapped += 10;
        if tapped < batch_taps {
            continue;
        }
        // Probe: clear any examine-text with B, then try LEFT, then UP-back.
        tap(core, B_B, 4, 6);
        let before = pos(core);
        clear_all();
        set_btn(B_LEFT, true);
        for _ in 0..14 { core.frame(); }
        clear_all();
        for _ in 0..4 { core.frame(); }
        if pos(core) != before {
            println!("CONTROL GAINED after {} A-taps via LEFT pos={:?}", tapped, pos(core));
            return true;
        }
    }
    false
}

fn frames_env_u32(k: &str, d: u32) -> u32 {
    std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
}

fn dname(d: usize) -> &'static str {
    match d { B_UP=>"UP",B_DOWN=>"DOWN",B_LEFT=>"LEFT",B_RIGHT=>"RIGHT",_=>"?" }
}

// Systematically escape the bedroom: try to find the staircase warp by exploring.
fn run_escape_room(core: &Core) {
    if !boot_to_bedroom(core) {
        println!("FAILED to gain control in bedroom");
        return;
    }
    // Close menus defensively
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    println!("In bedroom at {:?}. Exploring for the staircase warp...", pos(core));

    let start_map = core.rb(0xD35E);

    // First: map the full walkable bounding box by walking to each extreme.
    let go = |core: &Core, d: usize, n: u32| {
        for _ in 0..n {
            if !step(core, d) || core.rb(0xD35E) != start_map { break; }
        }
    };
    // Go to top-left corner
    go(core, B_UP, 10);
    go(core, B_LEFT, 10);
    println!("top-left corner ~= {:?}", pos(core));
    dump_ppm("/tmp/ccdd-probe/escape_topleft.ppm");

    if core.rb(0xD35E) != start_map {
        println!("  WARP during corner-seek -> map {}", core.rb(0xD35E));
    } else {
        // Sweep the left column (X minimal) at every Y from top to bottom, trying
        // LEFT (into the wall/stairs) and UP/DOWN, watching for warp.
        for sweep in 0..12u32 {
            if core.rb(0xD35E) != start_map { break; }
            let before = pos(core);
            // try LEFT
            step(core, B_LEFT);
            if core.rb(0xD35E) != start_map { println!("  WARP via LEFT at {:?}", before); break; }
            // try UP
            step(core, B_UP);
            if core.rb(0xD35E) != start_map { println!("  WARP via UP at {:?}", before); break; }
            // try DOWN
            step(core, B_DOWN);
            if core.rb(0xD35E) != start_map { println!("  WARP via DOWN at {:?}", before); break; }
            println!("  sweep {} : at {:?} (before {:?})", sweep, pos(core), before);
            dump_ppm(&format!("/tmp/ccdd-probe/escape_sweep{}.ppm", sweep));
            // move down one to try next row
            go(core, B_DOWN, 1);
            go(core, B_LEFT, 3);
        }
    }
    println!("escape end: map={} pos={:?}", core.rb(0xD35E), pos(core));
    dump_ppm("/tmp/ccdd-probe/escape_final.ppm");
}

// Hold a direction for a long time (continuous walk, like holding the dpad).
// Returns map id after. Useful for stairs/door warps that need sustained motion.
fn hold_dir(core: &Core, dir: usize, frames: u32) -> (u8,u8,u8) {
    clear_all();
    set_btn(dir, true);
    for _ in 0..frames { core.frame(); }
    clear_all();
    for _ in 0..6 { core.frame(); }
    pos(core)
}

fn run_stairs(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);
    println!("control at {:?}", pos(core));

    // Go to the left wall, then walk DOWN the left column with a long continuous
    // hold so we actually slide onto the staircase warp tile.
    for _ in 0..8 { step(core, B_LEFT); if core.rb(0xD35E)!=start_map {break;} }
    for _ in 0..8 { step(core, B_UP);   if core.rb(0xD35E)!=start_map {break;} }
    println!("at left-top {:?}", pos(core));
    // Now hold DOWN continuously for a long time (the staircase is at the bottom-left).
    for attempt in 0..6 {
        let p = hold_dir(core, B_DOWN, 60);
        println!("  hold DOWN attempt {} -> {:?}", attempt, p);
        dump_ppm(&format!("/tmp/ccdd-probe/stairs_down{}.ppm", attempt));
        if p.0 != start_map { println!("  *** WARP via DOWN -> map {} ***", p.0); break; }
        // nudge left then retry
        hold_dir(core, B_LEFT, 30);
    }
    // Reach the left wall top, then walk DOWN the X=0 column. The staircase is at
    // the bottom of column X=0. We single-step DOWN and check warp after each step.
    hold_dir(core, B_UP, 60);
    hold_dir(core, B_LEFT, 60);
    println!("at left wall {:?}", pos(core));
    // Walk down to the bottom of column 0 (Y=5), then test EVERY direction with a
    // long continuous hold to find the warp out (stairs auto-walk you down in Gen1).
    hold_dir(core, B_DOWN, 60);
    println!("bottom of col0 = {:?}", pos(core));
    let test = [(B_DOWN,"DOWN"),(B_LEFT,"LEFT"),(B_UP,"UP"),(B_RIGHT,"RIGHT")];
    for (d, nm) in test {
        if core.rb(0xD35E) != start_map { break; }
        let before = pos(core);
        let p = hold_dir(core, d, 100);
        println!("  long {} from {:?} -> {:?} (mapchg={})", nm, before, p, p.0!=start_map);
        if p.0 != start_map { println!("  *** WARP via {} -> map {} ***", nm, p.0); break; }
        // return to bottom-left for next test
        hold_dir(core, B_LEFT, 60);
        hold_dir(core, B_DOWN, 60);
    }
    // Also probe with A on the stair tiles (some warps fire only on interact)
    if core.rb(0xD35E) == start_map {
        for (d,_nm) in test {
            hold_dir(core, d, 24);
            tap(core, B_A, 6, 10);
            if core.rb(0xD35E) != start_map { println!("  *** WARP via A after move ***"); break; }
        }
    }
    println!("stairs end: map={} pos={:?}", core.rb(0xD35E), pos(core));
    dump_ppm("/tmp/ccdd-probe/stairs_final.ppm");
}

// Explore the current map with a pseudo-random walk (proper turn-then-step) until
// the map id changes, a battle starts, or we run out of budget. A is mashed
// periodically to clear cutscene/door text. Returns the reason for stopping.
// reason: 0=budget exhausted, 1=map changed, 2=battle, 3=party grew
fn explore_until_change(core: &Core, rng: &mut u32, budget: u32, mash_a: bool, bias: usize) -> (u8, &'static str) {
    let start_map = core.rb(0xD35E);
    let start_party = core.rb(0xD163);
    // Weighted direction bag: include the bias direction extra times so the walk
    // drifts toward an exit (e.g. DOWN toward the house door / out of town).
    let mut bag = vec![B_UP, B_DOWN, B_LEFT, B_RIGHT];
    if bias != usize::MAX {
        for _ in 0..4 { bag.push(bias); } // 4x weight toward exit direction
    }
    for i in 0..budget {
        if core.rb(0xD35E) != start_map { return (1, "map-change"); }
        if battle_active(core) != 0 { return (2, "battle"); }
        if core.rb(0xD163) != start_party { return (3, "party-change"); }
        *rng ^= *rng << 13; *rng ^= *rng >> 17; *rng ^= *rng << 5;
        let d = bag[(*rng as usize) % bag.len()];
        move_tile(core, d);
        if mash_a && i % 5 == 0 { tap(core, B_A, 4, 6); }
    }
    (0, "budget")
}

// Mash A for a while to drive forced cutscenes / dialog (e.g. Oak intercept).
fn drive_cutscene(core: &Core, taps: u32) {
    for _ in 0..taps {
        if battle_active(core) != 0 { return; }
        tap(core, B_A, 5, 7);
    }
}

// Full pipeline: boot -> bedroom -> 1F -> Pallet -> Oak cutscene -> lab -> starter
// -> rival battle. Uses random-walk room traversal (which provably escapes rooms).
fn run_fullreach(core: &Core) {
    let do_save = std::env::var("SAVE").is_ok();
    if !boot_to_bedroom(core) { println!("FAIL: no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let mut rng: u32 = std::env::var("SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x1234_5678);
    println!("FULLREACH start: {:?}", pos(core));

    let mut last_map = core.rb(0xD35E);
    for phase in 0..24u32 {
        if battle_active(core) != 0 { break; }
        // Choose an exit-bias per map:
        //  2F(38): leave via stairs -> no strong bias (random finds them)
        //  1F(37): door is at the bottom -> bias DOWN
        //  Pallet(0): grass/lab to the north -> bias UP
        //  Oak's Lab(40): ball table is north -> bias UP
        let cur = core.rb(0xD35E);
        let bias = match cur {
            37 => B_DOWN,
            0 => B_UP,
            40 => B_UP,
            _ => usize::MAX,
        };
        let (reason, name) = explore_until_change(core, &mut rng, 1200, true, bias);
        let m = core.rb(0xD35E);
        println!("  phase {}: stop={} ({}) map {} -> {} pos={:?} party={}",
            phase, reason, name, last_map, m, pos(core), core.rb(0xD163));
        dump_ppm(&format!("/tmp/ccdd-probe/fr_phase{}.ppm", phase));
        if reason == 2 { break; } // battle
        if reason == 0 {
            // stuck on a map with no exit found OR sitting in dialog: mash A hard.
            println!("    budget exhausted on map {}; driving cutscene/A", m);
            drive_cutscene(core, 60);
            if battle_active(core) != 0 { break; }
        }
        last_map = m;
        // After any transition, clear possible cutscene text.
        drive_cutscene(core, 12);
    }

    let inb = battle_active(core);
    println!("FULLREACH end: map={} pos={:?} party={} D057={}",
        core.rb(0xD35E), pos(core), core.rb(0xD163), inb as i8);
    dump_ppm("/tmp/ccdd-probe/fr_end.ppm");
    if inb != 0 {
        // Drive the battle intro (text + "sent out" animations) until the player's
        // active mon HP (D015) is populated AND the move-selection menu opens.
        println!("driving battle intro to move-selection...");
        let mut opened = false;
        for i in 0..200u32 {
            tap(core, B_A, 5, 7);
            let php = core.rw_be(0xD015);
            let elev = core.rb(0xCFE8);
            if i % 10 == 0 {
                println!("  intro tap {}: D057={} playerHP={} enemyLvl={} CCD5={}",
                    i, core.rb(0xD057), php, elev, core.rb(0xCCD5));
            }
            // FIGHT menu open heuristic: player HP populated and level sane.
            if php > 0 && core.rb(0xD022) > 0 && core.rb(0xD022) <= 100 {
                // a few more taps to surface the action menu, then stop
                opened = true;
                println!("  battle structs populated at intro tap {}", i);
                break;
            }
        }
        // Now navigate to FIGHT > first move selection screen: press A to choose FIGHT
        // then we are at the move list. Capture state HERE (move-selection open).
        idle(core, 8);
        dump_ppm("/tmp/ccdd-probe/fr_battle.ppm");
        dump_battle_ram(core, "fullreach-battle (intro driven)");
        println!("opened={}", opened);
        if do_save {
            save_state(core, "~/pokemon-pvp-red/states/battle.state");
        }
    }
}

// Definitive test: a long pseudo-random walk with proper turn-then-step + A mash,
// watching for ANY map change or battle. Conclusively tells us if scripted nav can
// ever leave the bedroom.
fn run_randwalk(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);
    println!("randwalk start {:?}", pos(core));
    let dirs = [B_UP,B_DOWN,B_LEFT,B_RIGHT];
    let mut rng: u32 = 0x1234_5678;
    let mut visited: std::collections::HashSet<(u8,u8)> = std::collections::HashSet::new();
    for i in 0..1500u32 {
        if core.rb(0xD35E) != start_map {
            println!("  *** MAP CHANGE at step {} -> map {} pos={:?} ***", i, core.rb(0xD35E), pos(core));
            break;
        }
        if battle_active(core) != 0 {
            println!("  *** BATTLE at step {} ***", i);
            break;
        }
        rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5;
        let d = dirs[(rng as usize) % 4];
        move_tile(core, d);
        visited.insert((pos(core).1, pos(core).2));
        // occasionally press A (in case the warp is a "press A on stairs" or to clear)
        if i % 7 == 0 { tap(core, B_A, 4, 6); }
        if i % 200 == 0 { println!("  step {} pos={:?} tiles={}", i, pos(core), visited.len()); }
    }
    let mut v: Vec<_> = visited.iter().cloned().collect(); v.sort();
    println!("randwalk visited {} distinct tiles: {:?}", v.len(), v);
    println!("randwalk end: map={} pos={:?}", core.rb(0xD35E), pos(core));
}

// Capture a controllable-bedroom savestate + full out-of-battle RAM snapshot.
fn run_bedstate(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    println!("bedroom control at {:?} party={}", pos(core), core.rb(0xD163));
    save_state(core, "~/pokemon-pvp-red/states/bedroom.state");
    // round-trip verify
    let p1 = pos(core);
    if load_state(core, "~/pokemon-pvp-red/states/bedroom.state") {
        println!("round-trip pos {:?} -> {:?} (match={})", p1, pos(core), p1==pos(core));
    }
}

// After we (somehow) reach the lab, walk to the ball table and grab a starter to
// trigger the rival battle. (Driver kept for completeness once escape is solved.)
fn run_labwalk(core: &Core) {
    let path = "~/pokemon-pvp-red/states/lab.state";
    if std::path::Path::new(path).exists() {
        load_state(core, path);
    } else {
        println!("no lab.state; cannot labwalk");
        return;
    }
    dump_battle_ram(core, "labwalk-start");
}

// At each of the staircase-adjacent tiles, face the wall (turn) then try to step,
// using the explicit turn-then-step mechanic. Report every attempt.
fn run_edgewarp(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);

    // helper: greedily go to a target (Y,X) inside the room
    let goto = |core: &Core, ty: u8, tx: u8| {
        for _ in 0..30 {
            let (_m,y,x) = pos(core);
            if y==ty && x==tx { return; }
            if core.rb(0xD35E)!=start_map { return; }
            if x > tx { move_tile(core, B_LEFT); }
            else if x < tx { move_tile(core, B_RIGHT); }
            else if y > ty { move_tile(core, B_UP); }
            else if y < ty { move_tile(core, B_DOWN); }
        }
    };

    // Candidate (tile, direction-into-stairs)
    let cands: &[(u8,u8,usize,&str)] = &[
        (5,0,B_DOWN,"DOWN"),
        (7,1,B_LEFT,"LEFT"),
        (6,1,B_LEFT,"LEFT"),
        (5,1,B_LEFT,"LEFT"),
        (4,0,B_DOWN,"DOWN"),
        (2,0,B_UP,"UP"),
        (2,0,B_LEFT,"LEFT"),
    ];
    for &(ty,tx,d,nm) in cands {
        if core.rb(0xD35E)!=start_map { break; }
        goto(core, ty, tx);
        let at = pos(core);
        // turn to face the wall: tap once (turn), then tap again (step), then a 3rd hold
        tap(core, d, 6, 4);
        tap(core, d, 6, 4);
        let p2 = pos(core);
        let (moved, mapchg) = move_tile(core, d);
        println!("  target(Y={},X={}) reached={:?} face+step {} -> after-face {:?} moved={} mapchg={} now={:?} map={}",
            ty, tx, at, nm, p2, moved, mapchg, pos(core), core.rb(0xD35E));
        if core.rb(0xD35E)!=start_map { println!("  *** WARP! ***"); break; }
    }
    dump_ppm("/tmp/ccdd-probe/edgewarp.ppm");
    println!("edgewarp end: map={} {:?}", core.rb(0xD35E), pos(core));
}

// A proper "press to turn, press again to step" move: tap dir, check; if no move,
// tap dir again (now facing it, will step). Returns (moved, mapchanged).
fn move_tile(core: &Core, dir: usize) -> (bool, bool) {
    let m0 = core.rb(0xD35E);
    let p0 = pos(core);
    // first tap (may only turn)
    tap(core, dir, 6, 4);
    if core.rb(0xD35E) != m0 { return (true, true); }
    if pos(core) != p0 { return (true, false); }
    // second tap (commit step)
    tap(core, dir, 10, 4);
    if core.rb(0xD35E) != m0 { return (true, true); }
    (pos(core) != p0, false)
}

// Flood-fill the room with the proper turn-then-step mechanic; at every tile try
// all 4 directions, recording any tile that produces a map change (the warp).
fn run_flood(core: &Core) {
    use std::collections::HashSet;
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);
    println!("flood start at {:?}", pos(core));

    // DFS over reachable tiles. State = (Y,X). We physically navigate by re-walking
    // from current pos toward a target using greedy steps (small room, fine).
    let mut visited: HashSet<(u8,u8)> = HashSet::new();
    let dirs = [B_UP,B_DOWN,B_LEFT,B_RIGHT];
    let mut stack: Vec<(u8,u8)> = vec![(pos(core).1, pos(core).2)];
    let mut warp_found = false;
    let mut steps = 0u32;
    while let Some(_t) = stack.pop() {
        if warp_found || steps > 400 { break; }
        let cur = (pos(core).1, pos(core).2);
        if visited.contains(&cur) { continue; }
        visited.insert(cur);
        for &d in dirs.iter() {
            steps += 1;
            let before = pos(core);
            let (moved, mapchg) = move_tile(core, d);
            if mapchg {
                println!("  *** WARP! from (Y={},X={}) via {} -> map {} pos={:?} ***",
                    before.1, before.2, dname(d), core.rb(0xD35E), pos(core));
                warp_found = true;
                break;
            }
            if moved {
                let np = (pos(core).1, pos(core).2);
                if !visited.contains(&np) { stack.push(np); }
                // step back to keep exploring from `before`
                let back = match d { B_UP=>B_DOWN,B_DOWN=>B_UP,B_LEFT=>B_RIGHT,B_RIGHT=>B_LEFT,_=>d };
                move_tile(core, back);
            }
        }
    }
    println!("flood visited {} tiles: {:?}", visited.len(), {
        let mut v: Vec<_> = visited.iter().cloned().collect(); v.sort(); v
    });
    println!("flood end: map={} pos={:?} warp_found={}", core.rb(0xD35E), pos(core), warp_found);
    dump_ppm("/tmp/ccdd-probe/flood_end.ppm");
}

// Continuously hold a direction, sampling position every frame, never releasing,
// to let warp-on-walk fire. Logs the position trace and any map change.
fn run_stairs2(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);
    // get to top of left column
    hold_dir(core, B_UP, 60);
    hold_dir(core, B_LEFT, 60);
    println!("start cont-walk at {:?}", pos(core));

    // Continuously hold DOWN, sampling. The bed/desk at top, stairs at bottom-left.
    let mut trace: Vec<(u8,u8)> = Vec::new();
    clear_all();
    set_btn(B_DOWN, true);
    for f in 0..400u32 {
        core.frame();
        let (m,y,x) = pos(core);
        if trace.last() != Some(&(y,x)) { trace.push((y,x)); }
        if m != start_map { println!("  MAP CHANGE at frame {} -> {} pos=({},{})", f, m, y, x); break; }
    }
    clear_all();
    idle(core, 8);
    println!("DOWN trace (Y,X): {:?}", trace);
    println!("after DOWN cont: map={} {:?}", core.rb(0xD35E), pos(core));

    if core.rb(0xD35E) == start_map {
        // From wherever we are, continuously hold LEFT
        let mut t2: Vec<(u8,u8)> = Vec::new();
        clear_all(); set_btn(B_LEFT, true);
        for f in 0..300u32 {
            core.frame();
            let (m,y,x)=pos(core);
            if t2.last()!=Some(&(y,x)) { t2.push((y,x)); }
            if m!=start_map { println!("  LEFT MAP CHANGE frame {} -> {}", f, m); break; }
        }
        clear_all(); idle(core, 8);
        println!("LEFT trace: {:?}", t2);
    }
    // Try the bottom-right door region too: maybe exit is bottom not left
    if core.rb(0xD35E) == start_map {
        hold_dir(core, B_UP, 80);
        hold_dir(core, B_RIGHT, 80);
        let mut t3: Vec<(u8,u8)> = Vec::new();
        clear_all(); set_btn(B_DOWN, true);
        for f in 0..300u32 {
            core.frame();
            let (m,y,x)=pos(core);
            if t3.last()!=Some(&(y,x)) { t3.push((y,x)); }
            if m!=start_map { println!("  RIGHT-DOWN MAP CHANGE frame {} -> {}", f, m); break; }
        }
        clear_all(); idle(core, 8);
        println!("RIGHT-col DOWN trace: {:?}", t3);
    }
    dump_ppm("/tmp/ccdd-probe/stairs2.ppm");
    println!("stairs2 end: map={} {:?}", core.rb(0xD35E), pos(core));

    // Now fully map the BOTTOM row: from (7,5) sweep LEFT, trying DOWN at each X.
    println!("--- bottom-row sweep ---");
    hold_dir(core, B_RIGHT, 80);
    hold_dir(core, B_DOWN, 80);
    println!("bottom-right = {:?}", pos(core));
    for s in 0..8u32 {
        if core.rb(0xD35E) != start_map { break; }
        let before = pos(core);
        let pd = hold_dir(core, B_DOWN, 40);
        if pd.0 != start_map { println!("  *** WARP DOWN at {:?} -> map {} ***", before, pd.0); break; }
        println!("  bottomsweep at {:?} : DOWN->{:?}", before, pd);
        dump_ppm(&format!("/tmp/ccdd-probe/bot_{}.ppm", s));
        // move one left
        let pl = hold_dir(core, B_LEFT, 24);
        if pl == before { break; } // hit left wall
    }
    println!("bottom-sweep end: map={} {:?}", core.rb(0xD35E), pos(core));
}

// Test whether forcing the player onto a candidate stair tile triggers the warp.
fn run_warptest(core: &Core) {
    if !boot_to_bedroom(core) { println!("no control"); return; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let start_map = core.rb(0xD35E);
    // Go to (Y=7,X=1)
    hold_dir(core, B_DOWN, 60);
    hold_dir(core, B_LEFT, 60);
    hold_dir(core, B_DOWN, 60);
    println!("at {:?} before warp-write", pos(core));
    // Candidate stair tiles to try by writing coords then stepping/holding.
    let candidates: &[(u8,u8)] = &[(7,0),(6,0),(7,1),(6,1)];
    for &(y,x) in candidates {
        if core.rb(0xD35E) != start_map { break; }
        core.wb(0xD361, y);
        core.wb(0xD362, x);
        // run a few frames then press DOWN/LEFT to provoke the warp check
        idle(core, 4);
        let after_set = pos(core);
        hold_dir(core, B_DOWN, 20);
        let pd = pos(core);
        hold_dir(core, B_LEFT, 20);
        let pl = pos(core);
        println!("  set=({},{}) -> read {:?} ; afterDOWN {:?} afterLEFT {:?} map={}",
            y, x, after_set, pd, pl, core.rb(0xD35E));
        if core.rb(0xD35E) != start_map {
            println!("  *** WARP from forced tile ({},{}) -> map {} ***", y, x, core.rb(0xD35E));
            break;
        }
    }
    dump_ppm("/tmp/ccdd-probe/warptest.ppm");
    println!("warptest end map={} pos={:?}", core.rb(0xD35E), pos(core));
}

fn run_fromstate(core: &Core) {
    let path = std::env::var("STATE_PATH")
        .unwrap_or_else(|_| "~/pokemon-pvp-red/states/battle.state".to_string());
    load_state(core, &path);
    idle(core, 10);
    dump_battle_ram(core, "from-state");
}

// Sample several battle-relevant bytes in one row.
fn sample_row(core: &Core, label: &str) {
    println!("  [{}] CCD5(turns)={} CCDC(pMove)={} CCDD(eMove)={} FFF3? D057={} pHP={} eHP={}",
        label, core.rb(0xCCD5), core.rb(0xCCDC), core.rb(0xCCDD),
        core.rb(0xD057), core.rw_be(0xD015), core.rw_be(0xCFE6));
}

// The CCDD experiment runner (operates on a restored battle state).
// Goal: determine whether writing CCDD (enemy-selected move) sticks or is
// overwritten by the engine's AI move pick, and WHEN in the turn it is chosen.
fn run_ccdd_from_state(core: &Core) {
    let path = std::env::var("STATE_PATH")
        .unwrap_or_else(|_| "~/pokemon-pvp-red/states/battle.state".to_string());
    if !load_state(core, &path) {
        println!("cannot run CCDD experiment: state load failed");
        return;
    }
    idle(core, 4);
    dump_battle_ram(core, "ccdd-start");

    // Advance carefully from "Go! <mon>" to the FIGHT action menu WITHOUT executing
    // a turn. We tap A in small steps and stop as soon as a CCDD-write sticks across
    // an idle frame (i.e., we are sitting in the menu, engine not touching CCDD).
    // Detection of "menu is up and idle": write a sentinel to CCDD, idle 1 frame
    // with NO input; if it survives several idle frames, the engine isn't running
    // the turn logic -> we are at a menu waiting for input.
    println!("=== advancing to FIGHT menu (no turn) ===");
    let sentinel: u8 = std::env::var("SENTINEL").ok().and_then(|s| s.parse().ok()).unwrap_or(0x99);
    let at_menu;
    let mut taps_used = 0u32;
    loop {
        // probe: write sentinel, idle 4 frames with no input, see if it survives
        core.wb(0xCCDD, sentinel);
        let mut survived = true;
        for _ in 0..4 { clear_all(); core.frame(); if core.rb(0xCCDD) != sentinel { survived = false; break; } }
        if survived {
            at_menu = true;
            println!("  MENU/idle detected after {} A-taps (sentinel survived idle); CCD5={} CCDC={} CCDD={}",
                taps_used, core.rb(0xCCD5), core.rb(0xCCDC), core.rb(0xCCDD));
            break;
        }
        // not idle: engine running text -> tap A to advance
        tap(core, B_A, 5, 7);
        taps_used += 1;
        if taps_used > 60 { at_menu = false; break; }
    }
    dump_ppm("/tmp/ccdd-probe/ccdd_menu.ppm");
    sample_row(core, "at-menu");
    println!("at_menu={}", at_menu);

    // We are now idle at a menu. Restore CCDD to its real value before proceeding
    // (we polluted it with the sentinel during probing).
    core.wb(0xCCDD, 0);

    // ---- EXPERIMENT A: write CCDD before selecting our move, watch it across turn.
    println!("\n=== EXPERIMENT A: write CCDD={} at FIGHT menu, then select FIGHT+move0 ===", sentinel);
    core.wb(0xCCDD, sentinel);
    println!("  pre-FIGHT: CCDD read-back={}", core.rb(0xCCDD));
    // press A: FIGHT -> move list
    tap(core, B_A, 5, 12);
    println!("  after FIGHT press: CCDD={} (selecting move list now)", core.rb(0xCCDD));
    core.wb(0xCCDD, sentinel); // re-assert sentinel right before confirming move
    // press A: confirm move 0 -> turn begins
    println!("  re-asserted CCDD={}; confirming move0, sampling every frame:", core.rb(0xCCDD));
    run_turn_sampling(core, sentinel);

    dump_ppm("/tmp/ccdd-probe/ccdd_afterturnA.ppm");
    sample_row(core, "after turn A");

    // ---- EXPERIMENT B: fresh state, find the AI-pick frame, then OVERWRITE CCDD
    //      *after* the engine sets it, to see if the later write is used.
    println!("\n=== EXPERIMENT B: overwrite CCDD AFTER the engine's AI pick ===");
    load_state(core, &path);
    idle(core, 4);
    // advance to menu (same probe)
    let mut tu = 0;
    loop {
        core.wb(0xCCDD, sentinel);
        let mut surv = true;
        for _ in 0..4 { clear_all(); core.frame(); if core.rb(0xCCDD)!=sentinel {surv=false;break;} }
        if surv { break; }
        tap(core, B_A, 5, 7); tu += 1; if tu > 60 { break; }
    }
    core.wb(0xCCDD, 0);
    // select FIGHT + move0
    tap(core, B_A, 5, 12); // FIGHT
    // confirm move0 and watch for the engine to SET ccdd (it was 0); when it becomes
    // nonzero (the AI pick), immediately overwrite with sentinel and keep sampling.
    clear_all(); set_btn(B_A, true); for _ in 0..3 { core.frame(); } clear_all();
    let mut wrote_after = false;
    let mut prev = core.rb(0xCCDD);
    println!("  watching for AI to set CCDD (currently {})...", prev);
    for f in 0..400u32 {
        if f % 10 == 0 { set_btn(B_A,true); } else { set_btn(B_A,false); }
        core.frame();
        let c = core.rb(0xCCDD);
        if c != prev {
            println!("  f{}: engine set CCDD {} -> {} (AI pick)", f, prev, c);
            if !wrote_after && c != 0 && c != sentinel {
                core.wb(0xCCDD, sentinel);
                println!("  f{}: OVERWROTE CCDD with sentinel={} right after AI pick; read-back={}",
                    f, sentinel, core.rb(0xCCDD));
                wrote_after = true;
                prev = sentinel;
                continue;
            }
            prev = c;
        }
        if core.rb(0xD057) == 0 { println!("  f{}: battle ended", f); break; }
        if wrote_after && core.rb(0xCCD5) >= 2 { println!("  f{}: next turn reached (CCD5>=2), stop", f); break; }
    }
    clear_all();
    sample_row(core, "after turn B");
    println!("  EXP-B: did sentinel survive into move execution? final CCDD={} (sentinel={})",
        core.rb(0xCCDD), sentinel);
    dump_ppm("/tmp/ccdd-probe/ccdd_afterturnB.ppm");
}

// Pure observation: no writes. Document the canonical CCDD/CCDC/CCD5 timeline of a
// single turn (when does the engine select the player move into CCDC and the enemy
// move into CCDD, and when does the move execute / turn count advance).
fn run_ccdd_observe(core: &Core) {
    let path = "~/pokemon-pvp-red/states/battle.state";
    if !load_state(core, path) { println!("no battle.state"); return; }
    idle(core, 4);
    // advance to idle FIGHT menu (probe with sentinel, then restore to 0)
    let s = 0x99u8; let mut tu=0;
    loop {
        core.wb(0xCCDD, s);
        let mut surv=true; for _ in 0..4 { clear_all(); core.frame(); if core.rb(0xCCDD)!=s {surv=false;break;} }
        if surv { break; }
        tap(core, B_A, 5, 7); tu+=1; if tu>60 {break;}
    }
    core.wb(0xCCDD, 0); core.wb(0xCCDC, 0);
    println!("OBSERVE: at FIGHT menu, CCDC={} CCDD={} CCD5={}", core.rb(0xCCDC), core.rb(0xCCDD), core.rb(0xCCD5));
    // FIGHT then confirm move0; sample every frame, report all changes.
    tap(core, B_A, 5, 12);
    clear_all(); set_btn(B_A,true); for _ in 0..3 { core.frame(); } clear_all();
    let (mut pcd, mut pcc, mut pt, mut php, mut peh) =
        (core.rb(0xCCDD), core.rb(0xCCDC), core.rb(0xCCD5), core.rw_be(0xD015), core.rw_be(0xCFE6));
    println!("  f0: CCDC={} CCDD={} CCD5={} pHP={} eHP={}", pcc, pcd, pt, php, peh);
    for f in 0..900u32 {
        if f % 10 == 0 { set_btn(B_A,true); } else { set_btn(B_A,false); }
        core.frame();
        let (cd,cc,t,ph,eh) = (core.rb(0xCCDD), core.rb(0xCCDC), core.rb(0xCCD5), core.rw_be(0xD015), core.rw_be(0xCFE6));
        if cc!=pcc { println!("  f{}: CCDC(playerMove) {} -> {}", f, pcc, cc); pcc=cc; }
        if cd!=pcd { println!("  f{}: CCDD(enemyMove)  {} -> {}", f, pcd, cd); pcd=cd; }
        if t!=pt   { println!("  f{}: CCD5(turns)      {} -> {}", f, pt, t); pt=t; }
        if ph!=php { println!("  f{}: playerHP {} -> {}", f, php, ph); php=ph; }
        if eh!=peh { println!("  f{}: enemyHP  {} -> {}", f, peh, eh); peh=eh; }
        if core.rb(0xD057)==0 { println!("  f{}: battle ended", f); break; }
        if t >= 2 { println!("  f{}: two turns elapsed, stop", f); break; }
    }
    clear_all();
}

// Definitive CCDD experiment: does the byte at CCDD drive the move the enemy
// actually executes? We overwrite CCDD right after the AI pick with a chosen move
// and observe the battle text + player HP to see which move executes. Also probes
// whether HRAM FFF3 is reachable via SYSTEM_RAM.
fn run_ccdd_definitive(core: &Core) {
    let path = "~/pokemon-pvp-red/states/battle.state";
    // override move to inject after AI pick: default 33 = TACKLE (damaging)
    let inject: u8 = std::env::var("INJECT").ok().and_then(|s| s.parse().ok()).unwrap_or(33);
    let mode = std::env::var("WHEN").unwrap_or_else(|_| "after".to_string()); // before|after

    // HRAM reachability probe (does SYSTEM_RAM cover 0xFF80-0xFFFE?)
    unsafe {
        let n = (core.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
        println!("SYSTEM_RAM size = {} bytes ({} KiB). 0xC000-0xDFFF needs 8192.",
            n, n/1024);
        println!("  => HRAM 0xFF80-0xFFFE is OUTSIDE this window (offset {} >= {}). FFF3 NOT in SYSTEM_RAM.",
            0xFFF3u32.wrapping_sub(0xC000), n);
    }

    if !load_state(core, path) { println!("no battle.state"); return; }
    idle(core, 4);
    let sentinel: u8 = 0x99;
    // advance to idle menu
    let mut tu = 0;
    loop {
        core.wb(0xCCDD, sentinel);
        let mut surv = true;
        for _ in 0..4 { clear_all(); core.frame(); if core.rb(0xCCDD)!=sentinel {surv=false;break;} }
        if surv { break; }
        tap(core, B_A, 5, 7); tu += 1; if tu > 60 { break; }
    }
    core.wb(0xCCDD, 0);
    println!("at FIGHT menu. inject move={} when={}", inject, mode);
    let php0 = core.rw_be(0xD015);
    let ehp0 = core.rw_be(0xCFE6);
    println!("  pre-turn: playerHP={} enemyHP={}", php0, ehp0);

    if mode == "before" {
        // write inject BEFORE selecting move (expect engine to overwrite it)
        core.wb(0xCCDD, inject);
    }
    // FIGHT
    tap(core, B_A, 5, 12);
    // confirm move0
    clear_all(); set_btn(B_A, true); for _ in 0..3 { core.frame(); } clear_all();

    let mut prev = core.rb(0xCCDD);
    let mut injected = mode == "before";
    let mut ai_pick: i32 = -1;
    for f in 0..900u32 {
        if f % 10 == 0 { set_btn(B_A,true); } else { set_btn(B_A,false); }
        core.frame();
        let c = core.rb(0xCCDD);
        if c != prev {
            if ai_pick < 0 && c != 0 && c != sentinel && c != inject { ai_pick = c as i32; println!("  f{}: AI picked CCDD={}", f, c); }
            else { println!("  f{}: CCDD {} -> {}", f, prev, c); }
            if mode == "after" && !injected && c != 0 && c != inject {
                core.wb(0xCCDD, inject);
                println!("  f{}: injected CCDD={} (after AI pick); read-back={}", f, inject, core.rb(0xCCDD));
                injected = true;
                prev = inject;
                continue;
            }
            prev = c;
        }
        if core.rb(0xD057) == 0 { println!("  f{}: battle ended", f); break; }
        if core.rb(0xCCD5) >= 2 { println!("  f{}: turn done (CCD5>=2)", f); break; }
    }
    clear_all();
    let php1 = core.rw_be(0xD015);
    let ehp1 = core.rw_be(0xCFE6);
    println!("  post-turn: playerHP={} (delta {}) enemyHP={} (delta {}) finalCCDD={} AIpick={}",
        php1, php0 as i32 - php1 as i32, ehp1, ehp0 as i32 - ehp1 as i32, core.rb(0xCCDD), ai_pick);
    dump_ppm("/tmp/ccdd-probe/ccdd_def.ppm");
}

// Confirm the currently-selected move and sample CCDD/CCDC/CCD5 every frame,
// reporting every change, until the turn completes (CCD5 increments) or battle ends.
fn run_turn_sampling(core: &Core, sentinel: u8) {
    clear_all();
    set_btn(B_A, true);
    for _ in 0..3 { core.frame(); }
    clear_all();
    let mut prev_ccdd = core.rb(0xCCDD);
    let mut prev_ccdc = core.rb(0xCCDC);
    let start_turns = core.rb(0xCCD5);
    let mut prev_turns = start_turns;
    println!("    f0: CCDD={} CCDC={} CCD5={}", prev_ccdd, prev_ccdc, prev_turns);
    let mut overwrite_frame: i64 = -1;
    for f in 0..900u32 {
        if f % 10 == 0 { set_btn(B_A, true); } else { set_btn(B_A, false); }
        core.frame();
        let c = core.rb(0xCCDD);
        let pc = core.rb(0xCCDC);
        let t = core.rb(0xCCD5);
        if c != prev_ccdd {
            println!("    f{}: CCDD {} -> {} (CCDC={} CCD5={})", f, prev_ccdd, c, pc, t);
            if prev_ccdd == sentinel && overwrite_frame < 0 { overwrite_frame = f as i64; }
            prev_ccdd = c;
        }
        if pc != prev_ccdc { println!("    f{}: CCDC {} -> {}", f, prev_ccdc, pc); prev_ccdc = pc; }
        if t != prev_turns { println!("    f{}: CCD5(turns) {} -> {}", f, prev_turns, t); prev_turns = t; }
        if core.rb(0xD057) == 0 { println!("    f{}: battle ended", f); break; }
        if t > start_turns + 1 { println!("    f{}: turn complete (CCD5 advanced 2)", f); break; }
    }
    clear_all();
    if overwrite_frame >= 0 {
        println!("    >>> sentinel was OVERWRITTEN at frame {} of the turn", overwrite_frame);
    } else {
        println!("    >>> sentinel was NOT overwritten during the sampled window");
    }
}

// ===================================================================================
// LEGENDARY INJECTION RESEARCH
// ===================================================================================

// Reach the rival battle (boot -> bedroom -> 1F -> Pallet -> Oak -> lab -> starter ->
// rival battle) and return the instant D057 != 0, WITHOUT driving the intro/send-out.
// Returns true if a battle was reached.
fn reach_battle(core: &Core) -> bool {
    if !boot_to_bedroom(core) { println!("FAIL: no control"); return false; }
    for _ in 0..3 { tap(core, B_B, 4, 8); }
    let mut rng: u32 = std::env::var("SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x1234_5678);
    println!("REACH start: {:?}", pos(core));
    let mut last_map = core.rb(0xD35E);
    for phase in 0..24u32 {
        if battle_active(core) != 0 { break; }
        let cur = core.rb(0xD35E);
        let bias = match cur { 37 => B_DOWN, 0 => B_UP, 40 => B_UP, _ => usize::MAX };
        let (reason, name) = explore_until_change(core, &mut rng, 1200, true, bias);
        let m = core.rb(0xD35E);
        println!("  phase {}: stop={} ({}) map {} -> {} pos={:?} party={}",
            phase, reason, name, last_map, m, pos(core), core.rb(0xD163));
        if reason == 2 { break; }
        if reason == 0 {
            drive_cutscene(core, 60);
            if battle_active(core) != 0 { break; }
        }
        last_map = m;
        drive_cutscene(core, 12);
    }
    let inb = battle_active(core);
    println!("REACH end: map={} pos={:?} party={} D057={}",
        core.rb(0xD35E), pos(core), core.rb(0xD163), inb as i8);
    inb != 0
}

// Dump the bytes that distinguish "intro (pre-send-out)" from "sent out".
fn intro_probe(core: &Core, label: &str) {
    println!("  [{}] D057={} | wBattleMon.species D014={} D009={} pLvl(D022)={} pHP={} | wEnemyMon.species CFE5={} eLvl(CFE8)={} eHP={} | enemyPartyCount D89C={} enemyPartySpec D89D={} | partyCount D163={} partySpec D164={}",
        label,
        core.rb(0xD057),
        core.rb(0xD014), core.rb(0xD009), core.rb(0xD022), core.rw_be(0xD015),
        core.rb(0xCFE5), core.rb(0xCFE8), core.rw_be(0xCFE6),
        core.rb(0xD89C), core.rb(0xD89D),
        core.rb(0xD163), core.rb(0xD164));
}

// Capture INTRO savestate candidates: from D057!=0, save states at increasing frame
// offsets (advancing with A taps), recording the send-out markers (D014/CFE5) so we can
// pick the candidate that is AFTER the trainer party is loaded but BEFORE send-out.
fn run_introcap(core: &Core) {
    let outdir = "/tmp/legendary";
    if !reach_battle(core) { println!("NO BATTLE REACHED"); return; }
    println!("=== BATTLE REACHED, D057={} — capturing intro candidates ===", core.rb(0xD057));
    intro_probe(core, "at-d057");
    dump_ppm(&format!("{}/intro_at_d057.ppm", outdir));
    // Candidate 0: the very first frame D057 went nonzero.
    save_state(core, &format!("{}/cand_000.state", outdir));

    // Now step forward in small increments, tapping A to push the intro text along,
    // and snapshot a candidate every `step_frames`. Stop once BOTH sides are sent out
    // (D014 != 0 AND CFE5 != 0) plus a margin so we bracket the send-out boundary.
    let step_frames = 12u32;
    let mut idx = 1u32;
    let mut sent_out_seen = 0i32;
    for k in 0..60u32 {
        // Advance: tap A a little (to clear "wants to fight" text) but mostly idle so
        // we don't accidentally blow past the menu into a turn.
        if k % 2 == 0 { tap(core, B_A, 4, 8); } else { idle(core, step_frames); }
        let d014 = core.rb(0xD014);
        let cfe5 = core.rb(0xCFE5);
        let label = format!("k{}", k);
        intro_probe(core, &label);
        save_state(core, &format!("{}/cand_{:03}.state", outdir, idx));
        dump_ppm(&format!("{}/cand_{:03}.ppm", outdir, idx));
        idx += 1;
        if d014 != 0 && cfe5 != 0 {
            sent_out_seen += 1;
            if sent_out_seen >= 3 { println!("  both sent out + margin reached at k={}", k); break; }
        }
        if core.rb(0xD057) == 0 { println!("  battle ended unexpectedly at k={}", k); break; }
        if core.rb(0xCCD5) >= 1 { println!("  a turn already elapsed (CCD5>=1) at k={}, stop", k); break; }
    }
    println!("=== introcap done: wrote cand_000..cand_{:03} ===", idx - 1);
}

// Build a minimal 44-byte party-mon struct at `base` (CPU addr). BE multibyte.
// species, level, hp/maxhp, two moves, types. Stats set = maxhp-ish sane values.
#[allow(clippy::too_many_arguments)]
fn build_party_mon(
    core: &Core, base: u16, species: u8, level: u8, hp: u16, maxhp: u16,
    moves: [u8; 4], pp: [u8; 4], type1: u8, type2: u8,
    atk: u16, def: u16, spd: u16, spc: u16,
) {
    let wb = |off: u16, v: u8| core.wb(base + off, v);
    let wbe = |off: u16, v: u16| { let b = v.to_be_bytes(); core.wb(base + off, b[0]); core.wb(base + off + 1, b[1]); };
    wb(0, species);
    wbe(1, hp);        // +1 current HP (BE)
    wb(3, 0);          // box level scratch
    wb(4, 0);          // status
    wb(5, type1);      // +5 type1
    wb(6, type2);      // +6 type2
    wb(7, 0);          // catch rate / held item
    wb(8, moves[0]); wb(9, moves[1]); wb(10, moves[2]); wb(11, moves[3]);
    wbe(12, 0);        // OT id
    // exp +14 (3 bytes) — set to a value consistent-ish with level; not critical for sprite.
    wb(14, 0); wb(15, 0); wb(16, 0);
    for o in 17..27u16 { wb(o, 0); } // EVs
    wb(27, 0xFF); wb(28, 0xFF);      // DVs (max)
    wb(29, pp[0]); wb(30, pp[1]); wb(31, pp[2]); wb(32, pp[3]);
    wb(33, level);     // +33 live level
    wbe(34, maxhp);    // +34 max HP
    wbe(36, atk);
    wbe(38, def);
    wbe(40, spd);
    wbe(42, spc);
}

// Write a 0x50-terminated, padded nickname (11 bytes) using Gen-1 char encoding.
// Gen-1 text: 'A'..'Z' = 0x80.., 0x50 = terminator. We just need uppercase letters.
fn write_nick(core: &Core, base: u16, name: &str) {
    let mut i = 0u16;
    for ch in name.chars().take(10) {
        let code = match ch {
            'A'..='Z' => 0x80 + (ch as u8 - b'A'),
            'a'..='z' => 0x80 + (ch as u8 - b'a'),
            ' ' => 0x7F,
            _ => 0x80,
        };
        core.wb(base + i, code);
        i += 1;
    }
    core.wb(base + i, 0x50); // terminator
    // pad remainder with 0x50
    for j in (i + 1)..11 {
        core.wb(base + j, 0x50);
    }
}

// Injection test. Load an intro candidate state (STATE_PATH or default cand), inject a
// fake party for BOTH sides (player + enemy), run through send-out, then read
// wBattleMon.species (D014) and wEnemyMon.species (CFE5) and dump a framebuffer.
//
// Species are internal Gen-1 indices (env PLAYER_SPECIES / ENEMY_SPECIES). Articuno=0x4A,
// Zapdos=0x4B, Moltres=0x49 (defaults). Level via env LVL (default 50).
fn run_injtest(core: &Core) {
    let outdir = "/tmp/legendary";
    let path = std::env::var("STATE_PATH")
        .unwrap_or_else(|_| format!("{}/cand_000.state", outdir));
    let player_species: u8 = std::env::var("PLAYER_SPECIES").ok()
        .and_then(|s| parse_u8(&s)).unwrap_or(0x4A); // Articuno
    let enemy_species: u8 = std::env::var("ENEMY_SPECIES").ok()
        .and_then(|s| parse_u8(&s)).unwrap_or(0x4B); // Zapdos
    let lvl: u8 = std::env::var("LVL").ok().and_then(|s| s.parse().ok()).unwrap_or(50);

    if !load_state(core, &path) { println!("cannot load {}", path); return; }
    idle(core, 2);
    println!("=== INJTEST: state={} player_species=0x{:02X} enemy_species=0x{:02X} lvl={} ===",
        path, player_species, enemy_species, lvl);
    intro_probe(core, "loaded");

    // --- PLAYER party injection ---
    // wPartyCount D163, wPartySpecies D164.., terminator, wPartyMon[0] D16B.
    core.wb(0xD163, 1);
    core.wb(0xD164, player_species);
    core.wb(0xD165, 0xFF);
    // Articuno: Ice(0x19)/Flying(0x02). Moves: PECK(0x40=64), 0,0,0 — keep simple.
    build_party_mon(core, 0xD16B, player_species, lvl, 150, 150,
        [64, 0, 0, 0], [35, 0, 0, 0], 0x19, 0x02, 100, 100, 110, 120);
    write_nick(core, 0xD2B5, "ARTICUNO"); // wPartyMonNicks (D2B5 in pokered)

    // --- ENEMY party injection (trainer party in WRAM) ---
    // wEnemyPartyCount D89C, wEnemyPartySpecies D89D.., terminator, wEnemyMons[0] D8A4.
    core.wb(0xD89C, 1);
    core.wb(0xD89D, enemy_species);
    core.wb(0xD89E, 0xFF);
    // Zapdos: Electric(0x17)/Flying(0x02). Moves: THUNDERSHOCK(0x54=84)? keep PECK simple.
    build_party_mon(core, 0xD8A4, enemy_species, lvl, 150, 150,
        [64, 0, 0, 0], [35, 0, 0, 0], 0x17, 0x02, 90, 85, 100, 125);
    write_nick(core, 0xD9EE, "ZAPDOS"); // wEnemyMonNicks (D9EE in pokered)

    println!("  injected: D163={} D164=0x{:02X}(={}) | D89C={} D89D=0x{:02X}(={})",
        core.rb(0xD163), core.rb(0xD164), core.rb(0xD164),
        core.rb(0xD89C), core.rb(0xD89D), core.rb(0xD89D));

    // Drive the send-out: tap A through the intro until BOTH active species populate,
    // sampling the send-out markers each step.
    let mut prev_d014 = core.rb(0xD014);
    let mut prev_cfe5 = core.rb(0xCFE5);
    println!("  driving send-out...");
    for i in 0..160u32 {
        tap(core, B_A, 5, 7);
        let d014 = core.rb(0xD014);
        let cfe5 = core.rb(0xCFE5);
        if d014 != prev_d014 { println!("   i{}: wBattleMon.species D014 {} -> {} (=0x{:02X})", i, prev_d014, d014, d014); prev_d014 = d014; }
        if cfe5 != prev_cfe5 { println!("   i{}: wEnemyMon.species CFE5 {} -> {} (=0x{:02X})", i, prev_cfe5, cfe5, cfe5); prev_cfe5 = cfe5; }
        if i % 10 == 0 { intro_probe(core, &format!("send i{}", i)); }
        // stop once both active species are set and a menu is reachable
        if d014 != 0 && cfe5 != 0 && core.rb(0xD022) > 0 {
            // a few more taps to settle, then stop before a turn executes
            idle(core, 4);
            println!("   both active species populated at i={}", i);
            break;
        }
        if core.rb(0xD057) == 0 { println!("   battle ended at i={}", i); break; }
        if core.rb(0xCCD5) >= 1 { println!("   turn elapsed (CCD5>=1) at i={}", i); break; }
    }

    idle(core, 4);
    // Optionally drive a few more taps so the PLAYER's mon back-sprite + name are drawn
    // (the "Go! <MON>" -> field state), capturing a second screenshot.
    let extra: u32 = std::env::var("EXTRA_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    for _ in 0..extra { tap(core, B_A, 5, 7); if core.rb(0xCCD5) >= 1 { break; } }
    if extra > 0 {
        let tag2 = std::env::var("TAG2").unwrap_or_else(|_| "injtest_field".to_string());
        dump_ppm(&format!("{}/{}.ppm", outdir, tag2));
        println!("  field framebuffer -> {}/{}.ppm", outdir, tag2);
    }
    println!("=== POST-SEND-OUT RESULTS ===");
    let d014 = core.rb(0xD014);
    let cfe5 = core.rb(0xCFE5);
    println!("  wBattleMon.species (D014) = {} (0x{:02X})  [injected player=0x{:02X}]  MATCH={}",
        d014, d014, player_species, d014 == player_species);
    println!("  wBattleMon (D009) species mirror = {} (0x{:02X})", core.rb(0xD009), core.rb(0xD009));
    println!("  wEnemyMon.species  (CFE5) = {} (0x{:02X})  [injected enemy=0x{:02X}]  MATCH={}",
        cfe5, cfe5, enemy_species, cfe5 == enemy_species);
    println!("  player level D022={} HP(D015)={} | enemy level CFE8={} HP(CFE6)={}",
        core.rb(0xD022), core.rw_be(0xD015), core.rb(0xCFE8), core.rw_be(0xCFE6));
    intro_probe(core, "final");
    let tag = std::env::var("TAG").unwrap_or_else(|_| "injtest".to_string());
    dump_ppm(&format!("{}/{}.ppm", outdir, tag));
    println!("  framebuffer -> {}/{}.ppm", outdir, tag);
    if std::env::var("SAVE_WIN").is_ok() {
        save_state(core, "~/pokemon-pvp-red/states/legendary_intro.state");
    }
}

fn parse_u8(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}
