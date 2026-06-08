# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

**Pokémon Red PVP** (crate `pokemon-red-pvp`) runs **Pokémon Red entirely server-side** (gambatte
libretro core, Game Boy / GBC) and streams the live game to a browser over **WebRTC** (VP8 video +
Opus audio), with **keyboard input** sent back over a WebRTC data channel. On top of that it adds the
PvP arena: matchmaking, 2-player battles, ranking, a collection, a live TV wall, and an **MCP server**
so AI agents can play. Open `http://localhost:3000`.

The libretro frontend (`src/libretro.rs`) is **core-agnostic** — it can load any core (the project
grew out of an NES→N64→GB streaming experiment, and that general capability is still here and reusable)
— but the product is Pokémon Red PVP. Emulation is server-only; the browser only receives the stream.

> **Docs:** see **`DOCS.md`** (the index). Highlights: `docs/ARCHITECTURE.md` (full project guide),
> `docs/battle-arena.md` (the AI battle arena), `docs/multiplayer.md` (the 2-player online arena),
> `docs/mcp.md` (AI agents), `docs/SCALING.md`, `docs/pokemon-red-ram-map.md` (Gen-1 WRAM map).
> Verified design records: `DESIGN-GB.md`, `DESIGN-BATTLE.md`, `DESIGN-MULTIPLAYER.md`.

There is also a **2-player online game** on top of the arena (register/login → Find Match → room →
slot-machine random Pokémon → 15s/turn battle → winner). It REUSES the single-emulator engine: P1 =
YOU side (`action_tx`), P2 = enemy (`enemy_force`/CCDD), slot roll → `setup_tx`. DB is SeaORM (SQLite
auto-created on boot; `DATABASE_URL` → Postgres, no code change). See `src/{db,auth,rooms,ws}.rs`,
`src/migrations`, `src/entities`, `static/{login,lobby,room,console}.html`, `docs/multiplayer.md`.
Run it with `Pokemon Red.gb` (savestate-specific); register needs username ≥3 / password ≥6; play
needs two sessions (normal + incognito). One emulator ⇒ one concurrent battle (extras queue).

Default ROM/core: `Pokemon Red.gb` + `cores/gambatte_libretro.dylib`. It renders in **color** via
gambatte's **GBC auto-colorization** (`gambatte_gb_colorization=auto`, forced in `libretro.rs`) — the same
palette a real Game Boy Color applied to a DMG cart — which is RENDER-ONLY, so its `.gb` savestates
still power the battle arena + multiplayer. Override via argv: `cargo run --release -- "<rom>" "<core.dylib>"`:
- native color romhack: `cargo run --release -- "Pokemon Red Color.gbc"` (= `Pokemon Red.gb` + the
  `pokered_color/pokered_color_vanilla.ips` patch, header 0x143=0xC0; see `scripts/apply_ips.py`).
  NOTE: the battle savestates are ROM-specific to `Pokemon Red.gb`, so `/battle/*` + multiplayer
  need the `.gb` (now also color), not the `.gbc`.

## Build & run

```sh
cargo run --release                          # DEFAULT = COORDINATOR: spawns an emulator worker per battle (N concurrent rooms)
MAX_WORKERS=8 cargo run --release            # ...capped at 8 concurrent battles (else unbounded)
cargo run --release -- --solo                # SOLO: single process, one emulator, one battle at a time
cargo build --release
./cores/fetch.sh               # (re)download the libretro cores if cores/*.dylib are missing

./build-docker-production.sh                          # build the linux/amd64 image (pokemon-red-pvp:prod)
docker run --rm --network host -e DEV=1 pokemon-red-pvp:prod   # then open http://localhost:3000
```
> **Docker gotcha (verified):** run the container with **`--network host`**, NOT `-p`. WebRTC
> advertises the server's ICE candidates; under Docker bridge networking those are the container's
> internal `127.0.0.1`/`172.x` → the UI loads but **no video reaches the browser**. Host networking
> makes them reachable (TV video flows). `-e DEV=1` lets guest agents mint MCP tokens (prod: real
> accounts). For a public deploy without host-net, use `set_nat_1to1_ips(<public IP>)` in the WebRTC
> SettingEngine + a published UDP range. See `docs/SCALING.md`.
> The default is now the scalable coordinator (so multiple battles — incl. agent battles via MCP —
> run at once). `--solo` is the old single-emulator mode. `--worker` is internal (spawned by the
> coordinator). `--mcp` runs the stdio MCP server (see `docs/mcp.md`).
Then open **http://localhost:3000** in **Chrome** and click **Connect**.

