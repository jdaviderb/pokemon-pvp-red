# DESIGN-BATTLE.md — Pokémon Red WebRTC → AI-Agent Battle Arena

Concrete, copy-pasteable implementation plan. Everything below is grounded in three verified
probes + a read of `src/{n64.rs,pipeline.rs,signaling.rs,main.rs,webrtc.rs}`.

- Memory/savestate API verified: `research/battle-memory-api.md`
- Reach-a-battle + RAM-map verify + CCDD experiment: `research/battle-reach-and-ccdd.md`
- Codebase integration + clean RAM map: `research/battle-integration.md`
- RAM addresses + endianness: `docs/pokemon-red-ram-map.md`
- **Captured battle savestate (REAL, 59650 bytes): `~/pokemon-pvp-red/states/battle.state`**

---

## 1. FEASIBILITY REPORT

### Can we "start a battle from memory"? Honest verdict.

**No — not by hand-assembling a battle out of zeroed RAM, and you should not try.** A Gen-1 battle
is not just the `wBattleMon`/`wEnemyMon` structs; it depends on a large, interlocking machine
state: the gameplay loop's program counter, the text/menu engine state, RNG seeds, the active
map/script context, HRAM battle-turn flags (`FFF3` — **not even exposed** by gambatte's memory
API), PPU/APU state, and MBC banking. Writing the ~80 bytes of two mon structs into WRAM while the
CPU is sitting in the overworld loop does **not** put the engine "in a battle" — `D057` would be a
dangling flag and the next `retro_run()` would either ignore it or crash the script engine. The
probe confirmed the memory map is readable/writable, but it also confirmed (a) HRAM is unreachable
and (b) the savestate blob is 59650 bytes of *whole-machine* state precisely because a battle is
that much more than the visible RAM fields.

**What IS feasible, verified, and is the MVP:**

> **MVP = savestate (battle bootstrap) + per-frame WRAM read (battle state) + agent action via
> input-macro (menu navigation).**

Each leg is empirically proven:

1. **Bootstrap via savestate.** `retro_serialize`/`retro_unserialize` round-trip cleanly (both
   return `true`; WRAM reverts byte-for-byte; blob is portable across fresh core instances —
   `research/battle-memory-api.md` Tasks 4/4b). A **real rival battle was reached fully headless**
   (boot → name → bedroom → Pallet → Oak → take starter → rival battle auto-starts, `D057=2`) and
   saved to `states/battle.state` (`research/battle-reach-and-ccdd.md` §1–2). Loading that blob
   drops the emulator straight onto the FIGHT action-menu of a Charmander-vs-Squirtle rival fight.
2. **Battle state via per-frame WRAM read.** `RETRO_MEMORY_SYSTEM_RAM` (id 2) = exactly 8192 bytes
   = GB WRAM `0xC000–0xDFFF`; CPU addr `A` ↦ `SYSTEM_RAM[A-0xC000]`. The full battle RAM map was
   read live and is internally consistent (L5 Charmander SCRATCH/GROWL vs L5 Squirtle TACKLE/TAIL
   WHIP, HPs, PP, stats). **HP/stats are BIG-ENDIAN** (`D015=20` only via `from_be_bytes`).
3. **Agent action via input-macro.** Determinism is proven (same state + same input script → byte
   identical WRAM). A menu-navigation macro (press A for FIGHT, Down×slot, A) drives the engine
   *the legit way*, so all the invisible machine state stays consistent — strictly better than
   poking selected-move bytes. (The CCDD poke also works, but only the player side needs driving;
   see §6.)

### Was a real battle savestate captured?

**Yes.** `~/pokemon-pvp-red/states/battle.state` — 59650 bytes (matches the
verified `retro_serialize_size`), 42620 nonzero bytes, captured at the FIGHT action-menu of a
genuine headless-reached rival battle (`D057=2`). Also present:
`states/bedroom.state` (a controllable overworld restore point).

**`POST /battle/save` flow (for users who want to capture their own start states by playing in the
browser):**

1. `cargo run` → open `http://localhost:3000` → Connect → play with the keyboard until you are
   sitting on the battle menu you want as the canonical start.
2. `curl -X POST http://localhost:3000/battle/save -o states/battle.state` — the emulator thread
   calls `emu.save_state()` and the handler streams the 59650-byte blob back; redirect it to a
   file (handler default also writes `states/battle.state` server-side, see §5).
3. To restart that exact match any time:
   `curl -X POST http://localhost:3000/battle/load --data-binary @states/battle.state`.

So bootstrap works two ways and both are covered: ship the already-captured `states/battle.state`,
**or** let a user capture their own via the documented `POST /battle/save` flow.

---

## 2. `src/n64.rs` ADDITIONS

The harness resolves symbols with `lib.get(b"retro_*")` and stores raw fn pointers on `N64` (see
`run: RunFn`). Add five symbols + two memory-id constants + four methods, matching that pattern
exactly. `c_uint`/`c_void` are already imported at the top of the file.

### 2a. Symbol types + memory-id constants (near the other `type *Fn` aliases, ~line 351)

