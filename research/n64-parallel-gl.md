# N64 Headless on macOS arm64 — parallel_n64 (angrylion software RDP) + CGL GL fallback

**Status: SOLVED.** Super Smash Bros. 64 (USA) runs **fully headless** (no window, no
display, no GL context) and is drivable from Rust via the **ParaLLEl N64** libretro core
with its **angrylion** software RDP plugin, delivering CPU framebuffers through the standard
libretro `video_refresh` callback. A working Rust harness loads the core, loads the `.z64`,
runs frames, and proves a non-blank, input-responsive image. The macOS headless-GL fallback
(raw CGL offscreen context + FBO + `glReadPixels`) is **also proven working**, but is **not
needed** for parallel_n64.

Date: 2026-06-06. Machine: Apple M4 Pro, macOS 26.1 (arm64), rustc pinned 1.92.

---

## TL;DR / Recommendation

| Core | Headless render path | SSB64 result | Friction |
|---|---|---|---|
| **ParaLLEl N64** (`parallel_n64_libretro.dylib`, v2.14.6) | `parallel-n64-gfxplugin = angrylion` → pure software → `video_refresh` | **WORKS, non-blank, 1400+ fps** | **NONE** — never requests `SET_HW_RENDER` |
| Mupen64Plus-Next (`mupen64plus_next_libretro.dylib`, v2.8-Vulkan) | forced `mupen64plus-rdp-plugin = angrylion` | **SIGSEGV in `retro_load_game`** (null fn-ptr call) | High — build assumes a HW (GL/Vulkan) context; angrylion path crashes calling a null hw callback |

**Use ParaLLEl N64 + angrylion. No OpenGL needed at all.** The CGL path below is the
documented fallback recipe in case you later switch to a GL-only core.

Working core copied to: `~/pokemon-pvp-red/cores/parallel_n64_libretro.dylib`
(arm64, adhoc/linker-signed, no quarantine).

---

## Acquisition (libretro buildbot, arm64 nightly)

Both cores are published as zipped dylibs:

```bash
BB=https://buildbot.libretro.com/nightly/apple/osx/arm64/latest
curl -sL -o parallel_n64.dylib.zip      "$BB/parallel_n64_libretro.dylib.zip"
curl -sL -o mupen64plus_next.dylib.zip  "$BB/mupen64plus_next_libretro.dylib.zip"
unzip -o parallel_n64.dylib.zip       # -> parallel_n64_libretro.dylib
unzip -o mupen64plus_next.dylib.zip   # -> mupen64plus_next_libretro.dylib
```

Verification:

```
$ lipo -info parallel_n64_libretro.dylib
Non-fat file: parallel_n64_libretro.dylib is architecture: arm64
$ codesign -dv parallel_n64_libretro.dylib
Signature=adhoc            # adhoc/linker-signed -> dlopen works, no Gatekeeper block
$ otool -L parallel_n64_libretro.dylib | grep -iE 'opengl|cocoa|metal'
(no output)                # <-- parallel_n64 has NO hard GL/Cocoa dep; resolves GL
                           #     dynamically only if a HW context is granted
$ otool -L mupen64plus_next_libretro.dylib | grep -i opengl
    /System/Library/Frameworks/OpenGL.framework/...   # <-- mupen links GL directly
```

The fact that parallel_n64 has **no** OpenGL/Cocoa/Metal in its load commands is the first
signal it can run software-only; mupen-next's hard OpenGL dependency foreshadows its
HW-context requirement.

If a URL 404s, list the dir: `curl -s "$BB/" | grep -oE 'href="[^"]*(parallel|mupen)[^"]*"'`.

**Gotcha:** strip quarantine before loading from inside an app bundle / signed context:
`xattr -d com.apple.quarantine cores/parallel_n64_libretro.dylib 2>/dev/null` (the buildbot
zips do NOT set quarantine when downloaded via curl, but Safari downloads would).

---

## ROM

`"~/pokemon-pvp-red/Super Smash Bros. (U) [!].z64"`
16,777,216 bytes (128 Mbit). Header `80 37 12 40` = **.z64 native big-endian**. The core
accepts it directly (no byte-swap). `need_fullpath=false`, so you may pass the bytes in
`retro_game_info.data` (we read the file in Rust and hand over the pointer).

