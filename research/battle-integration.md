# Battle-Arena Integration Plan (Pokémon Red, libretro/gambatte)

How to turn the existing headless WebRTC streamer into an AI-agent battle arena: read/write the
emulator's RAM, savestate, expose a battle-state reader and an action API. This is a
**codebase-grounded** plan — every signature below matches the existing style in `src/n64.rs`,
`src/pipeline.rs`, `src/signaling.rs`, `src/main.rs`. RAM addresses + endianness live in
`~/pokemon-pvp-red/docs/pokemon-red-ram-map.md`.

## 0. Architecture today (verified by reading the code)

- **`src/n64.rs`** — hand-rolled libretro frontend. `N64::new(core_path, rom_path)` `dlopen`s the
  core, resolves symbols with `lib.get(b"retro_*")`, wires `extern "C"` callbacks, calls
  `retro_init` → `retro_load_game` → `retro_get_system_av_info`. Per-instance state is in **process
  globals** (`static FRAME: Mutex<Frame>`, `static AUDIO`, `static PAD`) because libretro callbacks
  carry no user-data and there's exactly one emulator on one OS thread. `N64` holds only the raw
  `run: RunFn` pointer + `_lib: Library` (keeps the dylib mapped). `clock_frame()` calls
  `(self.run)()`. Input is `set_button(id, pressed)` writing into `PAD`; `cb_input_state` reads it.
  Button id constants `ID_A=8, ID_B=0, ID_START=3, ID_SELECT=2, ID_UP=4, ID_DOWN=5, ID_LEFT=6,
  ID_RIGHT=7` already match the libretro JOYPAD ids the prompt lists. `map_button(&str)` maps wire
  names to `N64Action`.
- **`src/pipeline.rs`** — `start()` spawns the emulator thread running `run_loop`. The loop:
  (1) drains `input_rx.try_recv()` and applies buttons/sticks, (2) handles keyframe requests,
  (3) `emu.clock_frame()`, (4) frame→I420→VP8, (5) audio→Opus, (7) paces to `frame_period`.
  `AppInner { video_tx, audio_tx, input_tx, keyframe_req }` is the shared handle; `InputEvent
  {kind, button, player}` is the wire input type.
- **`src/webrtc.rs`** — the browser opens an `"input"` data channel; `dc.on_message` deserializes
  `InputEvent` and does `input_tx.send(ev)`. So **all input already funnels through one mpsc
  channel into `run_loop`** — agent actions will reuse the exact same path.
- **`src/signaling.rs`** — axum `router(state)` with `AppState { api, inner }`. Only route today is
  `POST /offer`. `ServeDir` is the fallback. New HTTP endpoints attach here with `.route(...)`.
- **`src/main.rs`** — builds `AppState`, binds `127.0.0.1:3000`.

The emulator thread **owns `N64`** and never shares it. So battle reads/writes must happen **on
that thread** (inside `run_loop`), and HTTP handlers communicate with it via channels + a shared
snapshot — never by touching `N64` directly. This is the central design constraint.

---

## 1. `n64.rs` — add memory + savestate accessors (net-new symbols)

The harness does not resolve these yet. Resolve them in `N64::new` alongside the others and store
the raw pointers/fn-pointers on the struct (same pattern as `run`).

### 1a. New symbol types (top of file, near the other `type *Fn`)

```rust
type GetMemDataFn = unsafe extern "C" fn(c_uint) -> *mut c_void;
type GetMemSizeFn = unsafe extern "C" fn(c_uint) -> usize;
type SerializeSizeFn = unsafe extern "C" fn() -> usize;
type SerializeFn   = unsafe extern "C" fn(*mut c_void, usize) -> bool;
type UnserializeFn = unsafe extern "C" fn(*const c_void, usize) -> bool;

pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
```

### 1b. New fields on `struct N64`

```rust
pub struct N64 {
    _lib: Library,
    run: RunFn,
    get_mem_data: GetMemDataFn,
    get_mem_size: GetMemSizeFn,
    serialize_size: SerializeSizeFn,
    serialize: SerializeFn,
    unserialize: UnserializeFn,
    pub fps: f64,
    pub sample_rate: f64,
    pub width: u32,
    pub height: u32,
}
```

### 1c. Resolve them in `N64::new` (inside the existing `unsafe { ... }`, next to `run`)