```rust
type GetMemDataFn = unsafe extern "C" fn(c_uint) -> *mut c_void;
type GetMemSizeFn = unsafe extern "C" fn(c_uint) -> usize;
type SerializeSizeFn = unsafe extern "C" fn() -> usize;
type SerializeFn = unsafe extern "C" fn(*mut c_void, usize) -> bool;
type UnserializeFn = unsafe extern "C" fn(*const c_void, usize) -> bool;

pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2; // GB WRAM 0xC000..0xE000 (verified 8192 bytes)
```

### 2b. New fields on `struct N64` (extend the existing struct, ~line 353)

```rust
pub struct N64 {
    _lib: Library, // keep the dylib mapped for the process lifetime
    run: RunFn,    // raw fn pointer (valid while _lib lives)
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

### 2c. Resolve them in `N64::new`, inside the existing `unsafe { ... }` next to `run` (~line 401)

```rust
            // raw fn pointer for run (outlives the borrow; valid while `lib` is alive)
            let run: RunFn = *lib.get::<RunFn>(b"retro_run")?;
            // memory + savestate accessors (verified present & callable in gambatte)
            let get_mem_data: GetMemDataFn = *lib.get::<GetMemDataFn>(b"retro_get_memory_data")?;
            let get_mem_size: GetMemSizeFn = *lib.get::<GetMemSizeFn>(b"retro_get_memory_size")?;
            let serialize_size: SerializeSizeFn = *lib.get::<SerializeSizeFn>(b"retro_serialize_size")?;
            let serialize: SerializeFn = *lib.get::<SerializeFn>(b"retro_serialize")?;
            let unserialize: UnserializeFn = *lib.get::<UnserializeFn>(b"retro_unserialize")?;
```

Then carry them out of the `unsafe` block. The cleanest change to the existing code: the block
currently returns a 5-tuple `(run, fps, sample_rate, width, height)`. Bind the five new pointers
*outside* the tuple by hoisting their `let`s above the tuple expression is not possible (they're
inside `unsafe`), so extend the returned tuple:

```rust
            (
                run,
                get_mem_data,
                get_mem_size,
                serialize_size,
                serialize,
                unserialize,
                av.timing.fps,
                av.timing.sample_rate,
                av.geometry.base_width,
                av.geometry.base_height,
            )
        };
```

and change the binding + `Ok(...)`:

```rust
        let (run, get_mem_data, get_mem_size, serialize_size, serialize, unserialize,
             fps, sample_rate, width, height) = unsafe { /* block above */ };

        // One-time sanity: gambatte SYSTEM_RAM must be exactly 0x2000 (8 KiB WRAM).
        let sys_ram_len = unsafe { get_mem_size(RETRO_MEMORY_SYSTEM_RAM) };
        tracing::info!("SYSTEM_RAM size = {sys_ram_len} bytes (expect 8192 for GB WRAM)");

        Ok(N64 {
            _lib: lib,
            run,
            get_mem_data,
            get_mem_size,
            serialize_size,
            serialize,
            unserialize,
            fps,
            sample_rate,
            width,
            height,
        })
```

> The memory pointer is only valid after `retro_load_game`, which `new` already called — but we
> store the *function* pointers, not the data pointer, and re-call `get_mem_data` at every read
> (cores may relocate the buffer). Fetching the *fn* pointers here is fine.

### 2d. Public accessor methods (in `impl N64`, next to `with_frame`/`set_button`)

```rust
    /// Borrow the core's SYSTEM_RAM (GB: 8 KiB WRAM, CPU 0xC000..0xE000) for the closure.
    /// Returns None if the core exposes no such region. Slice valid only inside `f`.
    pub fn with_system_ram<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        unsafe {
            let ptr = (self.get_mem_data)(RETRO_MEMORY_SYSTEM_RAM) as *const u8;
            let len = (self.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
            if ptr.is_null() || len == 0 {
                return None;
            }
            Some(f(std::slice::from_raw_parts(ptr, len)))
        }
    }

    /// Mutable WRAM access for inject_* writes. Same validity rules.
    /// VERIFIED: writing SYSTEM_RAM[D163-0xC000]=3 reads back 3 and survives a retro_run().
    pub fn with_system_ram_mut<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
        unsafe {
            let ptr = (self.get_mem_data)(RETRO_MEMORY_SYSTEM_RAM) as *mut u8;
            let len = (self.get_mem_size)(RETRO_MEMORY_SYSTEM_RAM);
            if ptr.is_null() || len == 0 {
                return None;
            }
            Some(f(std::slice::from_raw_parts_mut(ptr, len)))
        }
    }

    /// Serialize full emulator state into a fresh Vec (savestate; verified 59650 bytes for this ROM).
    pub fn save_state(&self) -> Option<Vec<u8>> {
        unsafe {
            let n = (self.serialize_size)();
            if n == 0 {
                return None;
            }
            let mut buf = vec![0u8; n];
            if (self.serialize)(buf.as_mut_ptr() as *mut c_void, n) {
                Some(buf)
            } else {
                None
            }
        }
    }

    /// Restore from a savestate blob produced by `save_state`. VERIFIED: full WRAM revert.
    pub fn load_state(&self, data: &[u8]) -> bool {
        unsafe { (self.unserialize)(data.as_ptr() as *const c_void, data.len()) }
    }