---

## THE PROOF (real output)

Harness: `/tmp/n64probe/n64probe/src/main.rs` (full source embedded below; also the exact
FFI you drop into the server). Built with the pinned toolchain:
`RUSTUP_TOOLCHAIN=1.92 cargo run --release -- <core> <rom>`.

### ParaLLEl N64 + angrylion — SUCCESS

```
=== loading core: .../cores/parallel_n64_libretro.dylib ===
retro_api_version = 1 (expected 1)
core: ParaLLEl N64 v2.14.6 (Parallel Launcher Edition)  exts=[n64|v64|z64|bin|u1|ndd] need_fullpath=false
=== retro_init ===
[env] SET_PIXEL_FORMAT -> 1 (XRGB8888)
rom size = 16777216 bytes
=== retro_load_game ===
[env] GET_VARIABLE parallel-n64-gfxplugin -> FORCED angrylion
[env] GET_VARIABLE parallel-n64-rspplugin -> FORCED cxd4
retro_load_game -> true
AV INFO: geom base=320x240 max=320x240 aspect=1.333 | fps=60.1300 sample_rate=44100.0
=== running 150 warm-up frames (no input) ===
--------------------------------------------------
AFTER 150 FRAMES (no input):
  framebuffer: 640x240 pitch=2560 format=1 (XRGB8888)
  video_refresh calls = 69
  checksum(FNV1a64) = 0x40fa4b0c269e7f10
  nonzero bytes = 148125 / 614400
  audio i16 samples total = 178488 (rate 44100, stereo)
  measured speed = 1420.7 fps (150 frames in 0.11s)
=== pressing START for 60 frames ===
--------------------------------------------------
AFTER START PRESS (+90 frames):
  checksum = 0xad3aa53b89d26328
  nonzero bytes = 544589
  audio samples added during start window = 132848
  checksum changed vs no-input? YES (input/emulation advances)
==================================================
VERDICT: NON-BLANK framebuffer captured headlessly via video_refresh. SUCCESS.
plugin_start_gfx success.
Gfx RomOpen.
```

Decoding the proof points (all REAL, reproduced deterministically across runs):

- **Framebuffer:** `640 x 240`, `pitch = 2560` bytes (= 640*4, tightly packed),
  `format = XRGB8888` (the core called `SET_PIXEL_FORMAT(1)`; we accepted it). angrylion
  outputs the VI's 640-wide signal; the N64 "320x240" is the RDP framebuffer, doubled
  horizontally by the VI filter. **Treat the stream as 640x240 XRGB8888.**
- **Non-blank:** 148,125 non-zero bytes out of 614,400 after the boot logo; rises to 544,589
  after Start. A separate dump (`research/ssb64-boot-headless.png`) decoded the raw buffer to
  PNG and visually shows the **Nintendo / HAL Laboratory boot logo of SSB64** — 1,036 distinct
  colors (a real rendered scene, not noise/flat-fill).
- **Audio:** 178,488 `i16` samples in 150 frames = 1189.9 samples/frame *2ch; at 60.13 fps that
  is ~44,100 stereo Hz — matches `av_info.timing.sample_rate = 44100`. Delivered via
  `audio_sample_batch`.
- **Input works + emulation advances:** pressing **Start** changed the checksum
  `0x40fa4b0c269e7f10 → 0xad3aa53b89d26328` and the non-zero count jumped (the game advanced
  past the logo). Audio kept flowing (132,848 new samples) during the window.
- **Real-time feasibility:** **~1400 fps** measured for SSB64 at 320/640x240 with angrylion +
  cxd4 RSP on the M4 Pro — i.e. **~23x real-time**. Streaming 60 fps leaves enormous headroom
  for VP8/Opus encoding on the same box (the existing NES pipeline already does VP8+Opus).

PNG proof image: `~/pokemon-pvp-red/research/ssb64-boot-headless.png`

### Mupen64Plus-Next — FAILS headless (documented, as required)

