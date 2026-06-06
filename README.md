# nes-web — server-side **N64** emulation over WebRTC

Emulación **N64 en el servidor** (Super Smash Bros. 64), streaming **VP8 vídeo + Opus audio
estéreo** al navegador por **WebRTC**, con **teclado** de vuelta por un *data channel*. Abre
`http://localhost:3000`, pulsa **Connect** y ves/juegas el juego corriendo en el servidor.

> El nombre del crate sigue siendo `nes-web` por historia: empezó como NES (tetanes-core) y
> evolucionó a N64. La emulación pasa **100% en el servidor** — el navegador solo recibe el stream.

```
Navegador  ──POST /offer (SDP)──►  axum :3000
   <video> ◄═══ VP8 + Opus RTP ═══  ┌──────────────────────────────────────────────┐
   teclado ═══ DataChannel ════════►│ hilo emulador @60fps (libretro N64 core)     │
                                     │   retro_run() → XRGB8888 640x240 + i16 stereo │
                                     │   XRGB→I420→VP8 (libvpx) ─┐                   │
                                     │   i16→48k→Opus (libopus) ─┤ broadcast →       │
                                     └───────────────────────────┴──per-peer tracks  │
```

El core N64 (libretro) corre **headless** con el plugin **angrylion (RDP por software)**: sin
ventana, sin GPU, sin contexto GL. El truco clave es **rechazar `SET_HW_RENDER`** para que el
core entregue framebuffers de CPU por `video_refresh`.

## Requisitos (macOS arm64)

- Toolchain **Rust 1.92** (fijado en `rust-toolchain.toml`; webrtc 0.17 lo exige).
- `libvpx` y `libopus` de homebrew en `/opt/homebrew` (`.cargo/config.toml` los expone).
- `clang` (para `build.rs` → `logshim.c`).
- **Un core N64 libretro** en `cores/` (arm64). Si no están, descárgalos:
  ```sh
  ./cores/fetch.sh        # baja parallel_n64 + mupen64plus_next del libretro buildbot
  ```

## Build & run

```sh
cargo run --release                       # SSB64 + parallel_n64 por defecto
cargo run --release -- "/ruta/otro.z64"   # otra ROM
cargo run --release -- "/ruta/otro.z64" cores/mupen64plus_next_libretro.dylib   # otro core
```
Luego abre **http://localhost:3000** en **Chrome** y pulsa **Connect**.

### Controles (Player 1)

| Tecla | N64 |
|---|---|
| `← ↑ ↓ →` | Control stick (analógico) |
| `X` | A |
| `Z` | B |
| `C` | Z (gatillo) |
| `Q` / `E` | L / R |
| `Enter` | Start |
| `I J K L` | C-buttons (arriba/izq/abajo/der) |

## Estructura

| Fichero | Rol |
|---|---|
| `src/n64.rs` | frontend **libretro** headless (dlopen del core, callbacks, angrylion software, input N64) |
| `src/video.rs` | XRGB8888(BGRX)→I420 + encoder VP8 (canvas dinámico desde el 1er frame) |
| `src/audio.rs` | i16 estéreo → resample 44100→48000 → Opus estéreo (paquetes 960/canal) |
| `src/pipeline.rs` | hilo maestro @core-fps: retro_run → encode → broadcast; input N64 |
| `src/webrtc.rs` | PeerConnection, tracks (Opus estéreo), RTCP, data channel, señalización |
| `src/signaling.rs` | router axum + `POST /offer` |
| `src/main.rs` | arranque (ROM + core path) |
| `logshim.c` / `build.rs` | shim C-variádico para `GET_LOG_INTERFACE` (lo necesita mupen-next) |
| `cores/` | dylibs libretro (`fetch.sh` los baja; ignorados por git) |
| `static/index.html` | cliente navegador (teclado N64) |
| `DESIGN-N64.md` | diseño completo verificado + riesgos |
| `test/e2e-n64-*.cjs` | pruebas headless (Chrome/Puppeteer): media + input |
| `research/` | notas de investigación + capturas de prueba |

## Notas técnicas (no obvias)

- **Headless sin GL**: `src/n64.rs` rechaza `RETRO_ENVIRONMENT_SET_HW_RENDER` → el core usa
  angrylion (software) y entrega XRGB8888 por `video_refresh`. Cero dependencia de pantalla/GPU.
- **`GET_LOG_INTERFACE`**: mupen64plus-next llama un puntero de log variádico en `retro_load_game`;
  si no se le da uno real, SIGSEGV. `logshim.c` (compilado por `build.rs`) lo provee. parallel_n64
  no lo necesita pero es inofensivo.
- **Vídeo 640×240**: angrylion duplica líneas (320→640). El canvas VP8 se fija al **primer frame
  real** y los cambios de tamaño se letterboxean (VP8 no puede redimensionar en caliente).
- **Audio estéreo 44100→48000**: el core saca i16 estéreo @44100; se resamplea (lineal) a 48000
  para Opus estéreo (960 samples/canal por paquete de 20 ms).
- **ROM `.z64`** es big-endian nativo → sin byteswap. SMW/`.smc` (SNES) llevarían header copier;
  N64 `.z64` no.
- **Keyframe al conectar** y **limpieza de tasks por peer** se mantienen del diseño NES.

## Rendimiento

Medido headless en Apple Silicon: parallel_n64 ~1400 fps (~23× tiempo real), mupen64plus-next
~380 fps (~6×), ambos con angrylion multihilo. 60 fps + encode VP8/Opus caben con holgura. En un
combate de 4 jugadores cargado, si baja, el `pipeline` ya resincroniza el reloj (salta encode,
no emulación).