```

`n64.rs` stays Pokémon-agnostic — the `addr-0xC000` offset math and BE reads live in `battle.rs`.

---

## 3. `src/battle.rs` (NEW)

Pure module over `&[u8]` WRAM + an action enum. No dependency on `N64`, so unit-testable with a
fake `vec![0u8; 8192]`. Register it in `main.rs` with `mod battle;` (see §5).

```rust
//! Pokémon Red (Gen-1, DMG) battle-arena layer: read battle state out of WRAM, inject party/HP
//! for scenario setup, and turn high-level agent actions into per-frame button taps that drive
//! the in-battle menu. All addresses are CPU addresses in 0xC000..0xDFFF; see
//! docs/pokemon-red-ram-map.md. **Every multi-byte value is BIG-ENDIAN (Gen-1 quirk).**

use crate::n64::{ID_A, ID_DOWN, ID_LEFT, ID_RIGHT, ID_UP};

// ---------- WRAM access helpers (offset = addr - 0xC000) ----------
#[inline]
fn rd8(ram: &[u8], addr: u16) -> u8 {
    ram[(addr - 0xC000) as usize]
}
/// Gen-1 multi-byte values are BIG-ENDIAN: hi byte at the lower address.
#[inline]
fn rd16_be(ram: &[u8], addr: u16) -> u16 {
    let o = (addr - 0xC000) as usize;
    u16::from_be_bytes([ram[o], ram[o + 1]])
}
#[inline]
fn wr8(ram: &mut [u8], addr: u16, v: u8) {
    ram[(addr - 0xC000) as usize] = v;
}
#[inline]
fn wr16_be(ram: &mut [u8], addr: u16, v: u16) {
    let o = (addr - 0xC000) as usize;
    let b = v.to_be_bytes();
    ram[o] = b[0];
    ram[o + 1] = b[1];
}

// ---------- serde-serializable state types (the HTTP API speaks these) ----------
#[derive(Clone, serde::Serialize)]
pub struct BattleStats {
    pub attack: u16,
    pub defense: u16,
    pub speed: u16,
    pub special: u16,
}

#[derive(Clone, serde::Serialize)]
pub struct BattlePokemon {
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub status: u8,     // §7 bitfield: 0 = healthy
    pub moves: [u8; 4], // move ids
    pub pp: [u8; 4],    // low 6 bits = current PP
    pub stats: BattleStats,
}

#[derive(Clone, Copy, serde::Serialize, PartialEq, Eq, Debug)]
pub enum MenuPhase {
    Overworld,
    BattleIntro,
    MainMenu,
    MoveList,
    Animating,
    Idle,
}

#[derive(Clone, serde::Serialize)]
pub struct BattleState {
    pub in_battle: u8,            // wIsInBattle D057: 0 none, 1 wild, 2 trainer, 255 special
    pub battle_type: u8,          // wBattleType D05A
    pub turns_in_battle: u8,      // CCD5
    pub player_selected_move: u8, // CCDC
    pub enemy_selected_move: u8,  // CCDD
    pub menu: MenuPhase,          // software state machine (§4 of integration doc)
    pub player: BattlePokemon,    // wBattleMon  (D009..)
    pub enemy: BattlePokemon,     // wEnemyMon   (CFE5..)
    pub cur_map: u8,              // D35E
    pub x: u8,                    // D362
    pub y: u8,                    // D361
}

/// What an external agent asks for; deserialized from POST /battle/action JSON.
/// {"type":"move","slot":0} | {"type":"switch","slot":2} | {"type":"run"}
/// | {"type":"buttons","presses":["A","Down","A"]}
#[derive(Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentAction {
    Move { slot: u8 },             // FIGHT -> nth move (0..=3)
    Switch { slot: u8 },           // PKMN menu -> party slot (0..=5)
    Run,                           // RUN (wild only)
    Buttons { presses: Vec<String> }, // raw taps for scripting/navigation
}

// ---------- reader (pure; addresses verified live, see battle-reach-and-ccdd.md §3) ----------
fn read_player(ram: &[u8]) -> BattlePokemon {
    BattlePokemon {
        species: rd8(ram, 0xD014),
        level: rd8(ram, 0xD022),
        hp: rd16_be(ram, 0xD015),
        max_hp: rd16_be(ram, 0xD023),
        status: rd8(ram, 0xD018),
        moves: [rd8(ram, 0xD01C), rd8(ram, 0xD01D), rd8(ram, 0xD01E), rd8(ram, 0xD01F)],
        pp: [rd8(ram, 0xD02D), rd8(ram, 0xD02E), rd8(ram, 0xD02F), rd8(ram, 0xD030)],
        stats: BattleStats {
            attack: rd16_be(ram, 0xD025),
            defense: rd16_be(ram, 0xD027),
            speed: rd16_be(ram, 0xD029),
            special: rd16_be(ram, 0xD02B),
        },
    }
}
fn read_enemy(ram: &[u8]) -> BattlePokemon {
    BattlePokemon {
        species: rd8(ram, 0xCFE5),
        level: rd8(ram, 0xCFE8),
        hp: rd16_be(ram, 0xCFE6),
        max_hp: rd16_be(ram, 0xCFF4),
        status: rd8(ram, 0xCFE9),
        moves: [rd8(ram, 0xCFED), rd8(ram, 0xCFEE), rd8(ram, 0xCFEF), rd8(ram, 0xCFF0)],
        pp: [rd8(ram, 0xCFFE), rd8(ram, 0xCFFF), rd8(ram, 0xD000), rd8(ram, 0xD001)],
        stats: BattleStats {
            attack: rd16_be(ram, 0xCFF6),
            defense: rd16_be(ram, 0xCFF8),
            speed: rd16_be(ram, 0xCFFA),
            special: rd16_be(ram, 0xCFFC),
        },
    }
}