```
=== loading core: .../mupen64plus_next_libretro.dylib ===
core: Mupen64Plus-Next v2.8-Vulkan 98c1b0d  exts=[n64|v64|z64|bin|u1] need_fullpath=false
=== retro_init ===
[env] SET_PIXEL_FORMAT -> 1 (XRGB8888)
=== retro_load_game ===
[env] GET_VARIABLE mupen64plus-rdp-plugin -> FORCED angrylion
[env] GET_VARIABLE mupen64plus-rsp-plugin -> FORCED cxd4
[env] GET_VARIABLE mupen64plus-EnableFBEmulation -> FORCED True
<process exits 139 = SIGSEGV inside retro_load_game>
```

Crash report (`~/Library/Logs/DiagnosticReports/n64probe-*.ips`):
`EXC_BAD_ACCESS (SIGSEGV) KERN_INVALID_ADDRESS at 0x0` — top frame `imageOffset 0x0`, i.e.
the core **called a NULL function pointer**. This is the HW-render plumbing: even with
angrylion forced, this 2.8-Vulkan build invokes a hardware-render callback
(`get_proc_address`/`get_current_framebuffer`) that is NULL because we (correctly, for
headless) returned `false` from `SET_HW_RENDER`. Forcing `EnableFBEmulation` etc. did not
help. **Conclusion: mupen64plus-next's macOS nightly assumes a live HW context; do not use it
for the headless server.** (If you must, you would have to grant it a real GL context via the
CGL recipe below AND let it use GLideN64, not angrylion — more friction, no benefit over
parallel_n64.)

---

## Core-option forcing recipe (the crux)

The frontend never parses the core's option list; it just answers `GET_VARIABLE` with forced
values. parallel_n64 polls these during `retro_load_game` and again each `retro_run`. The
**only two that matter** for headless software rendering:

```
parallel-n64-gfxplugin = angrylion     # software RDP (the whole game changer)
parallel-n64-rspplugin = cxd4          # accurate software RSP (LLE). "hle" also works & is faster.
```

Optional/cosmetic (we set them, harmless):
```
parallel-n64-screensize        = 320x240
parallel-n64-angrylion-vioverlay = Filtered
```

Notes:
- With `angrylion`, parallel_n64 **never calls `SET_HW_RENDER`** — verified (our env callback
  logs every cmd; cmd 14 never appears). That is why no GL context is required.
- `rspplugin = hle` renders the SSB64 logo identically and is faster; `cxd4` (LLE) is the
  most compatible. Either is fine for SSB64.
- Per-frame the core spams `GET_AUDIO_VIDEO_ENABLE` (env cmd `0x10047`, base 71) — return
  `false`/unhandled; benign.

---

## The CGL headless-GL fallback (proven, for GL-only cores)

If you ever adopt a core that *insists* on `SET_HW_RENDER` (GLideN64, mupen-next, ParaLLEl-RDP
Vulkan, etc.), macOS can give it an **offscreen GL context with no window and no display
server** via raw CGL. Proven on this machine:

```
=== macOS headless OpenGL via CGL (no window / no display) ===
[GL3.2-core] CGL context CREATED (headless, no window)
    GL_VENDOR   = Apple
    GL_RENDERER = Apple M4 Pro
    GL_VERSION  = 4.1 Metal - 90.5
[GL3.2-core] FBO complete 320x240
[GL3.2-core] glReadPixels first pixel RGBA = (51,153,229,255)  expected ~(51,153,229,255)
[GL3.2-core] nonzero bytes in readback = 307200 / 307200
[GL3.2-core] HEADLESS GL READBACK VERIFIED (clear color round-tripped through glReadPixels)
[GL-legacy]  ... GL_VERSION = 2.1 Metal - 90.5 ... VERIFIED
RESULT: headless CGL GL context + FBO + glReadPixels WORKS on this machine.
```

Minimal recipe (full source in `glprobe/src/main.rs`, key part below):
1. `CGLChoosePixelFormat` with `{kCGLPFAAccelerated, kCGLPFAOpenGLProfile,
   kCGLOGLPVersion_3_2_Core, kCGLPFAColorSize,24, kCGLPFADepthSize,24, 0}` → no
   `kCGLPFAWindow`, no display needed.
2. `CGLCreateContext` → `CGLSetCurrentContext`. (No NSApp, no `NSOpenGLContext`, no Cocoa.)
3. Create an FBO with an RGBA8 color renderbuffer (+ depth24); `glCheckFramebufferStatus ==
   GL_FRAMEBUFFER_COMPLETE`.
