# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`nes-web` runs a **console emulator entirely server-side** and streams the live game to a browser
over **WebRTC** (VP8 video + Opus audio), with **keyboard input** sent back over a WebRTC data
channel. Open `http://localhost:3000`, click **Connect**, watch/play.

The crate name is historical: NES (tetanes-core) → N64 (libretro) → now also **Game Boy / GBC**
(libretro). The libretro frontend (`src/n64.rs`) loads ANY core. Emulation is server-only — the
browser only receives the stream.

> **Full docs:** `docs/ARCHITECTURE.md` (complete project guide: lineage, components, cores, HTTP
> API, build, extending), `docs/battle-arena.md` (the AI battle arena + matchup), and
> `docs/multiplayer.md` (the **2-player online arena**: DB/auth/rooms/WebSocket, flow, run/play).
> Verified design records: `DESIGN.md` (NES), `DESIGN-N64.md`, `DESIGN-GB.md`, `DESIGN-BATTLE.md`,
> `DESIGN-LEGENDARY.md`, `DESIGN-MULTIPLAYER.md`. Gen-1 RAM map: `docs/pokemon-red-ram-map.md`.

There is also a **2-player online game** on top of the arena (register/login → Find Match → room →
slot-machine random Pokémon → 15s/turn battle → winner). It REUSES the single-emulator engine: P1 =
YOU side (`action_tx`), P2 = enemy (`enemy_force`/CCDD), slot roll → `setup_tx`. DB is SeaORM (SQLite
auto-created on boot; `DATABASE_URL` → Postgres, no code change). See `src/{db,auth,rooms,ws}.rs`,
`src/migrations`, `src/entities`, `static/{login,lobby,room,console}.html`, `docs/multiplayer.md`.
Run it with `Pokemon Red.gb` (savestate-specific); register needs username ≥3 / password ≥6; play
needs two sessions (normal + incognito). One emulator ⇒ one concurrent battle (extras queue).

Default ROM/core: `Pokemon Red.gb` + `cores/gambatte_libretro.dylib`. It renders in **color** via
gambatte's **GBC auto-colorization** (`gambatte_gb_colorization=auto`, forced in `n64.rs`) — the same
palette a real Game Boy Color applied to a DMG cart — which is RENDER-ONLY, so its `.gb` savestates
still power the battle arena + multiplayer. Override via argv: `cargo run --release -- "<rom>" "<core.dylib>"`:
- native color romhack: `cargo run --release -- "Pokemon Red Color.gbc"` (= `Pokemon Red.gb` + the
  `pokered_color/pokered_color_vanilla.ips` patch, header 0x143=0xC0; see `scripts/apply_ips.py`).
  NOTE: the battle savestates are ROM-specific to `Pokemon Red.gb`, so `/battle/*` + multiplayer
  need the `.gb` (now also color), not the `.gbc`.
- N64: `cargo run --release -- "<rom>.z64" cores/parallel_n64_libretro.dylib` (RSP env-selectable:
  `N64_RSP=hle` faster vs default `cxd4` accurate LLE).

## Build & run

```sh
cargo run --release            # env + toolchain come from .cargo/config.toml + rust-toolchain.toml
cargo build --release
./cores/fetch.sh               # (re)download the libretro N64 cores if cores/*.dylib are missing
```
Then open **http://localhost:3000** in **Chrome** and click **Connect**.
Controls (Game Boy, P1): arrows = D-pad, `X`=A, `Z`=B, `Enter`=Start, `⇧Right`/`⌫`=Select.
(N64: arrows=stick, `X`=A `Z`=B `C`=Z `Q`/`E`=L/R `Enter`=Start `IJKL`=C-buttons.)

### Build prerequisites (satisfied on this machine)

- **Toolchain pinned to Rust 1.92** via `rust-toolchain.toml` — do NOT remove it (webrtc 0.17.x
  needs ≥1.87; the global default is 1.86).
- homebrew `libvpx` + `libopus` under `/opt/homebrew`; `.cargo/config.toml` exports
  `PKG_CONFIG_PATH`/`LIBRARY_PATH` so the `vpx-encode`/`opus` sys-crates link.
- `clang` for `build.rs` (compiles `logshim.c`).
- A libretro N64 core dylib in `cores/` (arm64). `cores/*.dylib` is gitignored; `cores/fetch.sh`
  pulls them from the libretro buildbot.