/// Read the full battle snapshot from a SYSTEM_RAM slice. `menu` is supplied by the run-loop's
/// software state machine (§4) since no single RAM flag marks "FIGHT list open".
pub fn read_battle_state(ram: &[u8], menu: MenuPhase) -> BattleState {
    BattleState {
        in_battle: rd8(ram, 0xD057),
        battle_type: rd8(ram, 0xD05A),
        turns_in_battle: rd8(ram, 0xCCD5),
        player_selected_move: rd8(ram, 0xCCDC),
        enemy_selected_move: rd8(ram, 0xCCDD),
        menu,
        player: read_player(ram),
        enemy: read_enemy(ram),
        cur_map: rd8(ram, 0xD35E),
        x: rd8(ram, 0xD362),
        y: rd8(ram, 0xD361),
    }
}

// ---------- injectors (scenario setup; write between turns or right after load_state) ----------
/// Overwrite the active player mon's current HP (BE). Useful for forcing low-HP test scenarios.
pub fn inject_player_hp(ram: &mut [u8], hp: u16) {
    wr16_be(ram, 0xD015, hp);
}
/// Overwrite the active enemy mon's current HP (BE).
pub fn inject_enemy_hp(ram: &mut [u8], hp: u16) {
    wr16_be(ram, 0xCFE6, hp);
}

/// Inject the PLAYER party header: count + species list (+ 0xFF terminator). Slots beyond `species`
/// are zeroed up to the terminator. Does NOT build the 44-byte mon structs — for full-roster
/// scenarios prefer capturing a savestate; this is for count/species smoke tests.
pub fn inject_player_party(ram: &mut [u8], species: &[u8]) {
    let n = species.len().min(6) as u8;
    wr8(ram, 0xD163, n); // wPartyCount
    for i in 0..6u16 {
        let v = species.get(i as usize).copied().unwrap_or(0);
        wr8(ram, 0xD164 + i, v); // wPartySpecies[i]
    }
    wr8(ram, 0xD164 + n as u16, 0xFF); // list terminator
}
/// Inject the ENEMY party header (count + species + terminator at D89C/D89D..).
pub fn inject_enemy_party(ram: &mut [u8], species: &[u8]) {
    let n = species.len().min(6) as u8;
    wr8(ram, 0xD89C, n); // wEnemyPartyCount
    for i in 0..6u16 {
        let v = species.get(i as usize).copied().unwrap_or(0);
        wr8(ram, 0xD89D + i, v); // wEnemyPartySpecies[i]
    }
    wr8(ram, 0xD89D + n as u16, 0xFF);
}

// ---------- action -> input macro (the menu driver) ----------
/// One scheduled tap: hold `button` for `hold` frames, then `gap` idle frames before the next tap.
/// ~4 down / ~6 up is reliable at 60 fps (the Gen-1 menu debounces). Verified determinism in
/// battle-memory-api.md Task 5 (same state + same input script => identical WRAM).
#[derive(Clone, Copy, Debug)]
pub struct Tap {
    pub button: usize,
    pub hold: u8,
    pub gap: u8,
}

fn tap(b: usize) -> Tap {
    Tap { button: b, hold: 4, gap: 6 }
}

/// Expand a high-level action into concrete taps for the Gen-1 battle menu. Assumes the main
/// FIGHT/PKMN/ITEM/RUN menu is showing with the cursor defaulted to FIGHT (true at turn start).
/// 2x2 grid: FIGHT top-left, PKMN bottom-left, ITEM top-right, RUN bottom-right.
pub fn action_to_taps(a: &AgentAction) -> Vec<Tap> {
    match a {
        // FIGHT (A) opens the move list; Down*slot moves the move cursor; A confirms.
        AgentAction::Move { slot } => {
            let mut v = vec![tap(ID_A)];
            for _ in 0..(*slot).min(3) {
                v.push(tap(ID_DOWN));
            }
            v.push(tap(ID_A));
            v
        }
        // PKMN is bottom-left: Down + A opens party; Down*slot; A; A (confirm SWITCH).
        AgentAction::Switch { slot } => {
            let mut v = vec![tap(ID_DOWN), tap(ID_A)];
            for _ in 0..(*slot).min(5) {
                v.push(tap(ID_DOWN));
            }
            v.push(tap(ID_A));
            v.push(tap(ID_A));
            v
        }
        // RUN is bottom-right: Down + Right + A.
        AgentAction::Run => vec![tap(ID_DOWN), tap(ID_RIGHT), tap(ID_A)],
        // Raw button names (reuse the existing wire-name mapping; ignore analog/unknown).
        AgentAction::Buttons { presses } => presses
            .iter()
            .filter_map(|s| match s.as_str() {
                "A" => Some(tap(ID_A)),
                "Up" => Some(tap(ID_UP)),
                "Down" => Some(tap(ID_DOWN)),
                "Left" => Some(tap(ID_LEFT)),
                "Right" => Some(tap(ID_RIGHT)),
                "B" => Some(tap(crate::n64::ID_B)),
                "Start" => Some(tap(crate::n64::ID_START)),
                "Select" => Some(tap(crate::n64::ID_SELECT)),
                _ => None,
            })
            .collect(),
    }
}

