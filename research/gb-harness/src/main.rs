// Headless libretro frontend probe for GAME BOY / GAME BOY COLOR cores
// (SameBoy / gambatte / mgba). Hand-rolled extern "C" callbacks + libloading.
// Adapted verbatim from the verified N64 mupen harness; only the forced-option
// table and the reporting (distinct-color counting + PPM dumps) changed.
use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

// ---------- libretro constants ----------
const RETRO_ENVIRONMENT_SET_ROTATION: c_uint = 1;
const RETRO_ENVIRONMENT_GET_OVERSCAN: c_uint = 2;
const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
const RETRO_ENVIRONMENT_SET_MESSAGE: c_uint = 6;
const RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL: c_uint = 8;
const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
const RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME: c_uint = 18;
const RETRO_ENVIRONMENT_GET_LIBRETRO_PATH: c_uint = 19;
const RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE: c_uint = 23;
const RETRO_ENVIRONMENT_GET_INPUT_DEVICE_CAPABILITIES: c_uint = 24;
const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
const RETRO_ENVIRONMENT_GET_PERF_INTERFACE: c_uint = 28;
const RETRO_ENVIRONMENT_GET_CORE_ASSETS_DIRECTORY: c_uint = 30;
const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: c_uint = 32;
const RETRO_ENVIRONMENT_SET_HW_RENDER: c_uint = 14;
const RETRO_ENVIRONMENT_SET_SUBSYSTEM_INFO: c_uint = 34;
const RETRO_ENVIRONMENT_SET_CONTROLLER_INFO: c_uint = 35;
const RETRO_ENVIRONMENT_SET_GEOMETRY: c_uint = 37;
const RETRO_ENVIRONMENT_GET_USERNAME: c_uint = 38;
const RETRO_ENVIRONMENT_GET_LANGUAGE: c_uint = 39;
const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS: c_uint = 53;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL: c_uint = 54;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY: c_uint = 55;
const RETRO_ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;
const RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: c_uint = 51 | 0x10000;
const RETRO_ENVIRONMENT_GET_FASTFORWARDING: c_uint = 50;
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;

const RETRO_PIXEL_FORMAT_0RGB1555: c_uint = 0;
const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;
const RETRO_PIXEL_FORMAT_RGB565: c_uint = 2;

const RETRO_DEVICE_JOYPAD: c_uint = 1;
const RETRO_DEVICE_ANALOG: c_uint = 5;

const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;

const RETRO_DEVICE_INDEX_ANALOG_LEFT: c_uint = 0;
const RETRO_DEVICE_ID_ANALOG_X: c_uint = 0;
const RETRO_DEVICE_ID_ANALOG_Y: c_uint = 1;

// ---------- libretro structs ----------
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

// ---------- global frame/audio capture ----------
struct FrameState {
    width: u32,
    height: u32,
    pitch: usize,
    pixfmt: u32,
    last_frame: Vec<u8>,
    frames_seen: u64,
    audio_samples: u64,
    sample_rate: f64,
    av_fps: f64,
    channels_seen_batch: bool,
    hw_render_requested: bool,
}
static STATE: Mutex<FrameState> = Mutex::new(FrameState {
    width: 0,
    height: 0,
    pitch: 0,
    pixfmt: RETRO_PIXEL_FORMAT_0RGB1555,
    last_frame: Vec::new(),
    frames_seen: 0,
    audio_samples: 0,
    sample_rate: 0.0,
    av_fps: 0.0,
    channels_seen_batch: false,
    hw_render_requested: false,
});

struct InputState {
    joypad: [bool; 16],
    analog_x: i16,
    analog_y: i16,
}
static INPUT: Mutex<InputState> = Mutex::new(InputState {
    joypad: [false; 16],
    analog_x: 0,
    analog_y: 0,
});

static mut SYS_DIR: *const c_char = ptr::null();
static mut SAVE_DIR: *const c_char = ptr::null();

