// Headless libretro frontend probe for N64 (mupen64plus-next) -> SSB64
// Hand-rolled extern "C" callbacks + libloading symbol resolution.
use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

// ---------- libretro constants ----------
const RETRO_API_VERSION: c_uint = 1;

const RETRO_ENVIRONMENT_SET_ROTATION: c_uint = 1;
const RETRO_ENVIRONMENT_GET_OVERSCAN: c_uint = 2;
const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
const RETRO_ENVIRONMENT_SET_MESSAGE: c_uint = 6;
const RETRO_ENVIRONMENT_SHUTDOWN: c_uint = 7;
const RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL: c_uint = 8;
const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
const RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK: c_uint = 12;
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
const RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: c_uint = 51 | 0x10000; // EXPERIMENTAL
const RETRO_ENVIRONMENT_GET_FASTFORWARDING: c_uint = 50; // not critical
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;

const RETRO_PIXEL_FORMAT_0RGB1555: c_uint = 0;
const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;
const RETRO_PIXEL_FORMAT_RGB565: c_uint = 2;

// Devices
const RETRO_DEVICE_JOYPAD: c_uint = 1;
const RETRO_DEVICE_ANALOG: c_uint = 5;

// Joypad button ids
const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
const RETRO_DEVICE_ID_JOYPAD_Y: c_uint = 1;
const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;
const RETRO_DEVICE_ID_JOYPAD_X: c_uint = 9;
const RETRO_DEVICE_ID_JOYPAD_L: c_uint = 10;
const RETRO_DEVICE_ID_JOYPAD_R: c_uint = 11;
const RETRO_DEVICE_ID_JOYPAD_L2: c_uint = 12;
const RETRO_DEVICE_ID_JOYPAD_R2: c_uint = 13;

// Analog
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
    // pixel format the core requested (default 0RGB1555)
    pixfmt: u32,
    last_frame: Vec<u8>,
    frames_seen: u64,
    audio_samples: u64, // number of stereo frames
    sample_rate: f64,
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
    hw_render_requested: false,
});

// Input: pressed buttons bitset + analog
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

// directories (leaked CStrings, valid for program lifetime)
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
        // dupe frame (GET_CAN_DUPE). keep last frame.
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

// Forced core option overrides. Key -> value.
// RDP/RSP plugin chosen via env PROBE_RDP / PROBE_RSP / PROBE_MT for the matrix.
fn forced_option(key: &str) -> Option<String> {
    let rdp = std::env::var("PROBE_RDP").unwrap_or_else(|_| "angrylion".into());
    let rsp = std::env::var("PROBE_RSP").unwrap_or_else(|_| "cxd4".into());
    let mt = std::env::var("PROBE_MT").unwrap_or_else(|_| "1".into());
    match key {
        "mupen64plus-rdp-plugin" => Some(rdp),
        "mupen64plus-rsp-plugin" => Some(rsp),
        "mupen64plus-angrylion-multithread" => Some(mt),
        "mupen64plus-angrylion-sync" => Some("Low".into()),
        "mupen64plus-EnableFBEmulation" => Some("True".into()),
        "mupen64plus-43screensize" => Some("320x240".into()),
        "mupen64plus-aspect" => Some("4:3".into()),
        "mupen64plus-cpucore" => Some("dynamic_recompiler".into()),
        "mupen64plus-BilinearMode" => Some("standard".into()),
        _ => None,
    }
}

unsafe extern "C" fn cb_environment(cmd: c_uint, data: *mut c_void) -> bool {
    // Quiet the high-frequency polls (GET_VARIABLE=15, GET_VARIABLE_UPDATE=17).
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
            // We accept any; but we requested XRGB8888 implicitly by accepting it.
            // Core picks; mupen-next supports XRGB8888.
            eprintln!("[env] SET_PIXEL_FORMAT -> {}", fmt);
            // Accept only known formats
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
                // leak a CString so pointer stays valid
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
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY => {
            // We just acknowledge; our GET_VARIABLE handles forcing.
            true
        }
        RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
            if !data.is_null() {
                *(data as *mut c_uint) = 2;
            }
            true
        }
        RETRO_ENVIRONMENT_SET_HW_RENDER => {
            // The core wants a hardware GL context. With angrylion (software RDP)
            // mupen64plus-next should NOT require this. Record it and REFUSE so the
            // core falls back to software output via video_refresh.
            let mut st = STATE.lock().unwrap();
            st.hw_render_requested = true;
            eprintln!("[env] SET_HW_RENDER requested by core -> REFUSING (forcing software)");
            false
        }
        ENV_GET_PREFERRED_HW_RENDER => {
            // 0 = NONE. Tell the core we prefer no HW context.
            if !data.is_null() {
                *(data as *mut c_uint) = 0;
            }
            true
        }
        RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {
            // Provide a real C-variadic log callback (compiled via logshim.c).
            // The core stores this pointer and calls it; declining it leaves a
            // NULL fn-ptr that the core blindly calls -> SIGSEGV. (Root cause of
            // the angrylion crash.)
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
        _ => {
            // Unknown / unhandled. Return false.
            false
        }
    }
}