```rust
let get_mem_data: GetMemDataFn = *lib.get::<GetMemDataFn>(b"retro_get_memory_data")?;
let get_mem_size: GetMemSizeFn = *lib.get::<GetMemSizeFn>(b"retro_get_memory_size")?;
let serialize_size: SerializeSizeFn = *lib.get::<SerializeSizeFn>(b"retro_serialize_size")?;
let serialize: SerializeFn = *lib.get::<SerializeFn>(b"retro_serialize")?;
let unserialize: UnserializeFn = *lib.get::<UnserializeFn>(b"retro_unserialize")?;
```
…and add them to the returned `Ok(N64 { ... })`. (Memory pointers are only valid *after*
`retro_load_game`, which `new` already calls — so fetching the *pointer* is fine here, but always
re-call `get_mem_data`/`get_mem_size` at read time; some cores can relocate the buffer.)

### 1d. Public accessor methods (impl N64)

```rust
/// Run `f` over the core's SYSTEM_RAM (GB: 8 KiB WRAM, CPU 0xC000..0xE000). Returns None if the
/// core exposes no such region. The slice is only valid for the closure (core may own it).
pub fn with_system_ram<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    unsafe {
        let ptr = (self.get_mem_data)(RETRO_MEMORY_SYSTEM_RAM) as *const u8;
        let len = (self.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
        if ptr.is_null() || len == 0 { return None; }
        Some(f(std::slice::from_raw_parts(ptr, len)))
    }
}

/// Mutable WRAM access — for inject_* writes. Same validity rules.
pub fn with_system_ram_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
    unsafe {
        let ptr = (self.get_mem_data)(RETRO_MEMORY_SYSTEM_RAM) as *mut u8;
        let len = (self.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
        if ptr.is_null() || len == 0 { return None; }
        Some(f(std::slice::from_raw_parts_mut(ptr, len)))
    }
}

/// Serialize full emulator state into a fresh Vec (savestate).
pub fn save_state(&self) -> Option<Vec<u8>> {
    unsafe {
        let n = (self.serialize_size)();
        if n == 0 { return None; }
        let mut buf = vec![0u8; n];
        if (self.serialize)(buf.as_mut_ptr() as *mut c_void, n) { Some(buf) } else { None }
    }
}

/// Restore from a savestate blob produced by `save_state`.
pub fn load_state(&self, data: &[u8]) -> bool {
    unsafe { (self.unserialize)(data.as_ptr() as *const c_void, data.len()) }
}
```

> **WRAM-offset helper.** All RAM-map addresses are CPU addresses; `wram_offset = addr - 0xC000`
> for `0xC000..=0xDFFF`. Put `rd8`/`rd16_be` in `battle.rs` (§2), not `n64.rs`, so `n64.rs` stays
> a pure frontend. **HRAM `FFF3` is NOT in SYSTEM_RAM** — derive turn state in the state machine.
> Verify gambatte's SYSTEM_RAM size is exactly `0x2000` at startup (`tracing::info!`).

---

## 2. New `src/battle.rs` — types, reader, injectors, action→input

This is the only Pokémon-specific module. It depends on `&[u8]` (WRAM) + the action queue; it does
**not** depend on `N64` directly, so it's trivially testable.

### 2a. Wire/state types (serde-serializable for the HTTP API)

```rust
#[derive(Clone, serde::Serialize)]
pub struct BattlePokemon {
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub status: u8,            // §7 bitfield
    pub moves: [u8; 4],        // move ids
    pub pp: [u8; 4],           // low 6 bits = current PP
    pub stats: BattleStats,    // attack/defense/speed/special
}

#[derive(Clone, serde::Serialize)]
pub struct BattleStats { pub attack: u16, pub defense: u16, pub speed: u16, pub special: u16 }

#[derive(Clone, serde::Serialize)]
pub struct BattleState {
    pub in_battle: u8,         // wIsInBattle D057: 0 none, 1 wild, 2 trainer, 255 special
    pub battle_type: u8,       // wBattleType D05A
    pub turns_in_battle: u8,   // CCD5
    pub player_selected_move: u8, // CCDC
    pub enemy_selected_move: u8,  // CCDD
    pub menu: MenuPhase,       // software state machine (§4)
    pub player: BattlePokemon, // wBattleMon  (D009..)
    pub enemy: BattlePokemon,  // wEnemyMon   (CFE5..)
    // navigation context
    pub cur_map: u8,           // D35E
    pub x: u8,                 // D362
    pub y: u8,                 // D361
}

/// What an external agent asks for. Deserialized from POST /battle/action JSON.
#[derive(Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentAction {
    /// Use move in slot 0..=3 (FIGHT -> nth move).
    Move { slot: u8 },
    /// Switch to party slot 0..=5 (PKMN menu).
    Switch { slot: u8 },
    /// Run from a wild battle.
    Run,
    /// Raw button taps for scripting/navigation, e.g. ["A","Down","A"].
    Buttons { presses: Vec<String> },
}
```

