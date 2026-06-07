# pokemon-red-pvp — Architecture & Project Guide

A Rust server that runs a **Game Boy / Pokémon Red emulator 100% on the server** and streams the
live game to a browser over **WebRTC** (VP8 video + Opus audio), with **keyboard input** sent back
over a WebRTC data channel. The browser shows the game inside a **retro CRT TV** UI and exposes an
**AI-agent battle arena** over HTTP.

---

## 1. Lineage (how it got here)

| Milestone | Commit | What changed |
|---|---|---|
| **Game Boy / GBC** | `3ef9cf2` | `gambatte` core; **format-aware** converter (RGB565 + XRGB8888); default Pokémon Red Color |
| **AI battle arena** | `0a867f8` | libretro memory + savestate; `BattleState` reader; action API over HTTP |
| In-browser battle console | `2bfb1ac` | UI panel to act as the agent |
| **Custom legendary matchup** | `63d6263` | pick the two Pokémon; inject full party structs into an intro savestate |

The libretro frontend (`src/libretro.rs`) loads a libretro core, so the same binary runs Game
Boy/GBC just by changing the ROM + core dylib passed on the command line.

---

## 2. High-level architecture

```
                                   SERVER (one process)
 ┌──────────────────────────────────────────────────────────────────────────────────────┐
 │  emulator thread (1 dedicated OS thread, paced to the core's fps)                      │
 │    retro_run()  ->  framebuffer (XRGB8888 | RGB565)  +  i16 stereo audio               │
 │      ├─ frame_to_i420 ──► VP8 encode (vpx-encode / libvpx) ──► video_tx ─┐ broadcast   │
 │      ├─ resample → 48k ──► Opus encode (opus / libopus) ─────► audio_tx ─┤             │
 │      ├─ apply input (browser data channel)  /  agent TapMachine          │             │
 │      └─ publish BattleState snapshot (Pokémon Red)                       │             │
 │                                                                          ▼             │
 │  tokio runtime:  axum :3000   +   webrtc-rs peers (per browser)   writer tasks → tracks│
 └──────────────────────────────────────────────────────────────────────────────────────┘
            ▲  POST /offer (SDP)             ║ VP8 + Opus RTP            ║ DataChannel "input"
            │  GET/POST /battle/*            ▼                          ▲
 ┌──────────────────────────────────────────────────────────────────────────────────────┐
 │  BROWSER  http://localhost:3000   —  CRT-TV UI: <video> + keyboard + AI battle console │
 └──────────────────────────────────────────────────────────────────────────────────────┘
```

**Key properties**
- The **emulator thread is the master clock**: it owns the libretro core and both encoders, runs
  `retro_run()` once per frame, drift-paced to the core's reported fps (GB ≈ 59.73).
- libretro callbacks are bare `extern "C" fn` (no user-data), so per-instance buffers live in
  process **globals** (`static Mutex<FRAME/AUDIO/PAD>` in `libretro.rs`). Exactly **one** emulator
  on one thread keeps that race-free; the core's worker threads write the same buffers under the lock.
- Encoded media is fanned out with `tokio::sync::broadcast`; each connected peer spawns a writer
  task that pulls samples and `write_sample`s into its track.
- HTTP signaling is **non-trickle**: the browser gathers ICE, POSTs a complete offer to `/offer`,
  gets a complete answer. ICE servers are empty (localhost host candidates only).

---

## 3. Components (source files)

| File | Responsibility |
|---|---|
| `src/main.rs` | entry: read ROM + core paths (argv), `pipeline::start`, build the WebRTC API, serve axum on `127.0.0.1:3000`. |
| `src/libretro.rs` | **libretro frontend**: `dlopen` a core, wire the 6 retro callbacks, force software rendering (refuse `SET_HW_RENDER`), provide `GET_LOG_INTERFACE`, load the ROM, expose `clock_frame`, `with_frame`, `audio_drain`, `set_button`, **memory** (`with_system_ram[_mut]`) and **savestates** (`save_state`/`load_state`). |
| `src/video.rs` | `frame_to_i420` (format-aware: **XRGB8888 BGRX** and **RGB565**, pitch-honoring) + realtime **VP8** encoder; canvas dims taken from the first frame; re-inits on resolution change. |
| `src/audio.rs` | i16 stereo from the core → linear resample `core_rate`→48000 → **stereo Opus** in exact 960-sample (20 ms) packets. |
| `src/pipeline.rs` | the per-frame loop; `broadcast` channels; `AppInner` (shared state + channels); input application; the battle snapshot + agent action queue + savestate/setup handlers (all on the emu thread). |
| `src/webrtc.rs` | per-peer `RTCPeerConnection`: VP8 + stereo-Opus tracks, RTCP drain, the input data channel, non-trickle offer/answer, keyframe-on-connect, per-peer cleanup. (Module shadows the `webrtc` crate → use `::webrtc`.) |
| `src/signaling.rs` | axum `Router` + `AppState`: `POST /offer` and the `/battle/*` API. |
| `src/battle.rs` | **Pokémon Red battle arena**: `BattleState`/`BattlePokemon` reader (WRAM, **big-endian** Gen-1 stats), `AgentAction` + `TapMachine` (menu input macro), `inject_*`, and the **custom-matchup** system (`Gen1Species` table, Gen-1 stat formula, `build_party_mon`, `setup_matchup`). |
| `logshim.c` + `build.rs` | a C-variadic log function for libretro `GET_LOG_INTERFACE`. |
| `scripts/apply_ips.py` | apply an IPS patch (used to make `Pokemon Red Color.gbc` from the base ROM). |
| `static/index.html` | the browser client: CRT-TV styling, WebRTC connect, keyboard mapping, and the AI battle console + matchup picker. |