## Architecture

```
emulator thread (one dedicated OS thread, core-fps ~60.13)      WebRTC (per browser peer, tokio)
  retro_run() -> XRGB8888 640x240 + i16 stereo @44100              VP8 track <- video broadcast
  XRGB(BGRX)->I420->VP8  --video_tx (broadcast)-->                 Opus stereo track <- audio
  i16 -> resample 48k -> stereo Opus  --audio_tx-->                write_sample(Sample{data,duration})
        ^ input_rx (mpsc)  <-- DataChannel "input" <-- browser keydown/keyup
axum :3000 serves static/index.html + POST /offer (non-trickle SDP exchange)
```

- **Master clock = the emulator thread** (`src/pipeline.rs::run_loop`), drift-paced to the core's
  reported fps. The libretro core + both encoders live only on that thread (the core also spawns
  angrylion worker threads that hit the same global buffers).
- libretro callbacks are bare `extern "C" fn` with no user-data, so per-instance buffers are
  process globals (`static Mutex<FRAME/AUDIO/PAD>` in `src/n64.rs`). One emulator, one thread.
- Encoded media is fanned out via `tokio::sync::broadcast`; each peer's writer task subscribes.

### File map

| File | Role |
|---|---|
| `src/n64.rs` | libretro frontend: dlopen core, 6 callbacks, force angrylion software, load .z64, input |
| `src/video.rs` | XRGB8888(BGRX)→I420 (`xrgb_to_i420`) + VP8 encoder; canvas sized from 1st frame |
| `src/audio.rs` | i16 stereo → 44100→48000 linear resample → stereo Opus 960-frame packets |
| `src/pipeline.rs` | the core-fps loop; broadcast channels; `AppInner`; N64 input; stats |
| `src/webrtc.rs` | per-peer PeerConnection, tracks (Opus stereo cap), RTCP drain, data channel, signaling, cleanup |
| `src/battle.rs` | Pokémon Red battle arena: `BattleState`/`BattlePokemon`/`AgentAction`, `read_battle_state` (WRAM, BIG-ENDIAN), inject_*, `TapMachine` (action→menu input) |
| `src/signaling.rs` | axum `Router` + `AppState`; `/offer`, `/battle/*`, `/auth/*`, `/api/{me,species}`, `/ws` |
| `src/db.rs` · `src/migrations/` · `src/entities/` | SeaORM: connect + create-if-missing + migrate; models (users/sessions/rooms/matches/user_room) |
| `src/auth.rs` | argon2id, register/login/logout, session cookie, `AuthUser` extractor |
| `src/rooms.rs` | multiplayer: matchmaking, room FSM, turn-based battle engine (15s timer, CPU, winner, resume) |
| `src/ws.rs` | per-client WebSocket (`WsHub`, JSON event protocol) |
| `src/main.rs` | entry: ROM + core paths, start pipeline, connect DB, spawn matchmaker, serve axum |
| `logshim.c` + `build.rs` | C-variadic log fn for `GET_LOG_INTERFACE` (mupen-next needs it) |
| `scripts/apply_ips.py` | IPS patcher (makes `Pokemon Red Color.gbc`) |
| `static/{login,lobby,room}.html` | multiplayer UI; `console.html` = single-player dev console (was `index.html`); `index.html` = `/api/me` router |
| `static/sprites/` | 151 Gen-1 front sprites by National Dex number (slot machine) |
| `cores/` | libretro core dylibs (`fetch.sh`; gitignored). `states/` = savestates, `data.db` = sqlite (gitignored) |
| `docs/` | `ARCHITECTURE.md`, `multiplayer.md`, `battle-arena.md`, `pokemon-red-ram-map.md` |
| `DESIGN*.md` | verified design records (NES, N64, GB, BATTLE, LEGENDARY, MULTIPLAYER) |
| `research/*.md`, `research/*.png` | grounded probe findings + proof screenshots |

## Non-obvious things — READ before editing these areas

- **Headless = refuse `SET_HW_RENDER`** (`src/n64.rs` env cmd 14 → return false). That keeps the
  core in angrylion software mode delivering CPU framebuffers via `video_refresh`. Accepting it
  would require an offscreen GL context. Don't change this without the CGL plan in DESIGN-N64 §10.
