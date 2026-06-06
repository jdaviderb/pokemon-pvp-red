# nes-web — NES server-side emulation over WebRTC

Emulación NES **en el servidor**, streaming **VP8 vídeo + Opus audio** al navegador por
**WebRTC**, con **input de teclado** de vuelta por un *data channel*. Abre
`http://localhost:3000`, pulsa **Connect** y ves el juego corriendo.

```
Navegador  ──POST /offer (SDP)──►  axum :3000
   <video> ◄═══ VP8 + Opus RTP ═══  ┌─────────────────────────────────────────┐
   teclado ═══ DataChannel ════════►│ hilo emulador @60fps (tetanes-core)      │
                                     │   clock_frame → RGBA 256x240 + f32 48k   │
                                     │   RGBA→I420→VP8 (libvpx) ─┐              │
                                     │   f32→i16→Opus (libopus) ─┤ broadcast →  │
                                     └───────────────────────────┴──per-peer tracks
```

## Requisitos (macOS, ya presentes en esta máquina)

- Rust ≥ 1.85 (`rustc 1.86`).
- `libvpx` y `libopus` de homebrew en `/opt/homebrew` (`brew install libvpx opus`).
- `pkg-config`, `cmake`, libclang (Xpath de Xcode CLT) — para bindgen de libvpx 1.16.

`.cargo/config.toml` ya exporta `PKG_CONFIG_PATH` y `LIBRARY_PATH` para que `cargo` encuentre las libs.

## Build & run

```sh
cargo run --release                 # ROM por defecto: .../nes-MK1/out/MK1.nes
cargo run --release -- /ruta/a/otro.nes   # cargar otra "room"
```

Luego abre **http://localhost:3000** en **Chrome** y pulsa **Connect**.

### Controles

**Player 1**

| Tecla            | Botón NES |
|------------------|-----------|
| ← ↑ ↓ →          | D-pad     |
| `Z`              | B         |
| `X`              | A         |
| `Enter`          | Start     |
| `Shift`          | Select    |

**Player 2**

| Tecla            | Botón NES |
|------------------|-----------|
| `W` `A` `S` `D`  | D-pad     |
| `J`              | B         |
| `K`              | A         |
| `O`              | Select    |
| `P`              | Start     |

## Estructura

| Fichero            | Rol |
|--------------------|-----|
| `src/emu.rs`       | wrapper tetanes-core (carga ROM + fix header NES 2.0, frame/audio/input) |
| `src/video.rs`     | RGBA→I420 + encoder VP8 realtime (vpx-encode) |
| `src/audio.rs`     | f32→i16 + encoder Opus (paquetes de 960 samples / 20 ms) |
| `src/pipeline.rs`  | hilo maestro @60fps: clock → encode → broadcast; aplica input |
| `src/webrtc.rs`    | PeerConnection, tracks, drenaje RTCP, data channel, señalización |
| `src/signaling.rs` | router axum + `POST /offer` |
| `src/main.rs`      | arranque |
| `static/index.html`| cliente navegador |
| `DESIGN.md`        | diseño completo verificado + riesgos |

## Notas técnicas (no obvias)

- **MK1.nes es NES 2.0** con `byte10=0x70` → tetanes-core 0.14.1 hace `64<<112` y falla al
  cargar. `emu.rs::sanitize_nes2_ram_header` pone a cero los bytes 10/11 antes de cargar.
- **libvpx 1.16** requiere `vpx-encode` con feature `ffi-generate` (bindgen en build-time);
  sin ella la build entra en pánico (`vpx-ffi-1.16.0.rs not found`).
- **Keyframe al conectar**: como vpx-encode 0.6.2 no expone *force keyframe*, al pasar el peer
  a `Connected` se resetea el encoder (`make_vp8_encoder`) → el siguiente frame es keyframe,
  así el navegador ve imagen limpia de inmediato.
- A/V sync lo lleva `Sample.duration` (vídeo 16.639 ms, audio 20 ms); el emulador es el reloj.