**Scaling (`src/coordinator.rs`):** one emulator = one battle (libretro globals are per-process), so
concurrency = worker PROCESSES. `--coordinator` runs no emulator; it owns auth/lobby/matchmaking and
**spawns an EPHEMERAL `--worker` process per battle ON DEMAND** (each = this binary, own emulator,
**shared DB** via `DATABASE_URL`); when the battle ends a reaper **KILLS the worker** to free its
CPU/RAM. It pairs players and **redirects** each match to its worker (`/room?id=` → 303 →
`worker:port`; WebRTC + battle WS go browser↔worker directly; localhost shares cookies across ports
so the session still authenticates against the shared DB). Concurrency cap = `MAX_WORKERS` env or
`--workers N`, **unbounded if neither is set**. Internal endpoints (`/internal/assign|status`) are
secret-gated (`INTERNAL_SECRET`). Use **Postgres** (`DATABASE_URL`) for real multi-process deploys —
sqlite write-contention across processes is fine for local dev only. Default (no flags) = `Coordinator` (a worker/emulator per battle → N concurrent rooms); `--solo` is the single-process fallback.
Controls (Game Boy, P1): arrows = D-pad, `X`=A, `Z`=B, `Enter`=Start, `⇧Right`/`⌫`=Select.

### Build prerequisites (satisfied on this machine)

- **Toolchain pinned to Rust 1.92** via `rust-toolchain.toml` — do NOT remove it (webrtc 0.17.x
  needs ≥1.87; the global default is 1.86).
- homebrew `libvpx` + `libopus` under `/opt/homebrew`; `.cargo/config.toml` exports
  `PKG_CONFIG_PATH`/`LIBRARY_PATH` so the `vpx-encode`/`opus` sys-crates link.
- `clang` for `build.rs` (compiles `logshim.c`).
- A libretro core dylib in `cores/` (arm64). `cores/*.dylib` is gitignored; `cores/fetch.sh`  pulls them from the libretro buildbot.

## Production deployment (context — lives in a separate repo)

