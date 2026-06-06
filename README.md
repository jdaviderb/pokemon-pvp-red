# nes-web — server-side emulation over WebRTC

Emulación **en el servidor** vía cores **libretro**, streaming **VP8 vídeo + Opus audio** al
navegador por **WebRTC**, con **teclado** de vuelta por un *data channel*. Abre
`http://localhost:3000`, pulsa **POWER** y ves/juegas dentro de una **TV CRT** retro.

Por defecto corre **Pokémon Red Color** (Game Boy Color, a color). La ROM original en gris
(`Pokemon Red.gb`, Game Boy clásico) y juegos **N64** (Super Smash Bros. / Pokémon Stadium, con un
`.z64` + core N64) corren en el mismo binario pasándolos por argv. La emulación pasa **100% en el
servidor** — el navegador solo recibe el stream.

> El crate se llama `nes-web` por historia: NES (tetanes-core) → N64 (libretro) → ahora también
> Game Boy/GBC (libretro). El frontend libretro (`src/n64.rs`) carga **cualquier** core.

```
Navegador  ──POST /offer (SDP)──►  axum :3000
   <video> ◄═══ VP8 + Opus RTP ═══  ┌──────────────────────────────────────────────┐
   teclado ═══ DataChannel ════════►│ hilo emulador @core-fps (libretro core)       │
                                     │   retro_run() → framebuffer + i16 stereo      │
                                     │   (XRGB8888 ó RGB565) → I420 → VP8 (libvpx) ─┐ │
                                     │   i16 → 48k → Opus (libopus) ───────────────┤ │
                                     └──────────────────────────────────────────────┘ broadcast → per-peer tracks
```

Los cores corren **headless** y por **software** (Game Boy es software puro; en N64 se fuerza el
RDP **angrylion** rechazando `SET_HW_RENDER`) → entregan framebuffers de CPU por `video_refresh`,
sin ventana ni GPU.

## Requisitos (macOS arm64)

- Toolchain **Rust 1.92** (fijado en `rust-toolchain.toml`; webrtc 0.17 lo exige).
- `libvpx` y `libopus` de homebrew en `/opt/homebrew` (`.cargo/config.toml` los expone).
- `clang` (para `build.rs` → `logshim.c`).
- **Cores libretro** (arm64) en `cores/`. Si no están:
  ```sh
  ./cores/fetch.sh   # gambatte + sameboy (GB/GBC) + parallel_n64 + mupen64plus_next (N64)
  ```

## Build & run

```sh
cargo run --release                 # Pokémon Red Color (.gbc, a color) por defecto
# Pokémon Red original en gris (Game Boy clásico):
cargo run --release -- "~/pokemon-pvp-red/Pokemon Red.gb"
# otra ROM / otro core (arg1 = ROM, arg2 = core dylib):
cargo run --release -- "/ruta/juego.gbc" cores/sameboy_libretro.dylib
# N64 (mismo binario):
cargo run --release -- "/ruta/juego.z64" cores/parallel_n64_libretro.dylib
```
Luego abre **http://localhost:3000** en **Chrome** y pulsa **POWER**.

> El `.gbc` se genera aplicando un parche IPS a `Pokemon Red.gb`:
> `python3 scripts/apply_ips.py "Pokemon Red.gb" pokered_color/pokered_color_vanilla.ips "Pokemon Red Color.gbc"`
> (header 0x143 pasa a 0xC0 = modo GBC → color).

### Controles

**Game Boy / GBC** (default)

| Tecla | GB |
|---|---|
| `← ↑ ↓ →` | D-pad |
| `X` | A |
| `Z` | B |
| `Enter` | Start |
| `⇧ Right` / `⌫` | Select |

**N64** (al cargar un `.z64`): `←↑↓→` stick · `X`=A · `Z`=B · `C`=Z · `Q`/`E`=L/R · `Enter`=Start ·
`I J K L`=C-buttons. (El cliente web manda nombres de botón; el servidor mapea según el juego.)

## 🎮 AI Battle Arena (Pokémon Red)

Un módulo experimental expone la **batalla de Pokémon Red como un entorno para agentes de IA**
sobre HTTP. La emulación corre el motor de batalla **real** (no una reimplementación); el agente
lee el estado de WRAM y elige movimientos, que se ejecutan navegando el menú por *input* (no se
hackea el engine).

Arranca con la ROM con la que se capturó el savestate:
```sh
cargo run --release -- "Pokemon Red.gb"
```

Endpoints (mismo origen, `:3000`):

| Método | Ruta | Qué hace |
|---|---|---|
| `POST` | `/battle/load` | restaura `states/battle.state` (cuerpo vacío) o un blob crudo (`--data-binary @file`) |
| `GET`  | `/battle/state` | JSON `BattleState` (in_battle, turnos, player/enemy {hp,lvl,moves,pp,stats}, menú) |
| `POST` | `/battle/action` | `{"type":"move","slot":0..3}` · `{"type":"switch","slot":N}` · `{"type":"run"}` · `{"type":"buttons","presses":["A",...]}` |
| `POST` | `/battle/save` | serializa el estado actual → devuelve el blob y escribe `states/battle.state` |
| `GET`  | `/battle/species` | lista de especies elegibles `[[idx,"NOMBRE"],…]` |
| `POST` | `/battle/setup` | **monta un combate a elección**: `{"player":74,"enemy":75,"level":50}` (índices internos Gen-1) |