### 2b. Reader — `read_battle_state(ram: &[u8], menu: MenuPhase) -> BattleState`

Pure function over the WRAM slice. **Every u16 is big-endian** (Gen-1). Uses the addresses in
`docs/pokemon-red-ram-map.md`.

```rust
#[inline] fn rd8(ram: &[u8], addr: u16) -> u8 { ram[(addr - 0xC000) as usize] }
#[inline] fn rd16_be(ram: &[u8], addr: u16) -> u16 {
    let o = (addr - 0xC000) as usize;
    u16::from_be_bytes([ram[o], ram[o + 1]])
}

fn read_player(ram: &[u8]) -> BattlePokemon {
    BattlePokemon {
        species: rd8(ram, 0xD014),
        level:   rd8(ram, 0xD022),
        hp:      rd16_be(ram, 0xD015),
        max_hp:  rd16_be(ram, 0xD023),
        status:  rd8(ram, 0xD018),
        moves: [rd8(ram,0xD01C),rd8(ram,0xD01D),rd8(ram,0xD01E),rd8(ram,0xD01F)],
        pp:    [rd8(ram,0xD02D),rd8(ram,0xD02E),rd8(ram,0xD02F),rd8(ram,0xD030)],
        stats: BattleStats {
            attack:  rd16_be(ram,0xD025), defense: rd16_be(ram,0xD027),
            speed:   rd16_be(ram,0xD029), special: rd16_be(ram,0xD02B),
        },
    }
}
fn read_enemy(ram: &[u8]) -> BattlePokemon {
    BattlePokemon {
        species: rd8(ram, 0xCFE5),
        level:   rd8(ram, 0xCFE8),
        hp:      rd16_be(ram, 0xCFE6),
        max_hp:  rd16_be(ram, 0xCFF4),
        status:  rd8(ram, 0xCFE9),
        moves: [rd8(ram,0xCFED),rd8(ram,0xCFEE),rd8(ram,0xCFEF),rd8(ram,0xCFF0)],
        pp:    [rd8(ram,0xCFFE),rd8(ram,0xCFFF),rd8(ram,0xD000),rd8(ram,0xD001)],
        stats: BattleStats {
            attack:  rd16_be(ram,0xCFF6), defense: rd16_be(ram,0xCFF8),
            speed:   rd16_be(ram,0xCFFA), special: rd16_be(ram,0xCFFC),
        },
    }
}

pub fn read_battle_state(ram: &[u8], menu: MenuPhase) -> BattleState {
    BattleState {
        in_battle: rd8(ram, 0xD057),
        battle_type: rd8(ram, 0xD05A),
        turns_in_battle: rd8(ram, 0xCCD5),
        player_selected_move: rd8(ram, 0xCCDC),
        enemy_selected_move:  rd8(ram, 0xCCDD),
        menu,
        player: read_player(ram),
        enemy:  read_enemy(ram),
        cur_map: rd8(ram, 0xD35E),
        x: rd8(ram, 0xD362),
        y: rd8(ram, 0xD361),
    }
}
```

### 2c. Injectors (write WRAM via `with_system_ram_mut`)

For "set up a battle scenario" tooling (e.g. force enemy HP, swap a move). Same offsets, BE writes.

```rust
#[inline] fn wr8(ram: &mut [u8], addr: u16, v: u8) { ram[(addr - 0xC000) as usize] = v; }
#[inline] fn wr16_be(ram: &mut [u8], addr: u16, v: u16) {
    let o = (addr - 0xC000) as usize; let b = v.to_be_bytes();
    ram[o] = b[0]; ram[o+1] = b[1];
}
pub fn inject_enemy_hp(ram: &mut [u8], hp: u16)  { wr16_be(ram, 0xCFE6, hp); }
pub fn inject_player_hp(ram: &mut [u8], hp: u16) { wr16_be(ram, 0xD015, hp); }
pub fn inject_player_move(ram: &mut [u8], slot: usize, id: u8) { wr8(ram, 0xD01C + slot as u16, id); }
```
> Writing live WRAM mid-battle is racy with the engine on the same turn — write between turns
> (when `MenuPhase::Idle`) or right after a `load_state` for deterministic setup.