/// Per-frame tap-macro player. Holds a button for `hold` frames, releases for `gap`, then advances.
/// Owned by run_loop; driven by `apply_agent_action` once per frame. Reuses N64::set_button — the
/// exact path the browser input channel uses, so a human can still co-drive the same PAD bits.
#[derive(Default)]
pub struct TapMachine {
    queue: std::collections::VecDeque<Tap>,
    cur: Option<Tap>,
    left: i32,    // frames remaining in the current phase
    pressed: bool, // true = in hold phase, false = in gap phase
}

impl TapMachine {
    /// Queue a high-level action's taps. Call from the HTTP-fed action drain.
    pub fn enqueue(&mut self, a: &AgentAction) {
        for t in action_to_taps(a) {
            self.queue.push_back(t);
        }
    }
    /// True while a macro is mid-flight (used to gate "ready for next action").
    pub fn busy(&self) -> bool {
        self.cur.is_some() || !self.queue.is_empty()
    }
    /// Drop everything (e.g. when leaving battle).
    pub fn clear(&mut self, emu: &crate::n64::N64) {
        if let Some(t) = self.cur.take() {
            emu.set_button(t.button, false);
        }
        self.queue.clear();
        self.left = 0;
        self.pressed = false;
    }

    /// Advance one frame: call ONCE per frame, BEFORE emu.clock_frame(). Pushes button state into
    /// PAD via emu.set_button. This is the "apply_agent_action" step driven from the pipeline.
    pub fn tick(&mut self, emu: &crate::n64::N64) {
        match &self.cur {
            Some(t) => {
                self.left -= 1;
                if self.left <= 0 {
                    if self.pressed {
                        // end of hold -> release, start gap
                        emu.set_button(t.button, false);
                        self.pressed = false;
                        self.left = t.gap as i32;
                    } else {
                        // gap done -> retire this tap
                        self.cur = None;
                    }
                }
            }
            None => {
                if let Some(t) = self.queue.pop_front() {
                    emu.set_button(t.button, true);
                    self.left = t.hold as i32;
                    self.pressed = true;
                    self.cur = Some(t);
                }
            }
        }
    }
}