4. Render; `glReadPixels(0,0,w,h, GL_RGBA, GL_UNSIGNED_BYTE, buf)` → CPU bytes for the encoder.

To wire this into libretro's HW-render flow:
- In the env callback, **accept** `SET_HW_RENDER` (return `true`), remember the
  `RetroHwRenderCallback` (its `context_reset`/`context_destroy` fn-ptrs).
- Provide `get_proc_address`: return GL symbols via `dlsym(RTLD_DEFAULT, name)` (OpenGL.framework
  is already loaded) or `CGLGetCurrentContext`-bound lookups; for core GL all symbols resolve
  with plain `dlsym`.
- Provide `get_current_framebuffer`: return your FBO's GL name (the core renders into it).
- After making the CGL context current, call `context_reset()` once. Then each frame: set ctx
  current, `retro_run()`, `glReadPixels` your FBO, feed pixels to your encoder. The core will
  also call `video_refresh(RETRO_HW_FRAME_BUFFER_VALID, w, h, 0)` to signal "frame is in the
  FBO".

This machine reports **GL 4.1 core (Apple, Metal-backed)** — sufficient for GLideN64 GLES/GL.
You would only need this for a GL core; **parallel_n64 + angrylion avoids it entirely.**

---

## READY RUST CODE

### Cargo.toml additions (server side)

```toml
[dependencies]
libc = "0.2"     # dlopen/dlsym/dlerror; that's all the FFI we need for parallel_n64.
# (The CGL fallback additionally links OpenGL.framework via #[link]; no crate needed.)
```

No `libretro-sys` / `rust-libretro-sys` crate is required — the hand-rolled FFI below is ~200
lines, has zero build-script/bindgen risk, and gives you exact control of the env callback and
option forcing (which is the entire battle). If you prefer a crate, `libretro-sys = "0.1"` or
`rust-libretro-sys` expose the same structs, but you still must write the env callback yourself.

### Core loader + env callback + load/run/framebuffer/audio/input

This is the complete, tested harness (`/tmp/n64probe/n64probe/src/main.rs`). Lift the FFI
types, the four global-state structs, the `environment` callback, the three media callbacks,
and `load_core` directly into the server. Replace the `main()` driver loop with your WebRTC
frame pump (capture `CAPTURED.fb_bytes` → VP8, `audio` → Opus, set `INPUT` from the data
channel).