### 2d. Action → input macro (the menu driver)

An `AgentAction` is **not** an instantaneous write — it's a *sequence of button taps spread over
frames* driving the in-battle menu. Model it as a queued macro the pipeline plays out one
press per N frames. Each press = press the button (`set_button(id,true)`) for a few frames, then
release (`set_button(id,false)`), then idle a few frames so the game registers it (the menu
debounces; ~4 frames down / 4 up is reliable at 60 fps).

```rust
/// One scheduled tap: hold `button` for `hold` frames, then `gap` idle frames.
pub struct Tap { pub button: usize, pub hold: u8, pub gap: u8 }

/// Expand a high-level action into the concrete button taps for the Gen-1 battle menu.
/// Assumes the FIGHT/PKMN/ITEM/RUN main menu is showing (MenuPhase::MainMenu).
pub fn action_to_taps(a: &AgentAction) -> Vec<Tap> {
    use crate::n64::{ID_A, ID_B, ID_DOWN, ID_UP, ID_RIGHT, ID_LEFT};
    let tap = |b| Tap { button: b, hold: 4, gap: 6 };
    match a {
        // Main menu cursor starts on FIGHT (top-left). FIGHT = A, then Down*slot, then A.
        AgentAction::Move { slot } => {
            let mut v = vec![tap(ID_A)];                       // open FIGHT move list
            for _ in 0..(*slot).min(3) { v.push(tap(ID_DOWN)); } // move cursor down to the move
            v.push(tap(ID_A));                                 // confirm move
            v
        }
        // PKMN is bottom-left: Down then A to open party, Down*slot, A, A (confirm "SWITCH").
        AgentAction::Switch { slot } => {
            let mut v = vec![tap(ID_DOWN), tap(ID_A)];
            for _ in 0..(*slot).min(5) { v.push(tap(ID_DOWN)); }
            v.push(tap(ID_A)); v.push(tap(ID_A));
            v
        }
        // RUN is bottom-right: Down + Right + A.
        AgentAction::Run => vec![tap(ID_DOWN), tap(ID_RIGHT), tap(ID_A)],
        AgentAction::Buttons { presses } => presses.iter()
            .filter_map(|s| crate::n64::map_button(s).and_then(|act| match act {
                crate::n64::N64Action::Btn(id) => Some(tap(id)),
                _ => None, // ignore analog names on GB
            })).collect(),
    }
}
```

> **Menu cursor caveat.** The main-battle-menu cursor is a 2×2 grid (FIGHT top-left, PKMN
> bottom-left, ITEM top-right, RUN bottom-right) and Gen-1 *remembers the last cursor position*.
> The taps above assume the cursor is on FIGHT when the menu opens, which is the default at the
> start of each turn. For robustness, the executor can first read `wCurrentMenuItem` (`CC26`) /
> `wTopMenuItemY/X` (`CC24`/`CC25`) and prepend corrective Up/Left taps to home the cursor before
> navigating. Document this as a v2 hardening step; v1 relies on the default-FIGHT position.

---

## 3. `pipeline.rs` — frame hook (read each frame) + action executor

### 3a. Extend `AppInner` with shared battle state + an action queue

```rust
use crate::battle::{AgentAction, BattleState};
use std::sync::Mutex;

pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>,
    pub audio_tx: broadcast::Sender<EncodedAudio>,
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
    pub keyframe_req: Arc<AtomicBool>,
    // NEW: latest battle snapshot, refreshed every frame by run_loop.
    pub battle: Arc<Mutex<Option<BattleState>>>,
    // NEW: agent actions queued by HTTP, consumed by run_loop.
    pub action_tx: mpsc::UnboundedSender<AgentAction>,
    // NEW: savestate request channels (see §5).
    pub save_tx: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<Option<Vec<u8>>>>,
    pub load_tx: mpsc::UnboundedSender<(Vec<u8>, tokio::sync::oneshot::Sender<bool>)>,
}
```
`start()` creates the new channels/`Arc<Mutex<None>>`, clones senders into `AppInner`, and passes
the receivers + `battle` clone into `run_loop` (exactly mirroring how `input_rx`/`keyframe_req`
are threaded through today).