// ---------- MenuPhase software state machine (no single RAM flag for "move list open") ----------
/// Derive the menu phase each frame from D057 + whether a macro is in flight. v1 heuristic; harden
/// later with wCurrentMenuItem (CC26) / wTopMenuItemY,X (CC24/CC25) reads. `macro_busy` = TapMachine::busy().
pub fn next_menu_phase(prev: MenuPhase, ram: &[u8], macro_busy: bool) -> MenuPhase {
    let in_battle = rd8(ram, 0xD057);
    let player_lvl = rd8(ram, 0xD022);
    if in_battle == 0 {
        return MenuPhase::Overworld;
    }
    if macro_busy {
        return MenuPhase::Animating; // a player action is being played out / resolving
    }
    match prev {
        MenuPhase::Overworld => MenuPhase::BattleIntro,
        MenuPhase::BattleIntro if player_lvl != 0 => MenuPhase::MainMenu,
        MenuPhase::Animating => MenuPhase::MainMenu, // macro finished -> back to the action menu
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Fake WRAM helper: write a CPU addr's byte into an 8 KiB slice.
    fn put(ram: &mut [u8], addr: u16, v: u8) {
        ram[(addr - 0xC000) as usize] = v;
    }
    #[test]
    fn hp_is_big_endian() {
        let mut ram = vec![0u8; 0x2000];
        // 20 decimal = 0x0014; BE => hi byte 0x00 at D015, lo 0x14 at D016.
        put(&mut ram, 0xD015, 0x00);
        put(&mut ram, 0xD016, 0x14);
        assert_eq!(read_player(&ram).hp, 20); // LE would wrongly give 0x1400 = 5120
    }
    #[test]
    fn move_taps_navigate_down() {
        // slot 2 => A, Down, Down, A
        let taps = action_to_taps(&AgentAction::Move { slot: 2 });
        assert_eq!(taps.len(), 4);
        assert_eq!(taps[0].button, ID_A);
        assert_eq!(taps[1].button, ID_DOWN);
        assert_eq!(taps[3].button, ID_A);
    }
}
```

---

## 4. `src/pipeline.rs` HOOK — exact diff points

### 4a. Imports (top of file, with the existing `use crate::...`)

```rust
use std::sync::Mutex;                         // ADD (next to the existing Arc import)
use crate::battle::{AgentAction, BattleState, MenuPhase, TapMachine};  // ADD
```

### 4b. Extend `AppInner` (replace the struct at ~line 40)

```rust
pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>,
    pub audio_tx: broadcast::Sender<EncodedAudio>,
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
    pub keyframe_req: Arc<AtomicBool>,
    // NEW — battle arena:
    pub battle: Arc<Mutex<Option<BattleState>>>, // latest snapshot, refreshed every frame
    pub action_tx: mpsc::UnboundedSender<AgentAction>, // agent actions queued by HTTP
    pub save_tx: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<Option<Vec<u8>>>>,
    pub load_tx: mpsc::UnboundedSender<(Vec<u8>, tokio::sync::oneshot::Sender<bool>)>,
}
```

### 4c. `start()` — create the channels and thread them in (replace the body at ~line 47)

```rust
pub fn start(core_path: String, rom_path: String) -> Arc<AppInner> {
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(16);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(64);
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let keyframe_req = Arc::new(AtomicBool::new(false));

    // NEW channels + shared snapshot.
    let (action_tx, action_rx) = mpsc::unbounded_channel::<AgentAction>();
    let (save_tx, save_rx) = mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<Option<Vec<u8>>>>();
    let (load_tx, load_rx) =
        mpsc::unbounded_channel::<(Vec<u8>, tokio::sync::oneshot::Sender<bool>)>();
    let battle: Arc<Mutex<Option<BattleState>>> = Arc::new(Mutex::new(None));

    let v = video_tx.clone();
    let a = audio_tx.clone();
    let kf = keyframe_req.clone();
    let battle_thread = battle.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_loop(
            core_path, rom_path, v, a, input_rx, kf,
            action_rx, save_rx, load_rx, battle_thread,
        ) {
            tracing::error!("emulator loop ended: {e:?}");
        }
    });

    Arc::new(AppInner {
        video_tx,
        audio_tx,
        input_tx,
        keyframe_req,
        battle,
        action_tx,
        save_tx,
        load_tx,
    })
}
```

### 4d. `run_loop` signature — add the new params (replace the fn signature at ~line 89)

```rust
fn run_loop(
    core_path: String,
    rom_path: String,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    keyframe_req: Arc<AtomicBool>,
    mut action_rx: mpsc::UnboundedReceiver<AgentAction>,
    mut save_rx: mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<Option<Vec<u8>>>>,
    mut load_rx: mpsc::UnboundedReceiver<(Vec<u8>, tokio::sync::oneshot::Sender<bool>)>,
    battle: Arc<Mutex<Option<BattleState>>>,
) -> anyhow::Result<()> {
```

### 4e. State locals — declare next to `held` (~line 124)

```rust
    let mut held: HashSet<String> = HashSet::new();
    // NEW: action macro player + menu state machine.
    let mut taps = TapMachine::default();
    let mut menu = MenuPhase::Overworld;
```

### 4f. Inside the loop — drain savestate cmds + agent actions + drive the macro

Place this block at the **top of the loop**, before step 1 (input drain). Savestate cmds run on the
emu thread (the only owner of `N64`), satisfying the integration doc's central constraint.

```rust
        // 0a. Savestate commands (cross async/sync via oneshot, like existing channels).
        while let Ok(reply) = save_rx.try_recv() {
            let blob = emu.save_state();
            let _ = reply.send(blob);
        }
        while let Ok((data, reply)) = load_rx.try_recv() {
            let ok = emu.load_state(&data);
            if ok {
                taps.clear(&emu); // abandon any in-flight macro; we just teleported state
            }
            let _ = reply.send(ok);
        }

        // 0b. Queue any new agent actions (only meaningful while the action menu is up).
        while let Ok(action) = action_rx.try_recv() {
            taps.enqueue(&action);
        }

        // 0c. Advance the tap macro ONE frame (sets/clears PAD bits via emu.set_button).
        taps.tick(&emu);
```

### 4g. After `emu.clock_frame()` (step 3, ~line 169) — refresh the snapshot + menu phase

```rust
        // 3. Advance one frame.
        emu.clock_frame();

        // 3c. NEW: refresh the battle snapshot from WRAM (negligible vs VP8 encode).
        let busy = taps.busy();
        let snap = emu.with_system_ram(|ram| {
            menu = crate::battle::next_menu_phase(menu, ram, busy);
            crate::battle::read_battle_state(ram, menu)
        });
        if let Some(s) = snap {
            *battle.lock().unwrap() = Some(s);
        }
```

That is the entire pipeline change: agent actions reuse the exact `emu.set_button` path the browser
uses (no new input mechanism), the snapshot is published every frame, and save/load run on the emu
thread.

---

## 5. `src/signaling.rs` HTTP API + wiring

### 5a. `main.rs` — register the module (one line, ~line 9)

```rust
mod battle; // ADD alongside the existing `mod n64;` etc.
```

No other `main.rs` change: `AppState { api, inner }` is built from `inner` exactly as today, and
`inner` now carries the new channels.

### 5b. `signaling.rs` — imports + routes

```rust
use axum::body::Bytes;          // ADD
use axum::routing::{get, post};  // CHANGE: was `use axum::routing::post;`
```

Replace `router()`:

```rust
pub fn router(state: AppState) -> Router {
    let static_service = ServeDir::new("static").append_index_html_on_directories(true);
    Router::new()
        .route("/offer", post(offer_handler))
        .route("/battle/state", get(battle_state_handler))
        .route("/battle/action", post(battle_action_handler))
        .route("/battle/save", post(battle_save_handler))
        .route("/battle/load", post(battle_load_handler))
        .fallback_service(static_service)
        .with_state(state)
}
```

### 5c. Handlers (append to `signaling.rs`)

```rust
// GET /battle/state -> the latest snapshot the emulator thread published.
async fn battle_state_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::battle::BattleState>, (StatusCode, String)> {
    match state.inner.battle.lock().unwrap().clone() {
        Some(st) => Ok(Json(st)),
        None => Err((StatusCode::SERVICE_UNAVAILABLE, "no battle state yet".into())),
    }
}

// POST /battle/action  body: {"type":"move","slot":0} (also switch/run/buttons). 202 = queued.
async fn battle_action_handler(
    State(state): State<AppState>,
    Json(action): Json<crate::battle::AgentAction>,
) -> StatusCode {
    let _ = state.inner.action_tx.send(action); // executor plays it over frames
    StatusCode::ACCEPTED
}

// POST /battle/save -> serialize on the emu thread, write states/battle.state, return the blob.
async fn battle_save_handler(
    State(state): State<AppState>,
) -> Result<Bytes, (StatusCode, String)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.save_tx.send(tx);
    match rx.await {
        Ok(Some(buf)) => {
            // best-effort canonical copy on disk so `POST /battle/load` with no body could reuse it
            let _ = std::fs::create_dir_all("states");
            if let Err(e) = std::fs::write("states/battle.state", &buf) {
                tracing::warn!("battle.state write failed: {e}");
            }
            Ok(Bytes::from(buf))
        }
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "serialize failed".into())),
    }
}