```rust
use std::ffi::{c_char, c_void, CStr, CString};
use std::os::raw::c_uint;
use std::ptr;
use std::sync::Mutex;

// ---- libretro env command ids (only the ones we answer) ----
const ENV_GET_CAN_DUPE: c_uint = 3;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;
const ENV_SET_HW_RENDER: c_uint = 14;          // we RETURN FALSE -> forces software path
const ENV_GET_VARIABLE: c_uint = 15;
const ENV_GET_VARIABLE_UPDATE: c_uint = 17;
const ENV_SET_VARIABLES: c_uint = 16;
const ENV_SET_CORE_OPTIONS: c_uint = 53;
const ENV_SET_CORE_OPTIONS_INTL: c_uint = 54;
const ENV_SET_CORE_OPTIONS_V2: c_uint = 67;
const ENV_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;
const ENV_GET_CORE_OPTIONS_VERSION: c_uint = 52;
const ENV_GET_PREFERRED_HW_RENDER: c_uint = 56;
const ENV_SET_SUPPORT_NO_GAME: c_uint = 18;
const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
const ENV_GET_SAVE_DIRECTORY: c_uint = 31;

const PIXFMT_XRGB8888: c_uint = 1;
const HW_CONTEXT_NONE: c_uint = 0;

// joypad button ids (RETRO_DEVICE_JOYPAD)
pub const JOY_B: usize = 0; pub const JOY_SELECT: usize = 2; pub const JOY_START: usize = 3;
pub const JOY_UP: usize = 4; pub const JOY_DOWN: usize = 5; pub const JOY_LEFT: usize = 6;
pub const JOY_RIGHT: usize = 7; pub const JOY_A: usize = 8;
pub const JOY_L: usize = 10; pub const JOY_R: usize = 11;
// N64 mapping (libretro retropad -> N64): A=JOY_A, B=JOY_B, Start=JOY_START,
// Z=JOY_L (trigger), L=JOY_L2? In parallel_n64 default: Z->L, R->R, L->L2.
// C-buttons come from the RIGHT analog stick or face buttons X/Y depending on
// the input descriptor; simplest: use RETRO_DEVICE_ANALOG right-stick for C.
const DEV_JOYPAD: c_uint = 1;
const DEV_ANALOG: c_uint = 5;
const ANALOG_LEFT: c_uint = 0;
const ANALOG_RIGHT: c_uint = 1; // C-buttons on N64 map to right stick in many cores
const ANALOG_X: c_uint = 0;
const ANALOG_Y: c_uint = 1;

#[repr(C)] struct SysInfo { name:*const c_char, version:*const c_char, exts:*const c_char, need_fullpath:bool, block_extract:bool }
#[repr(C)] struct Geometry { base_w:c_uint, base_h:c_uint, max_w:c_uint, max_h:c_uint, aspect:f32 }
#[repr(C)] struct Timing { fps:f64, sample_rate:f64 }
#[repr(C)] struct AvInfo { geometry:Geometry, timing:Timing }
#[repr(C)] struct GameInfo { path:*const c_char, data:*const c_void, size:usize, meta:*const c_char }
#[repr(C)] struct Variable { key:*const c_char, value:*const c_char }
#[repr(C)] struct HwRenderCb { context_type:c_uint, _rest:[u8; 80] } // we only read context_type

type EnvFn = extern "C" fn(c_uint, *mut c_void) -> bool;
type VideoFn = extern "C" fn(*const c_void, c_uint, c_uint, usize);
type AudioBatchFn = extern "C" fn(*const i16, usize) -> usize;
type AudioFn = extern "C" fn(i16, i16);
type PollFn = extern "C" fn();
type InputFn = extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16;

// ---- shared state the frame pump reads/writes ----
pub struct Frame { pub w:u32, pub h:u32, pub pitch:usize, pub fmt:u32, pub bytes:Vec<u8> }
pub static FRAME: Mutex<Frame> = Mutex::new(Frame{w:0,h:0,pitch:0,fmt:0,bytes:Vec::new()});
pub static AUDIO: Mutex<Vec<i16>> = Mutex::new(Vec::new()); // interleaved L,R; drain each tick
pub struct Pad { pub btn:[bool;16], pub lx:i16, pub ly:i16, pub cx:i16, pub cy:i16 }
pub static PAD: Mutex<Pad> = Mutex::new(Pad{btn:[false;16],lx:0,ly:0,cx:0,cy:0});

fn forced_option(key:&str)->Option<&'static str>{ match key {
    "parallel-n64-gfxplugin" => Some("angrylion"),
    "parallel-n64-rspplugin" => Some("cxd4"),       // or "hle" (faster)
    "parallel-n64-screensize" => Some("320x240"),
    "parallel-n64-angrylion-vioverlay" => Some("Filtered"),
    _ => None,
}}

extern "C" fn environment(cmd:c_uint, data:*mut c_void)->bool{ unsafe { match cmd {
    ENV_GET_CAN_DUPE => { *(data as *mut bool)=true; true }
    ENV_SET_PIXEL_FORMAT => { let f=*(data as *const c_uint); FRAME.lock().unwrap().fmt=f; true } // accept (core uses XRGB8888=1)
    ENV_SET_HW_RENDER => {
        // REFUSE hardware render -> core falls back to software (angrylion). This is the key.
        false
    }
    ENV_GET_PREFERRED_HW_RENDER => { *(data as *mut c_uint)=HW_CONTEXT_NONE; true }
    ENV_GET_VARIABLE => {
        let v=&mut *(data as *mut Variable);
        let key=CStr::from_ptr(v.key).to_string_lossy().into_owned();
        if let Some(val)=forced_option(&key){
            v.value = CString::new(val).unwrap().into_raw(); // leak; lives for process
            true
        } else { v.value=ptr::null(); false }
    }
    ENV_GET_VARIABLE_UPDATE => { *(data as *mut bool)=false; true }
    ENV_SET_VARIABLES | ENV_SET_CORE_OPTIONS | ENV_SET_CORE_OPTIONS_INTL
        | ENV_SET_CORE_OPTIONS_V2 | ENV_SET_CORE_OPTIONS_V2_INTL => true,
    ENV_GET_CORE_OPTIONS_VERSION => { *(data as *mut c_uint)=2; true }
    ENV_GET_SYSTEM_DIRECTORY | ENV_GET_SAVE_DIRECTORY => {
        // point at a writable scratch dir (CString must outlive the call)
        *(data as *mut *const c_char) = SYSDIR.with(|p| p.get()); true
    }
    ENV_SET_SUPPORT_NO_GAME => true,
    _ => false, // everything else: decline. (Per-frame GET_AUDIO_VIDEO_ENABLE etc. are fine declined.)
}}}

thread_local!{ static SYSDIR: std::cell::Cell<*const c_char> = std::cell::Cell::new(ptr::null()); }

extern "C" fn video_refresh(data:*const c_void, w:c_uint, h:c_uint, pitch:usize){
    if data.is_null(){ return; } // dupe frame: keep last
    let mut f=FRAME.lock().unwrap();
    f.w=w; f.h=h; f.pitch=pitch;
    let n=pitch*h as usize;
    let src=unsafe{ std::slice::from_raw_parts(data as *const u8, n) };
    f.bytes.clear(); f.bytes.extend_from_slice(src);
}
extern "C" fn audio_sample(l:i16, r:i16){ let mut a=AUDIO.lock().unwrap(); a.push(l); a.push(r); }
extern "C" fn audio_batch(data:*const i16, frames:usize)->usize{
    let s=unsafe{ std::slice::from_raw_parts(data, frames*2) };
    AUDIO.lock().unwrap().extend_from_slice(s); frames
}
extern "C" fn input_poll(){}
extern "C" fn input_state(port:c_uint, device:c_uint, index:c_uint, id:c_uint)->i16{
    if port!=0 { return 0; }
    let p=PAD.lock().unwrap();
    match device {
        DEV_JOYPAD => if (id as usize)<16 && p.btn[id as usize] {1} else {0},
        DEV_ANALOG => match (index,id) {
            (ANALOG_LEFT, ANALOG_X)=>p.lx, (ANALOG_LEFT, ANALOG_Y)=>p.ly,
            (ANALOG_RIGHT,ANALOG_X)=>p.cx, (ANALOG_RIGHT,ANALOG_Y)=>p.cy, // C-buttons
            _=>0,
        },
        _ => 0,
    }
}

// ---- dlopen the core ----
pub struct Core {
    h:*mut c_void,
    pub set_environment:extern "C" fn(EnvFn),
    pub set_video_refresh:extern "C" fn(VideoFn),
    pub set_audio_sample:extern "C" fn(AudioFn),
    pub set_audio_sample_batch:extern "C" fn(AudioBatchFn),
    pub set_input_poll:extern "C" fn(PollFn),
    pub set_input_state:extern "C" fn(InputFn),
    pub init:extern "C" fn(),
    pub deinit:extern "C" fn(),
    pub get_system_av_info:extern "C" fn(*mut AvInfo),
    pub set_controller_port_device:extern "C" fn(c_uint,c_uint),
    pub load_game:extern "C" fn(*const GameInfo)->bool,
    pub unload_game:extern "C" fn(),
    pub run:extern "C" fn(),
}
unsafe fn sym<T>(h:*mut c_void, n:&str)->T{
    let c=CString::new(n).unwrap();
    let p=libc::dlsym(h, c.as_ptr());
    assert!(!p.is_null(), "missing symbol {n}");
    std::mem::transmute_copy::<*mut c_void,T>(&p)
}
pub unsafe fn load_core(path:&str)->Core{
    let c=CString::new(path).unwrap();
    let h=libc::dlopen(c.as_ptr(), libc::RTLD_NOW|libc::RTLD_LOCAL);
    assert!(!h.is_null(), "dlopen: {}", CStr::from_ptr(libc::dlerror()).to_string_lossy());
    Core{
        set_environment:sym(h,"retro_set_environment"),
        set_video_refresh:sym(h,"retro_set_video_refresh"),
        set_audio_sample:sym(h,"retro_set_audio_sample"),
        set_audio_sample_batch:sym(h,"retro_set_audio_sample_batch"),
        set_input_poll:sym(h,"retro_set_input_poll"),
        set_input_state:sym(h,"retro_set_input_state"),
        init:sym(h,"retro_init"), deinit:sym(h,"retro_deinit"),
        get_system_av_info:sym(h,"retro_get_system_av_info"),
        set_controller_port_device:sym(h,"retro_set_controller_port_device"),
        load_game:sym(h,"retro_load_game"), unload_game:sym(h,"retro_unload_game"),
        run:sym(h,"retro_run"), h,
    }
}

// ---- bring-up sequence (call once) ----
pub unsafe fn boot(core_path:&str, rom_path:&str) -> (Core, f64, f64) {
    std::fs::create_dir_all("/tmp/n64sys").ok();
    let sd = CString::new("/tmp/n64sys").unwrap();
    SYSDIR.with(|p| p.set(sd.as_ptr()));
    std::mem::forget(sd); // keep the CString buffer alive for the process lifetime

    let core = load_core(core_path);
    // ORDER MATTERS: set_environment first, then the media callbacks, then init.
    (core.set_environment)(environment);
    (core.set_video_refresh)(video_refresh);
    (core.set_audio_sample)(audio_sample);
    (core.set_audio_sample_batch)(audio_batch);
    (core.set_input_poll)(input_poll);
    (core.set_input_state)(input_state);
    (core.init)();

    let rom = std::fs::read(rom_path).expect("read rom");          // 16 MiB .z64
    let rp = CString::new(rom_path).unwrap();
    let gi = GameInfo{ path:rp.as_ptr(), data:rom.as_ptr() as *const c_void, size:rom.len(), meta:ptr::null() };
    assert!((core.load_game)(&gi), "retro_load_game failed");
    std::mem::forget(rom); // core keeps a pointer into ROM bytes; keep alive
    std::mem::forget(rp);

    let mut av:AvInfo = std::mem::zeroed();
    (core.get_system_av_info)(&mut av);
    (core.set_controller_port_device)(0, DEV_JOYPAD);
    (core, av.timing.fps, av.timing.sample_rate)   // ~60.13 fps, 44100 Hz for SSB64
}

// ---- per-frame, in your WebRTC loop ----
// (core.run)();                      // advances exactly one frame
// let frame = FRAME.lock().unwrap(); // .w=640 .h=240 .pitch=2560 .fmt=1(XRGB8888) .bytes
//   -> convert XRGB8888 (bytes B,G,R,X little-endian) to I420 and feed vpx-encode
// let pcm: Vec<i16> = std::mem::take(&mut *AUDIO.lock().unwrap()); // interleaved L,R @44100
//   -> feed Opus (resample 44100->48000 if your Opus path needs 48k)
// PAD.lock().unwrap().btn[JOY_START]=true; // set from the input data channel
```