This project runs in production at **https://pokemonpvp.red**, but the **deployment is NOT in this
repo** — it's GitOps (Flux) in a separate private repo that builds this `Dockerfile` and ships it to a
small k3s cluster behind Traefik (auto Let's Encrypt TLS, `www` → 301 → apex). Useful context only;
nothing here depends on it. Prod runs **without `DEV`**, so the dev-only `/battle/*`, `/console` and
`/debug/frame` routes are NOT mounted; secrets live in cluster Secrets (never committed).

## Architecture

```
emulator thread (one dedicated OS thread, gambatte core-fps ~59.7)   WebRTC (per browser peer, tokio)
  retro_run() -> RGB565 160x144 + i16 stereo @32768                    VP8 track <- video broadcast
  RGB565 -> I420 -> VP8  --video_tx (broadcast)-->                     Opus stereo track <- audio
  i16 -> resample 48k -> stereo Opus  --audio_tx-->                   write_sample(Sample{data,duration})
        ^ input_rx (mpsc)  <-- DataChannel "input" <-- browser keydown/keyup
axum :3000 serves static/index.html + POST /offer (non-trickle SDP exchange)
```

- **Master clock = the emulator thread** (`src/pipeline.rs::run_loop`), drift-paced to the core's
  reported fps (gambatte ~59.7). The libretro core + both encoders live only on that thread.
- libretro callbacks are bare `extern "C" fn` with no user-data, so per-instance buffers are
  process globals (`static Mutex<FRAME/AUDIO/PAD>` in `src/libretro.rs`). One emulator, one thread.
- Encoded media is fanned out via `tokio::sync::broadcast`; each peer's writer task subscribes.

### File map

| File | Role |
|---|---|
| `src/libretro.rs` | libretro frontend (core-agnostic): dlopen core, 6 callbacks, load ROM, input. The emulator handle is `Emu` |
| `src/mcp.rs` | MCP server (rmcp): remote streamable-HTTP at `/mcp` + stdio (`--mcp`) so AI agents play. See `docs/mcp.md` |
| `Dockerfile` · `build-docker-production.sh` | production linux/amd64 image (bundles ROM + Linux core + states); see `docs/SCALING.md` |
| `src/video.rs` | RGB565→I420 (gambatte) + VP8 encoder; canvas sized from the 1st frame |
| `src/audio.rs` | i16 stereo → core rate→48000 linear resample → stereo Opus packets |
| `src/pipeline.rs` | the core-fps loop; broadcast channels; `AppInner`; input; stats; per-frame HUD hook |
| `src/hud.rs` | composite onto the RGB565 frame before encode. Hides the in-battle FIGHT/ITEM menu (PvP injects moves, so it's noise) by painting it with the sampled bg. Toggle: env `HIDE_BATTLE_MENU=0` |
| `src/webrtc.rs` | per-peer PeerConnection, tracks (Opus stereo cap), RTCP drain, data channel, signaling, cleanup |
| `src/battle.rs` | Pokémon Red battle arena: `BattleState`/`BattlePokemon`/`AgentAction`, `read_battle_state` (WRAM, BIG-ENDIAN), inject_*, `TapMachine` (action→menu input) |
| `src/signaling.rs` | axum `Router` + `AppState`; `/offer`, `/battle/*`, `/auth/*`, `/api/{me,species}`, `/ws` |
| `src/db.rs` · `src/migrations/` · `src/entities/` | SeaORM: connect + create-if-missing + migrate; models (users/sessions/rooms/matches/user_room/oauth_accounts/feature_flags) |
| `src/oauth.rs` · `src/flags.rs` | provider-agnostic OAuth2 social login (Google; `/auth/oauth/{provider}`) · runtime feature flags (`login_username`, `guest_mode`) read live from DB |
| `src/auth.rs` | argon2id, register/login/logout, session cookie, `AuthUser` extractor |
| `src/rooms.rs` | multiplayer: matchmaking, room FSM, turn-based battle engine (15s timer, CPU, winner, resume) |
| `src/ws.rs` | per-client WebSocket (`WsHub`, JSON event protocol); auth via session cookie OR `?token=` (agents) |
| `src/ranking.rs` | leaderboard: background job (RANKING_REFRESH_SECS, default 300) → wins/Today/Weekly/Monthly cached in memory + `cache/ranking.json`; `/api/ranking` |
| `src/main.rs` | entry: parse flags (`--coordinator`/`--worker`/`--port`/`--workers`/`--mcp`), pick Role, serve axum |
| `src/coordinator.rs` | SCALABLE mode: spawn + manage the emulator worker pool, global matchmaking, `/internal/{assign,status}`, redirect players to their worker (`AppState.role` = Solo/Worker/Coordinator) |
| `logshim.c` + `build.rs` | C-variadic log shim for libretro `GET_LOG_INTERFACE` (legacy; required by some cores) |
| `scripts/apply_ips.py` | IPS patcher (makes `Pokemon Red Color.gbc`) |
| `static/{login,lobby,room}.html` | multiplayer UI; `index.html` = `/api/me` router. `dev/console.html` = single-player dev console, served at `/console` **only when env `DEV=1`** (so are the unauthenticated `/battle/*` endpoints) |
| `static/sprites/` | 151 Gen-1 front sprites by National Dex number (slot machine) |
| `cores/` | libretro core dylibs (`fetch.sh`; gitignored). `states/` = savestates, `data.db` = sqlite (gitignored) |
| `docs/` | `ARCHITECTURE.md`, `battle-arena.md`, `multiplayer.md`, `mcp.md`, `SCALING.md`, `pokemon-red-ram-map.md` (index: `DOCS.md`) |
| `DESIGN*.md` | verified design records (GB, BATTLE, MULTIPLAYER) |

## Non-obvious things — READ before editing these areas

- **Headless / software render**: the frontend refuses `SET_HW_RENDER` so the core delivers CPU
  framebuffers via `video_refresh` (no GL context). gambatte is software-only, so this is automatic.
- **Pixel format**: gambatte delivers **RGB565** (little-endian u16, R5/G6/B5); `frame_to_i420`
  (`src/video.rs`) converts it. ALWAYS stride rows by the callback's real `pitch` — gambatte pads
  160px→256px = 512 B, never `width*2`. (The frontend also handles XRGB8888 cores for the general
  libretro capability.)
- **VP8 canvas is fixed at the first frame's dims** (160×144 for Game Boy). `xrgb_to_i420`
  letterboxes if the frame dims ever change, since VP8 can't resize mid-stream.
- **Audio is i16 stereo @ the core's `sample_rate`** (gambatte 32768), linear-resampled to 48000 for
  stereo Opus; never pad/truncate — the resampler carries fractional position across calls.
- **ROM lifetime**: `need_fullpath==false`, so the core reads our ROM bytes after `load_game`;
  `Emu::new` `mem::forget`s the ROM buffer to keep it alive. Don't "fix" that leak.
- **Forced core options** (`forced_option` in `src/libretro.rs`): `gambatte_gb_colorization=auto`
  (GBC auto-color). A core only queries its own keys, so unrelated options are harmless; to switch
  cores, change the dylib path.
- Keep `webrtc.rs` referring to the crate as `::webrtc` (our module shadows it).
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

End-to-end is verified with headless Chrome (Puppeteer) — see the e2e tests in `test/` and
`test/README.md`. They confirm 640×240 VP8 decode, stereo Opus, and that keyboard input reaches
the N64 core (before/after screenshots differ). Liveness signal in the server log:
`pokemon_red_pvp: ~59.7 fps | N video pkts | M audio pkts | viewers v=.. a=..` every 5 s.

## Conventions

- Edition 2021, `rust-version = "1.92"`. `tracing` for logs (`RUST_LOG=pokemon_red_pvp=info,webrtc=warn`).
- When bumping `webrtc`, re-check the API against the pinned version's source (master is
  restructured toward a different release and is misleading).