**Combate de leyenda (elige los Pokémon):** `/battle/setup` carga un savestate de intro
(`states/legendary_intro.state`) e inyecta el equipo de ambos lados, así el motor saca a los
elegidos **con sus sprites/nombres/cries reales**. Disponibles: **Articuno (74), Zapdos (75),
Moltres (73), Dragonite (66)** — extiende la tabla `SPECIES` en `src/battle.rs` para añadir más.
Desde la UI: fila **MATCHUP** (dropdowns + Lv + *Start Matchup*). Stats Lv50 calculados con la
fórmula Gen-1 (DV=15). **Requiere arrancar con `Pokemon Red.gb`** (el savestate es de esa ROM).
```sh
curl -s localhost:3000/battle/species
curl -XPOST localhost:3000/battle/setup -H 'content-type: application/json' -d '{"player":74,"enemy":75,"level":50}'
```

Bucle del agente:
```sh
curl -X POST localhost:3000/battle/load                 # bootstrap a una batalla
curl -s localhost:3000/battle/state | jq                # leer estado
curl -X POST localhost:3000/battle/action -H 'content-type: application/json' \
     -d '{"type":"move","slot":0}'                       # elegir movimiento -> se ejecuta
# repetir: leer estado; si un texto de resultado espera, avanzar con
#          {"type":"buttons","presses":["A"]} hasta que turns_in_battle suba y menu vuelva a MainMenu
```
Win/loss: `enemy.hp==0` (ganas) / `player.hp==0` (pierdes); batalla terminada cuando `in_battle==0`.

**Notas:** el savestate de batalla (`states/battle.state`) es **específico de la ROM** (capturado en
`Pokemon Red.gb`); regenéralo jugando hasta el menú FIGHT y `POST /battle/save`. HP/stats de Gen-1 son
**big-endian**. Detalles + RAM map en `DESIGN-BATTLE.md` y `docs/pokemon-red-ram-map.md`.

## Estructura

| Fichero | Rol |
|---|---|
| `src/n64.rs` | frontend **libretro** genérico (dlopen del core, callbacks, opciones, input) |
| `src/video.rs` | `frame_to_i420`: **XRGB8888 (BGRX)** ó **RGB565** → I420 + encoder VP8 |
| `src/audio.rs` | i16 estéreo → resample `core_rate`→48000 → Opus estéreo (960/canal) |
| `src/pipeline.rs` | hilo maestro @core-fps: retro_run → encode → broadcast; re-init en cambio de resolución |
| `src/webrtc.rs` | PeerConnection, tracks (Opus estéreo), RTCP, data channel, señalización |
| `src/battle.rs` | battle-arena: `BattleState` reader (WRAM, big-endian), `AgentAction`, tap-macro del menú |
| `src/signaling.rs` | router axum + `POST /offer` + `/battle/{state,action,save,load}` |
| `src/main.rs` | arranque (ROM + core path por argv) |
| `logshim.c` / `build.rs` | shim C-variádico para `GET_LOG_INTERFACE` (lo necesita mupen-next) |
| `scripts/apply_ips.py` | aplicar parches IPS (genera el `.gbc` de Pokémon Red Color) |
| `cores/` | dylibs libretro (`fetch.sh` los baja; ignorados por git) |
| `static/index.html` | cliente navegador (TV CRT + teclado) |
| `DESIGN-N64.md`, `DESIGN-GB.md`, `DESIGN-BATTLE.md` | diseños verificados + riesgos |
| `docs/pokemon-red-ram-map.md` | RAM map de batalla (direcciones + endianness) |
| `test/e2e-*.cjs` | pruebas headless (Chrome/Puppeteer) |
| `research/` | notas de investigación + capturas de prueba |

## Notas técnicas (no obvias)

- **Formato de píxel por core**: `video_refresh` registra `SET_PIXEL_FORMAT`. `frame_to_i420`
  ramifica: **RGB565** (gambatte/mGBA, 2 B/px, *pitch padded* — hay que respetar `pitch`, no `w*2`)
  vs **XRGB8888 BGRX** (N64/angrylion, SameBoy).
- **Headless sin GL (N64)**: se rechaza `SET_HW_RENDER` → angrylion software. Game Boy es software
  puro y nunca lo pide.
- **Dimensiones**: GB = 160×144 fijo; N64 = 640×240 ↔ 640×480 (cambia entre título y menús). El
  `pipeline` **re-inicializa el encoder VP8** al cambiar de tamaño (un encoder nuevo emite keyframe;
  el navegador se adapta; el CSS estira a 4:3).
- **Audio**: el resampler toma el `sample_rate` que reporte el core (GB gambatte = 32768 Hz; N64 =
  44100). gambatte es upsample suave a 48k; SameBoy (2.097 MHz) aliasea con el resampler lineal de
  2 taps — por eso el default GB es gambatte.
- **DMG vs GBC** lo decide el header del ROM (0x143: `.gb`=0x00 gris, `.gbc`=0xC0 color), no un flag.
- **`GET_LOG_INTERFACE`**: mupen64plus-next SIGSEGV sin un puntero de log real → lo da `logshim.c`.
- **Keyframe al conectar** y **limpieza de tasks por peer** del diseño original.