// POST /battle/load  body: raw savestate bytes; if empty, load states/battle.state from disk.
async fn battle_load_handler(State(state): State<AppState>, body: Bytes) -> StatusCode {
    let data = if body.is_empty() {
        match std::fs::read("states/battle.state") {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("no battle.state on disk: {e}");
                return StatusCode::NOT_FOUND;
            }
        }
    } else {
        body.to_vec()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.load_tx.send((data, tx));
    match rx.await {
        Ok(true) => StatusCode::OK,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

Same-origin (no CORS, matching the existing `/offer`), synchronous to the emu thread via the
oneshot channels drained in §4f. `AppState` is reused unchanged.

---

## 6. CCDD EXPERIMENT CONCLUSION + RECOMMENDATION

**CCDD experiment result (verified, `research/battle-reach-and-ccdd.md` §4):** within a turn the
player's move locks into `CCDC` (~frame 130), then **the enemy AI writes its move into `CCDD`
(~frame 141)**, then moves execute (~frame 331), then `CCD5` (turns) increments. Writing `CCDD`
**before** ~f141 is **overwritten** by the AI pick. Writing `CCDD` **after** the AI pick (and
before execution) **STICKS and the enemy executes that move** — proven decisively: injecting
`CCDD=33` (TACKLE) after the pick made the player take 3 damage; the control `CCDD=39` (TAIL WHIP)
did 0. So `CCDD` is the live source of truth for the enemy's executed move, but only inside a
narrow post-AI/pre-execution window. To drive the enemy you must poll for the single `0→nonzero`
transition each turn and overwrite once (there is a comfortable ~190-frame window).

**Recommendation: `savestate + injection` (input-macro for the player; optional poll-and-overwrite
of `CCDD` only if an agent must also control the enemy).**

- Bootstrap each match by `retro_unserialize` of a canonical battle blob (`states/battle.state`).
- Read battle state per-frame from WRAM (BE).
- Drive the *player's* move with the **input macro** (§3) — legit menu navigation keeps all the
  invisible machine state consistent; strictly safer than poking `CCDC`.
- If you want a 1-agent-controls-both or scripted-enemy mode, add CCDD poll-and-overwrite **inside
  the emu thread** (it already owns `N64` and runs every frame — detect `CCDD` 0→nonzero, write the
  agent's enemy move once per `CCD5`). This needs **no ROM patch**.

**Why not the alternatives:**
- `minimal-rom-patch` (patch `AIChooseMove`/the write to `wEnemySelectedMove`): cleaner than racing
  the poll, but introduces a forked ROM, breaks savestate semantics vs the vanilla blob, and is
  unnecessary — the poll-and-overwrite is proven to work and is reversible.
- `external-simulator` (reimplement Gen-1 battle math in Rust): loses ground-truth fidelity (status
  quirks, the famous Gen-1 crit/badge bugs, RNG) and throws away the whole point of running the
  real ROM. Only justified if you needed millions of headless battles/sec with no rendering.

---

## 7. BUILD / RUN + HOW AN AGENT PLAYS

### Build & run
```bash
cd ~/pokemon-pvp-red
cargo build --release
# Grayscale DMG ROM (battle RAM identical to the color hack):
cargo run --release -- "Pokemon Red.gb"
#   (default core = cores/gambatte_libretro.dylib; default ROM = the .gbc color hack)
# open http://localhost:3000 to watch the stream
```

### Start a battle from the captured savestate
```bash
# Load the verified rival-battle start (or POST raw bytes; empty body uses states/battle.state):
curl -X POST http://localhost:3000/battle/load --data-binary @states/battle.state
```

### Agent loop: GET state -> pick move -> POST action -> repeat
```bash
# 1) Read state
curl -s http://localhost:3000/battle/state | jq
#   { "in_battle":2, "menu":"MainMenu", "turns_in_battle":0,
#     "player":{"hp":20,"max_hp":20,"moves":[10,45,0,0],...},
#     "enemy":{"hp":20,"max_hp":20,...}, ... }

# 2) Decide (agent logic), then act — use move in slot 0 (SCRATCH):
curl -X POST http://localhost:3000/battle/action \
     -H 'content-type: application/json' -d '{"type":"move","slot":0}'   # -> 202

# 3) Poll until the macro finishes and the turn resolves (menu back to MainMenu,
#    turns_in_battle incremented), then repeat from step 1.
```

Agent decision rule of thumb: only `POST /battle/action` when `in_battle != 0` **and**
`menu == "MainMenu"` (action menu up, no macro in flight). Other action shapes:
`{"type":"switch","slot":2}`, `{"type":"run"}`, `{"type":"buttons","presses":["A","Down","A"]}`.

### Win / loss detection
- **Win:** enemy fainted = `enemy.hp == 0`. Match over when the enemy *party* is exhausted and the
  engine leaves battle = `in_battle == 0` (`D057 == 0`) after the faint.
- **Loss:** `player.hp == 0` and, after the engine resolves, `in_battle == 0`.
- **Authoritative "battle ended":** poll `in_battle` (`D057`); `0` means the battle is fully over
  (this is the master flag — derive higher-level win/loss from which side hit 0 HP first).
- For reproducible evaluation: `POST /battle/load` the same blob to reset, log per-frame inputs
  (determinism verified) to replay any match exactly.

---

## 8. RISKS

1. **HP/stat endianness (highest impact).** Gen-1 HP/MaxHP/Attack/Defense/Speed/Special are
   **BIG-ENDIAN**. The reader uses `from_be_bytes`; a little-endian read gives garbage (`D015=20`
   becomes `5120`). The `battle.rs` unit test `hp_is_big_endian` pins this. Cross-checked live
   (Charmander/Squirtle HP=20).
2. **Action-macro timing / menu state.** Taps assume the cursor starts on FIGHT each turn (Gen-1
   *remembers* the last cursor cell). `hold=4/gap=6` frames is reliable but heuristic. The
   `MenuPhase` machine is a software approximation (no single "move list open" RAM flag). v2
   hardening: read `wCurrentMenuItem` (`CC26`) + `wTopMenuItemY/X` (`CC24`/`CC25`) and prepend Up/Left
   homing taps; gate exactly one action per `CCD5` increment.
3. **Savestate portability across core versions.** Blobs are portable across *instances of the same
   gambatte build* (verified). A different/upgraded gambatte may change the 59650-byte layout and
   reject old blobs — `load_state` returns `false` (handled: handler 500s). Pin the core dylib used
   to capture states; re-capture if you bump `cores/gambatte_libretro.dylib`.
4. **`.gb` grayscale vs `.gbc` color — battle RAM is IDENTICAL.** The color hack uses the same
   WRAM battle/party layout; addresses and endianness are unchanged. The captured `states/battle.state`
   was made on the grayscale `Pokemon Red.gb`; if you run the `.gbc` default ROM the *core's
   savestate layout* may differ (different ROM → potentially different blob), so capture a fresh
   start state for whichever ROM you actually run. The *RAM reader* works for both.
5. **HRAM `FFF3` is unreachable.** gambatte exposes only WRAM (id 2, 8192 B) + cart SRAM (id 0); the
   battle-turn HRAM byte is not in the memory API. The design does not depend on it — turn/phase is
   derived from `D057` + `CCD5` + the `MenuPhase` machine.
6. **`D057`/`D05A` label and enemy-struct off-by-one.** This plan follows pokered's `wIsInBattle=D057`,
   `wBattleType=D05A`, and enemy anchors `CFE6`/`CFE8`/`CFED`/`CFF4`/`CFFE`. Verified live in the
   rival battle. If a future ROM build shows bytes swapped, flip per the runtime probe (one wild
   battle: `D057` should read `1`).
7. **Mid-turn WRAM writes are racy.** `inject_*` writes are for *setup* — do them while
   `menu == Idle`/`MainMenu` or immediately after `load_state`, never while the engine is resolving
   a turn.
8. **Live WRAM pointer validity.** `get_mem_data` is re-called at every read (the closure-scoped
   slice never outlives the borrow) in case the core relocates the buffer.

---

### File-change summary

| File | Change |
|---|---|
| `src/n64.rs` | +5 symbols, +2 mem-id consts, +5 struct fields, resolve in `new`, +`with_system_ram`/`with_system_ram_mut`/`save_state`/`load_state` (§2). |
| `src/battle.rs` | **NEW** — `BattleState`/`BattlePokemon`/`BattleStats`/`AgentAction`/`MenuPhase`/`Tap`/`TapMachine`, `read_battle_state`, `inject_*`, `action_to_taps`, `next_menu_phase`, tests (§3). |
| `src/pipeline.rs` | Extend `AppInner`, new channels in `start`, new `run_loop` params, drain save/load + actions, `taps.tick`, refresh snapshot after `clock_frame` (§4). |
| `src/signaling.rs` | +`get`/`Bytes` imports, 4 routes, 4 handlers (§5). |
| `src/main.rs` | +`mod battle;` (one line). |