// ---------- main ----------
fn main() {
    let core_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/n64probe/mupen64plus_next_libretro.dylib".to_string());
    let rom_path = std::env::args().nth(2).unwrap_or_else(|| {
        "~/pokemon-pvp-red/Super Smash Bros. (U) [!].z64".to_string()
    });

    // set up dirs
    unsafe {
        SYS_DIR = CString::new("/tmp/n64probe/systemdir").unwrap().into_raw();
        SAVE_DIR = CString::new("/tmp/n64probe/savedir").unwrap().into_raw();
    }

    println!("=== N64 mupen64plus-next headless probe ===");
    println!("core: {}", core_path);
    println!("rom:  {}", rom_path);

    let lib = unsafe { libloading::Library::new(&core_path).expect("load core dylib") };

    unsafe {
        // resolve symbols
        let retro_api_version: libloading::Symbol<unsafe extern "C" fn() -> c_uint> =
            lib.get(b"retro_api_version").unwrap();
        let retro_get_system_info: libloading::Symbol<
            unsafe extern "C" fn(*mut retro_system_info),
        > = lib.get(b"retro_get_system_info").unwrap();
        let retro_set_environment: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool),
        > = lib.get(b"retro_set_environment").unwrap();
        let retro_set_video_refresh: libloading::Symbol<
            unsafe extern "C" fn(
                unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize),
            ),
        > = lib.get(b"retro_set_video_refresh").unwrap();
        let retro_set_audio_sample: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(i16, i16)),
        > = lib.get(b"retro_set_audio_sample").unwrap();
        let retro_set_audio_sample_batch: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(*const i16, usize) -> usize),
        > = lib.get(b"retro_set_audio_sample_batch").unwrap();
        let retro_set_input_poll: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn()),
        > = lib.get(b"retro_set_input_poll").unwrap();
        let retro_set_input_state: libloading::Symbol<
            unsafe extern "C" fn(unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16),
        > = lib.get(b"retro_set_input_state").unwrap();
        let retro_init: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"retro_init").unwrap();
        let retro_load_game: libloading::Symbol<
            unsafe extern "C" fn(*const retro_game_info) -> bool,
        > = lib.get(b"retro_load_game").unwrap();
        let retro_get_system_av_info: libloading::Symbol<
            unsafe extern "C" fn(*mut retro_system_av_info),
        > = lib.get(b"retro_get_system_av_info").unwrap();
        let retro_run: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"retro_run").unwrap();
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
        let libname = if info.library_name.is_null() {
            "?".into()
        } else {
            CStr::from_ptr(info.library_name).to_string_lossy().to_string()
        };
        let libver = if info.library_version.is_null() {
            "?".into()
        } else {
            CStr::from_ptr(info.library_version)
                .to_string_lossy()
                .to_string()
        };
        let exts = if info.valid_extensions.is_null() {
            "?".into()
        } else {
            CStr::from_ptr(info.valid_extensions)
                .to_string_lossy()
                .to_string()
        };
        println!(
            "library: {} {} | exts: {} | need_fullpath: {}",
            libname, libver, exts, info.need_fullpath
        );

        // Register callbacks BEFORE init/load (environment first per spec).
        retro_set_environment(cb_environment);
        retro_set_video_refresh(cb_video_refresh);
        retro_set_audio_sample(cb_audio_sample);
        retro_set_audio_sample_batch(cb_audio_sample_batch);
        retro_set_input_poll(cb_input_poll);
        retro_set_input_state(cb_input_state);

        retro_init();

        // load ROM into memory (need_fullpath may be false; provide both)
        let rom_bytes = std::fs::read(&rom_path).expect("read rom");
        println!("rom size = {} bytes", rom_bytes.len());
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

        // av info
        let mut av: retro_system_av_info = std::mem::zeroed();
        retro_get_system_av_info(&mut av);
        println!(
            "av_info: geom base {}x{} max {}x{} aspect {:.3} | fps {:.4} sample_rate {:.1}",
            av.geometry.base_width,
            av.geometry.base_height,
            av.geometry.max_width,
            av.geometry.max_height,
            av.geometry.aspect_ratio,
            av.timing.fps,
            av.timing.sample_rate
        );
        {
            let mut st = STATE.lock().unwrap();
            st.sample_rate = av.timing.sample_rate;
        }

        // controller port 0 = JOYPAD
        retro_set_controller_port_device(0, RETRO_DEVICE_JOYPAD);

        // ---- run frames, no input ----
        let t0 = std::time::Instant::now();
        let warm_frames = 150u32;
        for _ in 0..warm_frames {
            retro_run();
        }
        let elapsed = t0.elapsed().as_secs_f64();
        let fps_measured = warm_frames as f64 / elapsed;

        let (w, h, pitch, pixfmt, frame, audio, srate, hw) = {
            let st = STATE.lock().unwrap();
            (
                st.width,
                st.height,
                st.pitch,
                st.pixfmt,
                st.last_frame.clone(),
                st.audio_samples,
                st.sample_rate,
                st.hw_render_requested,
            )
        };

        let pixfmt_name = match pixfmt {
            0 => "0RGB1555",
            1 => "XRGB8888",
            2 => "RGB565",
            _ => "?",
        };
        let checksum_a = fnv1a(&frame);
        let nonzero_a = frame.iter().filter(|&&b| b != 0).count();
        println!("--------------------------------------------------");
        println!("HW_RENDER requested by core: {}", hw);
        println!(
            "FRAMEBUFFER after {} frames: {}x{} pitch={} fmt={} ({})",
            warm_frames, w, h, pitch, pixfmt, pixfmt_name
        );
        println!(
            "  bytes captured: {} | checksum(FNV1a)=0x{:016x} | nonzero bytes: {} / {}",
            frame.len(),
            checksum_a,
            nonzero_a,
            frame.len()
        );
        println!(
            "AUDIO: {} stereo frames seen | rate {:.1} Hz | channels 2 (i16)",
            audio, srate
        );
        println!(
            "PERF: {} frames in {:.3}s => {:.1} fps (target {:.2})",
            warm_frames, elapsed, fps_measured, av.timing.fps
        );

        // ---- press START for some frames, then check checksum changes ----
        {
            let mut inp = INPUT.lock().unwrap();
            inp.joypad[RETRO_DEVICE_ID_JOYPAD_START as usize] = true;
        }
        for _ in 0..60 {
            retro_run();
        }
        {
            let mut inp = INPUT.lock().unwrap();
            inp.joypad[RETRO_DEVICE_ID_JOYPAD_START as usize] = false;
        }
        // a few more frames to let menu settle
        for _ in 0..60 {
            retro_run();
        }

        let (frame_b, nonzero_b) = {
            let st = STATE.lock().unwrap();
            let nz = st.last_frame.iter().filter(|&&b| b != 0).count();
            (st.last_frame.clone(), nz)
        };
        let checksum_b = fnv1a(&frame_b);
        println!("--------------------------------------------------");
        println!(
            "AFTER pressing START (+120 frames): checksum=0x{:016x} | nonzero {} | changed_vs_before: {}",
            checksum_b,
            nonzero_b,
            checksum_b != checksum_a
        );

        // also test analog stick + A to further prove input plumbing
        {
            let mut inp = INPUT.lock().unwrap();
            inp.analog_x = 30000;
            inp.joypad[RETRO_DEVICE_ID_JOYPAD_A as usize] = true;
        }
        for _ in 0..60 {
            retro_run();
        }
        let frame_c = {
            let st = STATE.lock().unwrap();
            st.last_frame.clone()
        };
        let checksum_c = fnv1a(&frame_c);
        println!(
            "AFTER analog-right + A (+60 frames): checksum=0x{:016x} | changed_vs_B: {}",
            checksum_c,
            checksum_c != checksum_b
        );

        // dump a PPM so we can visually confirm if needed
        if w > 0 && h > 0 && !frame_c.is_empty() {
            dump_ppm("/tmp/n64probe/frame.ppm", &frame_c, w, h, pitch, pixfmt);
            println!("wrote /tmp/n64probe/frame.ppm");
        }

        retro_unload_game();
        retro_deinit();
    }

    println!("=== done ===");
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
    let mut out = Vec::with_capacity((w * h * 3) as usize + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", w, h).as_bytes());
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (r, g, b) = match pixfmt {
                1 => {
                    // XRGB8888 little-endian in memory: B,G,R,X
                    let off = y * pitch + x * 4;
                    if off + 3 < frame.len() {
                        (frame[off + 2], frame[off + 1], frame[off])
                    } else {
                        (0, 0, 0)
                    }
                }
                2 => {
                    // RGB565
                    let off = y * pitch + x * 2;
                    if off + 1 < frame.len() {
                        let v = u16::from_le_bytes([frame[off], frame[off + 1]]);
                        let r = ((v >> 11) & 0x1f) as u8;
                        let g = ((v >> 5) & 0x3f) as u8;
                        let b = (v & 0x1f) as u8;
                        ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
                    } else {
                        (0, 0, 0)
                    }
                }
                _ => {
                    // 0RGB1555
                    let off = y * pitch + x * 2;
                    if off + 1 < frame.len() {
                        let v = u16::from_le_bytes([frame[off], frame[off + 1]]);
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
    if let Ok(mut f) = std::fs::File::create(path) {
        let _ = f.write_all(&out);
    }
}