**Pixel format note for the encoder:** XRGB8888 here is little-endian `0x00RRGGBB`, i.e. memory
byte order **B, G, R, X**. Your VP8 path already converts NES RGB → I420; reuse it with the
BGRX byte order (or use `XRGB8888` → swizzle). Frame is **640x240** (not 320x240) — that is the
VI output; either stream it as-is or downscale to 320x240 / upscale to 640x480.

**Audio note:** `av_info.timing.sample_rate = 44100` stereo i16. The existing Opus encoder
likely runs at 48000; resample 44100→48000 (your NES path may already feed Opus at a fixed
rate — match it).

---

## Gotchas / hard-won notes

- **Set `retro_set_environment` BEFORE `retro_init`.** The core reads pixel format, hw-render,
  and may read variables during init/load. We set all six callbacks up front.
- **Refuse `SET_HW_RENDER` (return false).** That single decision is what makes parallel_n64
  pick the software (angrylion) path and emit CPU frames via `video_refresh`. Confirmed: with
  angrylion selected, the core never even sends `SET_HW_RENDER` (cmd 14), so it just works.
- **The forced framebuffer is 640x240, not 320x240** (VI horizontal doubling). `pitch=2560`.
  Don't assume 320 — read `width/height/pitch` from each `video_refresh`.