- **`GET_LOG_INTERFACE` (env cmd 27)** must return a REAL C-variadic fn pointer (`n64_core_log`
  from `logshim.c`). Declining it makes mupen64plus-next SIGSEGV in `retro_load_game`. Harmless
  for parallel_n64. `build.rs` links the shim; don't drop it.
- **Pixel format is per-core**: `frame_to_i420` (src/video.rs) branches on `Frame.fmt`. **XRGB8888**
  = memory bytes B,G,R,X (N64/angrylion, SameBoy). **RGB565** = little-endian u16 (gambatte/mGBA),
  R5/G6/B5. ALWAYS stride rows by the callback's real `pitch` (gambatte pads 160px→256px = 512 B;
  never `width*2` or `width*4`).
- **VP8 canvas is fixed at the first frame's dims** (640×240 for SSB64; angrylion line-doubles
  320→640). Frame dims can change (interlace/menus); `xrgb_to_i420` letterboxes onto the fixed
  canvas because VP8 can't resize mid-stream.
- **Audio is i16 stereo @ the core's `sample_rate` (44100)**, linear-resampled to 48000 for
  stereo Opus; never pad/truncate, the resampler carries fractional position across calls.
- **ROM lifetime**: `need_fullpath==false`, so the core reads our ROM bytes after `load_game`;
  `N64::new` `mem::forget`s the 16 MiB buffer to keep it alive. Don't "fix" that leak.
- **Forced core options** (`forced_option` in `n64.rs`) cover BOTH cores (parallel-n64-* and
  mupen64plus-*); a core only queries its own keys. To switch cores, just change the dylib path.
- **`.z64` is native big-endian** → no byteswap. Keep `webrtc.rs` referring to the crate as
  `::webrtc` (our module shadows it).
- **AI battle arena** (`src/battle.rs`, Pokémon Red): WRAM = `RETRO_MEMORY_SYSTEM_RAM` (id 2, 8 KiB,
  CPU `addr-0xC000`); HRAM (FFF3) NOT exposed. Gen-1 HP/stats are **BIG-ENDIAN** (`from_be_bytes`) —
  #1 bug, pinned by a unit test. Battles bootstrap from a savestate (`states/battle.state`,
  ROM-specific to `Pokemon Red.gb`) captured at the **FIGHT menu** (a few text-boxes AFTER `D057`
  goes nonzero — capturing too early makes the action macro off-by-one). Agent moves run via the
  `TapMachine` input macro (A→FIGHT, Down×slot, A) on `emu.set_button` (same PAD path as the browser);
  status-move/result text may wait for an A — advance with `{"type":"buttons","presses":["A"]}`. CCDD
  enemy-move override works only in a post-AI-pick window (poll the 0→nonzero transition). `states/`
  is gitignored. Run on `Pokemon Red.gb` (the .gbc savestate would differ).
- **Custom matchup** (`POST /battle/setup`, `src/battle.rs::setup_matchup`): loads
  `states/legendary_intro.state` (pre-send-out: D057!=0, D014==0, CFE5==0) and injects full 44-byte
  party structs for BOTH sides (species internal index, Lv-scaled BE stats via the Gen-1 formula
  DV=15, moves/PP, and the player NICKNAME at D2B5 — mandatory or the on-screen name is stale), then
  taps A through send-out so the engine draws the real sprites/names/cries. Species table `SPECIES`
  (Articuno 0x4A, Zapdos 0x4B, Moltres 0x49, Dragonite 0x42 — INTERNAL indices, not Pokédex).
  Verified: Articuno vs Zapdos renders with correct sprites + Lv50 HP 165.

## Testing

End-to-end is verified with headless Chrome (Puppeteer) — see `test/e2e-n64-*.cjs` and
`test/README.md`. They confirm 640×240 VP8 decode, stereo Opus, and that keyboard input reaches
the N64 core (before/after screenshots differ). Liveness signal in the server log:
`n64: ~60.0 fps | N video pkts | M audio pkts | viewers v=.. a=..` every 5 s.

## Conventions

- Edition 2021, `rust-version = "1.92"`. `tracing` for logs (`RUST_LOG=nes_web=info,webrtc=warn`).
- When bumping `webrtc`, re-check the API against the pinned version's source (master is
  restructured toward a different release and is misleading).