// ---------- callbacks ----------
unsafe extern "C" fn cb_video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    let mut st = STATE.lock().unwrap();
    st.frames_seen += 1;
    if data.is_null() {
        st.width = width;
        st.height = height;
        st.pitch = pitch;
        return;
    }
    st.width = width;
    st.height = height;
    st.pitch = pitch;
    let bytes = pitch * height as usize;
    let slice = std::slice::from_raw_parts(data as *const u8, bytes);
    st.last_frame.clear();
    st.last_frame.extend_from_slice(slice);
}

unsafe extern "C" fn cb_audio_sample(_left: i16, _right: i16) {
    let mut st = STATE.lock().unwrap();
    st.audio_samples += 1;
}

unsafe extern "C" fn cb_audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    let mut st = STATE.lock().unwrap();
    st.audio_samples += frames as u64;
    st.channels_seen_batch = true;
    frames
}

unsafe extern "C" fn cb_input_poll() {}

unsafe extern "C" fn cb_input_state(
    _port: c_uint,
    device: c_uint,
    index: c_uint,
    id: c_uint,
) -> i16 {
    let inp = INPUT.lock().unwrap();
    match device {
        RETRO_DEVICE_JOYPAD => {
            if (id as usize) < 16 && inp.joypad[id as usize] {
                1
            } else {
                0
            }
        }
        RETRO_DEVICE_ANALOG => {
            if index == RETRO_DEVICE_INDEX_ANALOG_LEFT {
                if id == RETRO_DEVICE_ID_ANALOG_X {
                    inp.analog_x
                } else if id == RETRO_DEVICE_ID_ANALOG_Y {
                    inp.analog_y
                } else {
                    0
                }
            } else {
                0
            }
        }
        _ => 0,
    }
}