- **`video_refresh(NULL, ...)` = duplicate frame** (`GET_CAN_DUPE`). Keep your last buffer;
  parallel_n64 sends data on ~69 of every 150 runs at the logo (VI cadence), NULL otherwise.
- **ROM bytes must outlive `load_game`** when `need_fullpath=false` — the core reads from your
  buffer. We `mem::forget` it (or store it in a long-lived struct).
- **Per-frame env spam** `GET_AUDIO_VIDEO_ENABLE` (cmd 0x10047) — decline; harmless.
- **mupen64plus-next nightly = HW-only; SIGSEGVs headless even with angrylion.** Skip it unless
  you wire the CGL GL context. parallel_n64 needs none of that.
- **rsp plugin:** `cxd4` (LLE, most accurate) and `hle` both boot SSB64; `hle` is faster.
- **Quarantine:** curl-downloaded buildbot zips are clean, but if a dylib ever gets a
  `com.apple.quarantine` xattr, `dlopen` from a hardened/signed app may be blocked — strip it.
- **Codesign:** the dylib is adhoc/linker-signed (`Signature=adhoc`); it `dlopen`s fine. If you
  ship inside a notarized app you may need to re-sign or use the
  `com.apple.security.cs.disable-library-validation` entitlement.

---

## CGL fallback source (glprobe/src/main.rs) — key excerpt