### 3b. In `run_loop`, after `emu.clock_frame()` (step 3), refresh the snapshot

```rust
// 3c. Refresh battle snapshot from WRAM each frame.
let snap = emu.with_system_ram(|ram| {
    crate::battle::read_battle_state(ram, menu_phase.current())
});
if let Some(s) = snap { *battle.lock().unwrap() = Some(s); }
```
Reading 8 KiB-bounded fields every frame is negligible next to VP8 encode. Reading *after*
`clock_frame` gives the post-frame state.

### 3c. Action executor: a small per-frame state machine, fed by the existing input path

Add, alongside `held`, a tap queue + countdown. Each frame, pop/advance taps and call
`emu.set_button` — i.e. **reuse the exact `set_button` path the browser uses**, no new input
mechanism:

```rust
let mut tap_queue: std::collections::VecDeque<crate::battle::Tap> = VecDeque::new();
let mut cur_tap: Option<(usize /*btn*/, i32 /*frames left*/, bool /*pressed phase*/)> = None;

// --- at top of loop, before clock_frame ---
// enqueue any new agent actions (expand to taps)
while let Ok(action) = action_rx.try_recv() {
    for t in crate::battle::action_to_taps(&action) { tap_queue.push_back(t); }
}
// drive the current tap
match &mut cur_tap {
    Some((btn, left, pressed)) => {
        *left -= 1;
        if *pressed && *left <= 0 {            // end of hold -> release, start gap
            emu.set_button(*btn, false);
            if let Some(t) = tap_queue.front() { /* gap handled below */ }
            *pressed = false;
            *left = /* gap */ 6;
        } else if !*pressed && *left <= 0 {
            cur_tap = None;                     // gap done
        }
    }
    None => {
        if let Some(t) = tap_queue.pop_front() {
            emu.set_button(t.button, true);
            cur_tap = Some((t.button, t.hold as i32, true));
        }
    }
}
```
(Tighten the hold/gap bookkeeping when implementing; the shape is: one button "down for hold
frames, up for gap frames" before the next tap.) Because both the browser and the agent write
into the same `PAD`, a human can still take over; the agent macro just sets/clears the same bits.

---

## 4. `MenuPhase` software state machine (detecting the move menu)

There is no clean single RAM flag that says "the FIGHT move list is open." Drive it off `D057`
(`wIsInBattle`) plus optional menu-cursor reads. Keep it in `battle.rs`, owned by `run_loop`:

```rust
#[derive(Clone, Copy, serde::Serialize, PartialEq)]
pub enum MenuPhase { Overworld, BattleIntro, MainMenu, MoveList, Animating, Idle }
```
Transition logic each frame:
- `D057 == 0` → `Overworld` (clear any tap queue / action in flight).
- `D057 != 0` and we just entered → `BattleIntro`; once `wBattleMon` HP/level look populated
  (`D022 != 0`), go `MainMenu`.
- After issuing a `Move`'s first `A`, mark `MoveList` (so the reader can expose it); after the
  confirm `A`, mark `Animating`. Return to `MainMenu` when `wPlayerSelectedMove`/`turns_in_battle`
  advance and HP bars settle.

For a *robust* "is the move menu open" detector without a dedicated flag, read
`wCurrentMenuItem` (`CC26`) / `wMaxMenuItem` and the text-box state; but **for v1 the `D057` +
issued-action state machine is sufficient** — agents poll `GET /battle/state`, see
`in_battle != 0` and `menu == MainMenu`, then `POST /battle/action`. The executor only plays taps
while `in_battle != 0`.

---

## 5. `signaling.rs` + `main.rs` — HTTP API

`AppState` already carries `inner: Arc<AppInner>`, so handlers reach everything via `state.inner`.
Add four routes in `router()`:

```rust
Router::new()
    .route("/offer", post(offer_handler))
    .route("/battle/state",  get(battle_state_handler))
    .route("/battle/action", post(battle_action_handler))
    .route("/battle/save",   post(battle_save_handler))
    .route("/battle/load",   post(battle_load_handler))
    .fallback_service(static_service)
    .with_state(state)
```
(add `use axum::routing::get;`)

```rust
// GET /battle/state -> the latest snapshot the emulator thread published.
async fn battle_state_handler(State(s): State<AppState>)
    -> Result<Json<crate::battle::BattleState>, (StatusCode, String)> {
    match s.inner.battle.lock().unwrap().clone() {
        Some(st) => Ok(Json(st)),
        None => Err((StatusCode::SERVICE_UNAVAILABLE, "no battle state yet".into())),
    }
}

// POST /battle/action  body: {"type":"move","slot":0} | {"type":"switch","slot":2}
//                            | {"type":"run"} | {"type":"buttons","presses":["A","Down","A"]}
async fn battle_action_handler(State(s): State<AppState>, Json(a): Json<crate::battle::AgentAction>)
    -> StatusCode {
    let _ = s.inner.action_tx.send(a);   // queued; executor plays it over frames
    StatusCode::ACCEPTED
}

// POST /battle/save -> raw savestate bytes (application/octet-stream).
async fn battle_save_handler(State(s): State<AppState>)
    -> Result<Vec<u8>, (StatusCode, String)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = s.inner.save_tx.send(tx);
    match rx.await {
        Ok(Some(buf)) => Ok(buf),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "serialize failed".into())),
    }
}

// POST /battle/load  body: raw savestate bytes.
async fn battle_load_handler(State(s): State<AppState>, body: axum::body::Bytes)
    -> StatusCode {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = s.inner.load_tx.send((body.to_vec(), tx));
    match rx.await { Ok(true) => StatusCode::OK, _ => StatusCode::INTERNAL_SERVER_ERROR }
}
```

`run_loop` drains `save_rx`/`load_rx` each iteration (they cross the async/sync boundary via
oneshot, like the existing channels):
```rust
while let Ok(reply) = save_rx.try_recv() { let _ = reply.send(emu.save_state()); }
while let Ok((data, reply)) = load_rx.try_recv() { let _ = reply.send(emu.load_state(&data)); }
```
`main.rs` needs **no change** beyond what `pipeline::start` already returns — `AppState` is built
from `inner` exactly as today.

---

## 6. Module-boundary summary

| Concern | Location | Notes |
|---|---|---|
| dlopen + raw memory/serialize FFI | `src/n64.rs` | add 5 symbols + 4 methods (§1). Stays Pokémon-agnostic. |
| Pokémon types, reader, injectors, action→taps, MenuPhase | **new `src/battle.rs`** | pure functions over `&[u8]` + action enum (§2, §4). Unit-testable with a fake WRAM `Vec<u8>`. |
| frame hook + action executor + savestate plumbing | `src/pipeline.rs` | refresh snapshot after `clock_frame`; play tap queue via existing `set_button` (§3, §5). |
| HTTP API | `src/signaling.rs` | 4 routes on existing `AppState.inner` (§5). |
| wiring | `src/main.rs` | none beyond `mod battle;` in main.rs and unchanged `AppState` build. |

## 7. Risks / verify-at-runtime (no emulator was run for this doc)

1. **Endianness** — Gen-1 HP/stats are **big-endian**; the reader uses `from_be_bytes`. Documented
   and cross-checked against DataCrystal + pokered convention. (Highest-impact bug if wrong.)
2. **gambatte SYSTEM_RAM** — confirm `retro_get_memory_size(2) == 0x2000` and that CPU `0xC000`
   maps to offset 0. Log it once at startup. If gambatte returns a different region, adjust the
   `- 0xC000` base.
3. **`FFF3` HRAM** — almost certainly outside SYSTEM_RAM; the plan does not depend on it (uses the
   `D057` state machine). If a probe shows HRAM is included (size > 0x2000), it can be added.
4. **D057 vs D05A label** — DataCrystal swaps the names vs pokered; this plan uses pokered's
   `wIsInBattle=D057`, `wBattleType=D05A`. Verify by entering a wild battle and checking `D057==1`.
5. **Enemy struct off-by-one** (`CFED` moves vs DataCrystal's `CFEE`) — verify `CFE6` HP and
   `CFED` first-move id against the on-screen battle once. Anchors chosen to match the prompt spec.
6. **Menu cursor memory** — taps assume cursor starts on FIGHT each turn; harden with `CC26`/
   `CC24`/`CC25` reads + homing taps in v2 (§2d).
7. **Savestate determinism** — `retro_unserialize` then immediately `inject_*` gives reproducible
   battle setups for agent eval.

See `~/pokemon-pvp-red/docs/pokemon-red-ram-map.md` for all addresses.
Template harness: `~/pokemon-pvp-red/research/gb-harness`.