// GB cores auto-detect DMG vs GBC from the 0x143 header byte. We force NO options
// so the core uses its own (correct) defaults. Optionally allow overriding via env
// PROBE_FORCE="key=val;key2=val2" for experimentation.
fn forced_option(key: &str) -> Option<String> {
    if let Ok(v) = std::env::var("PROBE_FORCE") {
        for pair in v.split(';') {
            let mut it = pair.splitn(2, '=');
            if let (Some(k), Some(val)) = (it.next(), it.next()) {
                if k == key {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    if std::env::var("PROBE_VERBOSE").is_ok() && cmd != 15 && cmd != 17 {
        eprintln!("[env] cmd={} (exp_base={})", cmd, cmd & !RETRO_ENVIRONMENT_EXPERIMENTAL);
    }
    match cmd {
        RETRO_ENVIRONMENT_GET_CAN_DUPE => {
            if !data.is_null() {
                *(data as *mut bool) = true;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            let fmt = *(data as *const c_uint);
            let mut st = STATE.lock().unwrap();
            st.pixfmt = fmt;
            eprintln!("[env] SET_PIXEL_FORMAT -> {}", fmt);
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
                let cs = CString::new(val.clone()).unwrap();
                var.value = cs.into_raw();
                if std::env::var("PROBE_VERBOSE").is_ok() {
                    eprintln!("[GET_VAR] {} -> FORCED {}", key, val);
                }
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
        RETRO_ENVIRONMENT_SET_HW_RENDER => {
            let mut st = STATE.lock().unwrap();
            st.hw_render_requested = true;
            eprintln!("[env] SET_HW_RENDER requested by core -> REFUSING (forcing software)");
            false
        }
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
                *(data as *mut u64) =
                    (1u64 << RETRO_DEVICE_JOYPAD) | (1u64 << RETRO_DEVICE_ANALOG);
            }
            true
        }
        RETRO_ENVIRONMENT_GET_LANGUAGE => {
            if !data.is_null() {
                *(data as *mut c_uint) = 0;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => {
            if !data.is_null() {
                let av = &*(data as *const retro_system_av_info);
                let mut st = STATE.lock().unwrap();
                st.sample_rate = av.timing.sample_rate;
                st.av_fps = av.timing.fps;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_GEOMETRY => true,
        RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL => true,
        RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME => true,
        RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS => true,
        RETRO_ENVIRONMENT_SET_CONTROLLER_INFO => true,
        RETRO_ENVIRONMENT_SET_SUBSYSTEM_INFO => true,
        RETRO_ENVIRONMENT_SET_ROTATION => true,
        RETRO_ENVIRONMENT_SET_MESSAGE => true,
        RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE => false,
        RETRO_ENVIRONMENT_GET_PERF_INTERFACE => false,
        RETRO_ENVIRONMENT_GET_USERNAME => false,
        RETRO_ENVIRONMENT_GET_FASTFORWARDING => false,
        _ => false,
    }
}

// distinct (r,g,b) colors after decode -> proves real color vs 4-shade DMG.
fn decode_pixel(frame: &[u8], off_base: usize, pixfmt: u32, x: usize) -> Option<(u8, u8, u8)> {
    match pixfmt {
        1 => {
            let off = off_base + x * 4;
            if off + 3 < frame.len() {
                // XRGB8888 little-endian in memory: B,G,R,X
                Some((frame[off + 2], frame[off + 1], frame[off]))
            } else {
                None
            }
        }
        2 => {
            let off = off_base + x * 2;
            if off + 1 < frame.len() {
                let v = u16::from_le_bytes([frame[off], frame[off + 1]]);
                let r = ((v >> 11) & 0x1f) as u8;
                let g = ((v >> 5) & 0x3f) as u8;
                let b = (v & 0x1f) as u8;
                Some(((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)))
            } else {
                None
            }
        }
        _ => {
            let off = off_base + x * 2;
            if off + 1 < frame.len() {
                let v = u16::from_le_bytes([frame[off], frame[off + 1]]);
                let r = ((v >> 10) & 0x1f) as u8;
                let g = ((v >> 5) & 0x1f) as u8;
                let b = (v & 0x1f) as u8;
                Some(((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2)))
            } else {
                None
            }
        }
    }
}

fn distinct_colors(frame: &[u8], w: u32, h: u32, pitch: usize, pixfmt: u32) -> usize {
    use std::collections::HashSet;
    let mut set: HashSet<(u8, u8, u8)> = HashSet::new();
    for y in 0..h as usize {
        let row = y * pitch;
        for x in 0..w as usize {
            if let Some(c) = decode_pixel(frame, row, pixfmt, x) {
                set.insert(c);
            }
        }
    }
    set.len()
}

fn main() {
    let core_path = std::env::args().nth(1).expect("usage: probe <core.dylib> <rom> [tag]");
    let rom_path = std::env::args().nth(2).expect("usage: probe <core.dylib> <rom> [tag]");
    let tag = std::env::args().nth(3).unwrap_or_else(|| "out".to_string());

    unsafe {
        SYS_DIR = CString::new("/tmp/gbprobe/systemdir").unwrap().into_raw();
        SAVE_DIR = CString::new("/tmp/gbprobe/savedir").unwrap().into_raw();
    }

    println!("=== GB/GBC headless libretro probe ===");
    println!("core: {}", core_path);
    println!("rom:  {}", rom_path);
    println!("tag:  {}", tag);

    let lib = unsafe { libloading::Library::new(&core_path).expect("load core dylib") };

    unsafe {
        let retro_api_version: libloading::Symbol<unsafe extern "C" fn() -> c_uint> =
            lib.get(b"retro_api_version").unwrap();
        let retro_get_system_info: libloading::Symbol<unsafe extern "C" fn(*mut retro_system_info)> =
            lib.get(b"retro_get_system_info").unwrap();
        let retro_set_environment: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool),
        > = lib.get(b"retro_set_environment").unwrap();
        let retro_set_video_refresh: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize)),
        > = lib.get(b"retro_set_video_refresh").unwrap();
        let retro_set_audio_sample: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(i16, i16)),
        > = lib.get(b"retro_set_audio_sample").unwrap();
        let retro_set_audio_sample_batch: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(*const i16, usize) -> usize),
        > = lib.get(b"retro_set_audio_sample_batch").unwrap();
        let retro_set_input_poll: libloading::Symbol<unsafe extern "C" fn(unsafe extern "C" fn())> =
            lib.get(b"retro_set_input_poll").unwrap();
        let retro_set_input_state: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16),
        > = lib.get(b"retro_set_input_state").unwrap();
        let retro_init: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_init").unwrap();
        let retro_load_game: libloading::Symbol<
            unsafe extern "C" fn(*const retro_game_info) -> bool,
        > = lib.get(b"retro_load_game").unwrap();
        let retro_get_system_av_info: libloading::Symbol<
            unsafe extern "C" fn(*mut retro_system_av_info),
        > = lib.get(b"retro_get_system_av_info").unwrap();
        let retro_run: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_run").unwrap();
        let retro_set_controller_port_device: libloading::Symbol<
            unsafe extern "C" fn(c_uint, c_uint),
        > = lib.get(b"retro_set_controller_port_device").unwrap();
        let retro_deinit: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"retro_deinit").unwrap();
        let retro_unload_game: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"retro_unload_game").unwrap();

        println!("retro_api_version = {}", retro_api_version());

        let mut info: retro_system_info = std::mem::zeroed();
        retro_get_system_info(&mut info);
        let cstr = |p: *const c_char| {
            if p.is_null() { "?".to_string() } else { CStr::from_ptr(p).to_string_lossy().to_string() }
        };
        println!(
            "library: {} {} | exts: {} | need_fullpath: {}",
            cstr(info.library_name), cstr(info.library_version), cstr(info.valid_extensions), info.need_fullpath
        );

        retro_set_environment(cb_environment);
        retro_set_video_refresh(cb_video_refresh);
        retro_set_audio_sample(cb_audio_sample);
        retro_set_audio_sample_batch(cb_audio_sample_batch);
        retro_set_input_poll(cb_input_poll);
        retro_set_input_state(cb_input_state);

        retro_init();

        let rom_bytes = std::fs::read(&rom_path).expect("read rom");
        println!("rom size = {} bytes | header CGB-flag(0x143)=0x{:02x}", rom_bytes.len(), rom_bytes[0x143]);
        let path_c = CString::new(rom_path.clone()).unwrap();
        let gi = retro_game_info {
            path: path_c.as_ptr(),
            data: rom_bytes.as_ptr() as *const c_void,
            size: rom_bytes.len(),
            meta: ptr::null(),
        };
        let ok = retro_load_game(&gi);
        println!("retro_load_game -> {}", ok);
        if !ok {
            eprintln!("LOAD FAILED");
            return;
        }

        let mut av: retro_system_av_info = std::mem::zeroed();
        retro_get_system_av_info(&mut av);
        println!(
            "av_info: geom base {}x{} max {}x{} aspect {:.4} | fps {:.4} sample_rate {:.1}",
            av.geometry.base_width, av.geometry.base_height,
            av.geometry.max_width, av.geometry.max_height,
            av.geometry.aspect_ratio, av.timing.fps, av.timing.sample_rate
        );
        {
            let mut st = STATE.lock().unwrap();
            st.sample_rate = av.timing.sample_rate;
            st.av_fps = av.timing.fps;
        }

        retro_set_controller_port_device(0, RETRO_DEVICE_JOYPAD);

        // ---- warm up (default 200) frames, no input. PROBE_WARM overrides. ----
        let warm_frames: u32 = std::env::var("PROBE_WARM").ok()
            .and_then(|s| s.parse().ok()).unwrap_or(200);
        let t0 = std::time::Instant::now();
        for _ in 0..warm_frames {
            retro_run();
        }
        let elapsed = t0.elapsed().as_secs_f64();
        let fps_measured = warm_frames as f64 / elapsed;

        let (w, h, pitch, pixfmt, frame, audio, srate, av_fps, hw, batch) = {
            let st = STATE.lock().unwrap();
            (st.width, st.height, st.pitch, st.pixfmt, st.last_frame.clone(),
             st.audio_samples, st.sample_rate, st.av_fps, st.hw_render_requested, st.channels_seen_batch)
        };

        let pixfmt_name = match pixfmt { 0 => "0RGB1555", 1 => "XRGB8888", 2 => "RGB565", _ => "?" };
        let checksum_a = fnv1a(&frame);
        let nonzero_a = frame.iter().filter(|&&b| b != 0).count();
        let colors_a = distinct_colors(&frame, w, h, pitch, pixfmt);
        let bpp = match pixfmt { 1 => 4, _ => 2 };
        println!("--------------------------------------------------");
        println!("HW_RENDER requested by core: {}", hw);
        println!("FRAMEBUFFER after {} frames: {}x{} pitch={} (={} px * {}B) fmt={} ({})",
            warm_frames, w, h, pitch, if bpp > 0 { pitch / bpp } else { 0 }, bpp, pixfmt, pixfmt_name);
        println!("  bytes captured: {} | checksum(FNV1a)=0x{:016x} | nonzero bytes: {} / {}",
            frame.len(), checksum_a, nonzero_a, frame.len());
        println!("  DISTINCT COLORS in frame: {}", colors_a);
        println!("AUDIO: {} stereo frames seen | rate {:.1} Hz | channels 2 (i16) | via batch_cb: {}",
            audio, srate, batch);
        println!("PERF: {} frames in {:.3}s => {:.1} fps (av target {:.4})",
            warm_frames, elapsed, fps_measured, av_fps);

        dump_ppm(&format!("/tmp/gbprobe/{}_warm.ppm", tag), &frame, w, h, pitch, pixfmt);

        // ---- mash START+A to advance past intros toward the (colorful) title ----
        // Tap each for 8 frames on, 8 off, repeated, to navigate the intro.
        for _ in 0..12 {
            { let mut inp = INPUT.lock().unwrap();
              inp.joypad[RETRO_DEVICE_ID_JOYPAD_START as usize] = true;
              inp.joypad[RETRO_DEVICE_ID_JOYPAD_A as usize] = true; }
            for _ in 0..8 { retro_run(); }
            { let mut inp = INPUT.lock().unwrap();
              inp.joypad[RETRO_DEVICE_ID_JOYPAD_START as usize] = false;
              inp.joypad[RETRO_DEVICE_ID_JOYPAD_A as usize] = false; }
            for _ in 0..8 { retro_run(); }
        }

        let frame_b = { STATE.lock().unwrap().last_frame.clone() };
        let checksum_b = fnv1a(&frame_b);
        let colors_b = distinct_colors(&frame_b, w, h, pitch, pixfmt);
        println!("--------------------------------------------------");
        println!("AFTER mashing START+A (+192 frames): checksum=0x{:016x} | distinct_colors {} | changed_vs_warm: {}",
            checksum_b, colors_b, checksum_b != checksum_a);
        dump_ppm(&format!("/tmp/gbprobe/{}_start.ppm", tag), &frame_b, w, h, pitch, pixfmt);

        // ---- let title screen animate (no input) to capture peak color variety ----
        for _ in 0..240 { retro_run(); }
        let frame_c = { STATE.lock().unwrap().last_frame.clone() };
        let checksum_c = fnv1a(&frame_c);
        let colors_c = distinct_colors(&frame_c, w, h, pitch, pixfmt);
        println!("AFTER +240 idle frames (title settled): checksum=0x{:016x} | distinct_colors {} | changed_vs_B: {}",
            checksum_c, colors_c, checksum_c != checksum_b);
        dump_ppm(&format!("/tmp/gbprobe/{}_title.ppm", tag), &frame_c, w, h, pitch, pixfmt);

        retro_unload_game();
        retro_deinit();
    }
    println!("=== done ({}) ===", std::env::args().nth(3).unwrap_or_default());
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn dump_ppm(path: &str, frame: &[u8], w: u32, h: u32, pitch: usize, pixfmt: u32) {
    use std::io::Write;
    if w == 0 || h == 0 || frame.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity((w * h * 3) as usize + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", w, h).as_bytes());
    for y in 0..h as usize {
        let row = y * pitch;
        for x in 0..w as usize {
            let (r, g, b) = decode_pixel(frame, row, pixfmt, x).unwrap_or((0, 0, 0));
            out.push(r); out.push(g); out.push(b);
        }
    }
    if let Ok(mut f) = std::fs::File::create(path) {
        let _ = f.write_all(&out);
    }
}