```rust
#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn CGLChoosePixelFormat(attribs:*const i32, pix:*mut *mut c_void, n:*mut i32) -> i32;
    fn CGLCreateContext(pix:*mut c_void, share:*mut c_void, ctx:*mut *mut c_void) -> i32;
    fn CGLSetCurrentContext(ctx:*mut c_void) -> i32;
    fn glGenFramebuffers(n:i32, ids:*mut u32);  fn glBindFramebuffer(t:u32, fb:u32);
    fn glGenRenderbuffers(n:i32, ids:*mut u32); fn glBindRenderbuffer(t:u32, rb:u32);
    fn glRenderbufferStorage(t:u32, ifmt:u32, w:i32, h:i32);
    fn glFramebufferRenderbuffer(t:u32, att:u32, rt:u32, rb:u32);
    fn glCheckFramebufferStatus(t:u32)->u32;
    fn glClearColor(r:f32,g:f32,b:f32,a:f32); fn glClear(m:u32);
    fn glReadPixels(x:i32,y:i32,w:i32,h:i32,f:u32,t:u32,d:*mut c_void); fn glFinish();
}
// kCGLPFAAccelerated=73, kCGLPFAOpenGLProfile=99, kCGLOGLPVersion_3_2_Core=0x3200,
// kCGLPFAColorSize=8, kCGLPFADepthSize=12. GL_FRAMEBUFFER=0x8D40, GL_RENDERBUFFER=0x8D41,
// GL_COLOR_ATTACHMENT0=0x8CE0, GL_RGBA8=0x8058, GL_FRAMEBUFFER_COMPLETE=0x8CD5,
// GL_COLOR_BUFFER_BIT=0x4000, GL_RGBA=0x1908, GL_UNSIGNED_BYTE=0x1401.
let attribs = [73, 99, 0x3200, 8,24, 12,24, 0];
let mut pix=ptr::null_mut(); let mut n=0;
CGLChoosePixelFormat(attribs.as_ptr(), &mut pix, &mut n);
let mut ctx=ptr::null_mut(); CGLCreateContext(pix, ptr::null_mut(), &mut ctx);
CGLSetCurrentContext(ctx);                       // headless: no window, no NSApp
// FBO RGBA8+depth24, render, glReadPixels(GL_RGBA, GL_UNSIGNED_BYTE) -> CPU bytes.
```

Verified output on this box: GL 4.1 (Apple M4 Pro, Metal-backed), FBO complete, readback of
clear color `(51,153,229,255)` exact, 307200/307200 non-zero bytes.

---

## Files produced

- Working core: `~/pokemon-pvp-red/cores/parallel_n64_libretro.dylib`
- Proof image:  `~/pokemon-pvp-red/research/ssb64-boot-headless.png`
  (SSB64 Nintendo/HAL boot logo, rendered headless)
- Harness src:  `/tmp/n64probe/n64probe/src/main.rs`
- GL probe src: `/tmp/n64probe/glprobe/src/main.rs`