---

## 4. Supported systems & cores

All cores are **libretro** dylibs in `cores/` (arm64). `cores/fetch.sh` downloads them from the
libretro nightly buildbot. `cores/*.dylib` is **gitignored**.

| System | Core(s) | Pixel format | Audio rate | Notes |
|---|---|---|---|---|
| **Game Boy / GBC** | `gambatte` (default), `sameboy` | gambatte **RGB565** (pitch 512, 160 visible) / sameboy XRGB8888 | gambatte 32768 Hz | software; auto-detects DMG vs GBC from ROM header `0x143`. |

**Running each:**
```sh
cargo run --release                                            # default: Pokémon Red Color (.gbc)
cargo run --release -- "Pokemon Red.gb"                        # GB, grayscale + battle arena
cargo run --release -- "Pokemon Red Color.gbc" cores/sameboy_libretro.dylib
```
(`./run.sh [rom] [core]` is a convenience wrapper that also exports the homebrew lib paths.)

---

## 5. The emulation loop (`pipeline.rs::run_loop`, per frame)

0. Drain emu-thread commands: **savestate** save/load, **/battle/setup** (load intro + inject),
   **agent actions** → `TapMachine`; then `taps.tick()` (push button bits via `set_button`).
1. Apply pending **browser input** (data channel → `input_rx`): digital buttons.
2. If a new viewer connected, **reset the VP8 encoder** so the next frame is a keyframe.
3. `emu.clock_frame()` (one `retro_run`). Then **publish the BattleState snapshot** from WRAM.
4. If the frame **resolution changed**, re-init the encoder (VP8 can't resize mid-stream).
5. **Video**: latest framebuffer → `frame_to_i420` → VP8 → `video_tx`.
6. **Audio**: drain i16 stereo → resample → Opus 960-frame packets → `audio_tx`.
7. Periodic stats log. **Drift-compensated sleep** to the next frame deadline.

---

## 6. WebRTC path (`webrtc.rs`)

- `build_api()` once: `MediaEngine` + default codecs (VP8 `video/VP8@90k`, Opus `audio/opus@48k/2`)
  + default interceptors.
- Per `POST /offer`: new `RTCPeerConnection`, add a **VP8** and a **stereo Opus**
  `TrackLocalStaticSample`, spawn an RTCP drain per sender (mandatory), spawn writer tasks that
  subscribe to the broadcast and `write_sample` (`Sample.duration` drives RTP timestamps), wire the
  `"input"` data channel → `input_tx`, and on `Connected` request a keyframe. On
  Disconnected/Failed/Closed an `alive` flag stops the writer tasks (no leak).
- Signaling order is load-bearing: `create_answer → gathering_complete_promise →
  set_local_description → recv() → local_description`.

---

## 7. Web UI (`static/index.html`)

- **CRT television**: rounded "tube" screen with scanlines, RGB shadow-mask, vignette, glass
  glare, flicker, a rolling scanline bar, and a power-on flash. `POWER` button = Connect; a status
  LED (amber standby → green playing). Pure CSS/JS; served live (no rebuild to change it).
- **Controls** (sent as `{type:"down|up", button, player}` over the data channel):
  - **Game Boy**: arrows = D-pad, `X`=A, `Z`=B, `Enter`=Start, `⇧Right`/`⌫`=Select.
- **AGENT BATTLE CONSOLE** (Pokémon Red): Load Battle / Save State / Advance ▶A; live HP bars +
  state; four move buttons; Run; and a **MATCHUP** row (player/enemy dropdowns + level + Start
  Matchup) populated from `/battle/species`.

---

## 8. HTTP API reference (same-origin, `:3000`)

| Method | Path | Body / Response |
|---|---|---|
| `GET` | `/` (+ static) | the CRT-TV client (`static/index.html`) |
| `POST` | `/offer` | `{type:"offer",sdp}` → `{type:"answer",sdp}` (non-trickle WebRTC) |
| `GET` | `/battle/state` | → `BattleState` JSON (503 until the first snapshot) |
| `POST` | `/battle/action` | `{"type":"move","slot":0..3}` · `{"type":"switch","slot":N}` · `{"type":"run"}` · `{"type":"buttons","presses":["A",...]}` → `202` |
| `POST` | `/battle/save` | → the savestate blob (also writes `states/battle.state`) |
| `POST` | `/battle/load` | raw savestate bytes, or empty body → loads `states/battle.state` |
| `GET` | `/battle/species` | → `[[index,"NAME"],...]` selectable species |
| `POST` | `/battle/setup` | `{"player":74,"enemy":75,"level":50}` (internal indices) → `200` live / `400` reason |

See **`docs/battle-arena.md`** for the `BattleState` schema, the agent loop, and the matchup system.

---

## 9. Build, toolchain, dependencies

- **Toolchain pinned to Rust 1.92** (`rust-toolchain.toml`) — webrtc 0.17.x uses `is_multiple_of`
  (stable 1.87) and some transitive deps need 1.88+. The machine's global default (1.86) can't build it.
- **System libs** (homebrew, arm64): `libvpx` (VP8) + `libopus` (Opus). `.cargo/config.toml` exports
  `PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig` and `LIBRARY_PATH=/opt/homebrew/lib` so the
  `vpx-encode` (`ffi-generate`) and `opus` sys-crates link. `clang` compiles `logshim.c` via `build.rs`.
- Crates: `libloading` (dlopen the core), `vpx-encode`, `opus`, `webrtc`, `axum` + `tower-http`,
  `tokio`, `bytes`, `serde`/`serde_json`, `anyhow`, `tracing(-subscriber)`.

```sh
cargo build --release          # or ./run.sh
./cores/fetch.sh               # (re)download cores if cores/*.dylib are missing
RUST_LOG=pokemon_red_pvp=info,webrtc=warn cargo run --release    # verbose logs
```

---

## 10. Extending

**Add a libretro core.** Drop the arm64 `*_libretro.dylib` in `cores/` (add it to `cores/fetch.sh`),
run `cargo run -- "<rom>" cores/<core>.dylib`. If the core uses a new pixel format, add a branch to
`frame_to_i420`. If it needs forced options, add them to `forced_option` in `libretro.rs` (a core
only queries its own keys). The current path is software-only.

**Add a battle species.** Append a `Gen1Species` row to `SPECIES` in `src/battle.rs` with the
**internal index** (NOT the Pokédex number), base stats, type ids, catch rate, and a 4-move set
(ids + PP). It appears in `/battle/species` and the UI dropdowns automatically.

---

## 11. Known quirks / gotchas

- **Headless = refuse `SET_HW_RENDER`** → keeps the core in software. Don't accept it.
- **`GET_LOG_INTERFACE` needs a real C-variadic fn** (`logshim.c`).
- **Pixel format & pitch**: always stride by the callback's real `pitch` (gambatte pads 160→256px).
- **Gen-1 HP/stats are BIG-ENDIAN** — the #1 battle-reader bug (a unit test pins it).
- **HRAM (e.g. FFF3) is not exposed** by libretro memory; battle turn/phase is derived from WRAM.
- **Savestates are ROM-specific**: `states/battle.state` and `states/legendary_intro.state` were
  captured on `Pokemon Red.gb` — run that ROM for the battle arena. `states/` is gitignored.
- **`.gb` is grayscale by design** (DMG); color needs the `.gbc` (CGB header `0x143=0xC0`).
- Cosmetic: enemy `level` sometimes reads 0 and enemy `max_hp` may differ slightly (the engine
  recomputes enemy stats at send-out); doesn't affect sprites/moves/mechanics.

---

## 12. Repository layout

```
pokemon-red-pvp/
├── src/{main,libretro,video,audio,pipeline,webrtc,signaling,battle}.rs
├── static/index.html            CRT-TV client + battle console
├── logshim.c · build.rs         libretro log shim
├── scripts/apply_ips.py         IPS patcher (makes the .gbc)
├── cores/  fetch.sh + *.dylib   libretro cores (dylibs gitignored)
├── states/                      savestates (gitignored; ROM-specific)
├── test/e2e-*.cjs               headless-Chrome end-to-end tests
├── docs/ARCHITECTURE.md         this file
├── docs/battle-arena.md         AI battle arena guide
├── docs/pokemon-red-ram-map.md  Gen-1 battle RAM map
├── DESIGN-GB.md · DESIGN-BATTLE.md   verified design docs (see DOCS.md)
├── CLAUDE.md                    guide for Claude Code
└── README.md
```

**Gitignored** (not in version control): `cores/*.dylib`, `states/`, all ROMs/patches
(`*.gb *.gbc *.ips`), `/target`.

---

## 13. Testing

End-to-end is validated with **headless Chrome (Puppeteer)** — see `test/` and `test/README.md`:
media decode (correct dimensions + stereo Opus), keyboard input reaching the core, peer cleanup,
GB color vs DMG grayscale, and the battle-arena flows (load → state → move; matchup → correct
sprites). Unit tests in `src/battle.rs` pin the big-endian HP read and the Lv50 stat formula.
Liveness signal in the server log: `~60.0 fps | … | viewers v=.. a=..` every 5 s.
```
