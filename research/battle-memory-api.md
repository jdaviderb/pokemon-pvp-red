# Battle Memory + Savestate API — gambatte + Pokémon Red (VERIFIED)

Empirically verified against `cores/gambatte_libretro.dylib` loading the vanilla
`Pokemon Red.gb` (DMG, 1 MiB, header 0x143 = 0x00). All numbers below are REAL printed
output from a runnable harness.

- Probe source:  `/tmp/battlemem-work/src/main.rs` (copied/extended from `research/gb-harness`)
- Toolchain:     `RUSTUP_TOOLCHAIN=1.92` (rustc 1.92.0)
- Run command:
  ```
  cd /tmp/battlemem-work
  RUSTUP_TOOLCHAIN=1.92 cargo build --release
  ./target/release/gbprobe \
    ~/pokemon-pvp-red/cores/gambatte_libretro.dylib \
    "~/pokemon-pvp-red/Pokemon Red.gb"
  ```

> NOTE: this is a `/tmp` scratch copy. Port the symbol-loading + helpers into the real
> frontend (`src/n64.rs`) — the symbol signatures and constants below are exactly what to add.

---

## Symbol signatures (extern "C") — all present in gambatte and verified callable

```rust
// Memory access
retro_get_memory_data(unsigned id)  -> *mut c_void      // pointer to the live region
retro_get_memory_size(unsigned id)  -> usize            // region size in bytes

// Savestate
retro_serialize_size()               -> usize           // blob size
retro_serialize(*mut c_void, usize)  -> bool            // write blob; true on success
retro_unserialize(*const c_void, usize) -> bool         // restore blob; true on success

// Frame stepping + input (already in the harness)
retro_run() -> ()
retro_set_input_state(cb: extern "C" fn(port,device,index,id)->i16)
```

Rust `libloading` declarations (drop straight into the frontend):
```rust
let get_mem_data:  Symbol<unsafe extern "C" fn(c_uint) -> *mut c_void> = lib.get(b"retro_get_memory_data")?;
let get_mem_size:  Symbol<unsafe extern "C" fn(c_uint) -> usize>       = lib.get(b"retro_get_memory_size")?;
let serialize_size:Symbol<unsafe extern "C" fn() -> usize>            = lib.get(b"retro_serialize_size")?;
let serialize:     Symbol<unsafe extern "C" fn(*mut c_void, usize) -> bool>   = lib.get(b"retro_serialize")?;
let unserialize:   Symbol<unsafe extern "C" fn(*const c_void, usize) -> bool> = lib.get(b"retro_unserialize")?;
```

## Memory-id constants
```rust
const RETRO_MEMORY_SAVE_RAM:   c_uint = 0;
const RETRO_MEMORY_RTC:        c_uint = 1;
const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;   // <-- GB WRAM, use this for battle/party RAM
const RETRO_MEMORY_VIDEO_RAM:  c_uint = 3;
```

---

## TASK 1 — memory regions (REAL output)

```
SAVE_RAM   (id=0): size=32768 bytes  ptr=0x83b61c000
RTC        (id=1): size=0 bytes
SYSTEM_RAM (id=2): size=8192 bytes  ptr=0x83b624000   (== 8192 = GB WRAM 0xC000-0xDFFF) ✓
VIDEO_RAM  (id=3): size=0 bytes  ptr=0x0
mem id 4 -> size 0 (unavailable)
mem id 5 -> size 0 (unavailable)
```

- **SYSTEM_RAM = exactly 8192 bytes** = the 8 KiB GB WRAM `0xC000–0xDFFF`. CONFIRMED.
- SAVE_RAM = 32768 bytes (cart SRAM, 4 banks of 8 KiB; Pokémon Red uses MBC3 32 KiB SRAM —
  this is the on-cart save, NOT live game state, so it is **not** where battle RAM lives).
- VIDEO_RAM / RTC / ids 4,5 are **not exposed** by gambatte (size 0, null ptr).

### WRAM base mapping (THE key fact)
```
GB CPU address A in 0xC000..=0xDFFF   ==>   SYSTEM_RAM[A - 0xC000]
```
Use this for every address in the Pokémon Red RAM map (they are all in 0xCxxx/0xDxxx).
E.g. party count D163 -> `SYSTEM_RAM[0xD163-0xC000]` = `SYSTEM_RAM[0x1163]` (= index 4451).

---

## TASK 2 — boot + read battle/party RAM (REAL output)

Booted with ~1100 frames (120 idle warm + 980 of mashing Start/A/B) from cold boot.
Raw bytes read via `SYSTEM_RAM[addr-0xC000]`:

```
party count D163             [D163..D163] = 00
party species D164..         [D164..D169] = ff 00 00 00 00 00
enemy count D89C             [D89C..D89C] = 00
wIsInBattle D057             [D057..D057] = 00
battle type D05A             [D05A..D05A] = 00
turns CCD5                   [CCD5..CCD5] = 00
wCurMap D35E                 [D35E..D35E] = 26   (= map id 0x26)
wYCoord D361                 [D361..D361] = 06
wXCoord D362                 [D362..D362] = 03
player active HP D015        [D015..D016] = 00 00
enemy active HP CFE6         [CFE6..CFE7] = 00 00
plausibility: party_count=0 (0..6 plausible: true), wCurMap=38
```

Values are PLAUSIBLE, not garbage: party_count=0 / wIsInBattle=0 are correct for a state that
has not yet started a battle or received a starter; wCurMap=0x26 with small Y/X coords (6,3)
are sane overworld coordinates. The species list begins `ff` (party terminator) which is the
correct empty-party sentinel. This confirms the `addr-0xC000` indexing lands on the documented
cells. (The boot-mash lands on the new-game intro/name area, so no party is created yet — to
reach an actual battle you must script the full new-game menu flow or, far simpler, **boot from
a savestate already in/near a battle** — see Task 4.)

### Write test (REAL output)
```
D163 before=0 after write of 3 -> 3  (write WORKS)
D163 after one retro_run() = 3
```
**Writing `SYSTEM_RAM[0xD163-0xC000] = 3` reads back as 3, and survives a `retro_run()`** —
this is definitive proof the slice aliases the core's live WRAM cell (read AND write). This is
how the action API can poke selected-move bytes (CCDC) etc. directly if desired.

---

## TASK 3 — HRAM (0xFF80–0xFFFE, incl. FFF3 battle-turn) reachability

```
SYSTEM_RAM size = 8192 bytes. HRAM is 127 bytes at 0xFF80.
FFF3 - 0xC000 = 0x3FF3 = 16371 (>= 8192 => OUT of SYSTEM_RAM range).
mem id 0..5 scan: only id 0 (32768) and id 2 (8192) are non-null.
```

**HRAM is NOT reachable via `retro_get_memory_data` for gambatte.** Only WRAM (id 2) and cart
SRAM (id 0) are exposed. Address 0xFFF3 is far past the 8 KiB WRAM window and there is no
flat-64KiB / HRAM region exposed under any id 0..5.

Implications for the battle reader:
- **FFF3 (HRAM battle-turn) cannot be read live via the memory API.** It IS captured inside the
  savestate blob (full machine state), but parsing it out of the opaque gambatte blob is fragile.
- Practical recommendation: **don't depend on FFF3.** The WRAM battle-state fields are sufficient:
  `D057` (wIsInBattle), `CCD5` (turns-in-battle), `CCDC`/`CCDD` (player/enemy selected move),
  active-mon HP at `D015/CFE6`, etc. are all in WRAM and fully readable. Derive "whose turn"
  from CCD5/selected-move bytes and the menu/animation state rather than HRAM FFF3.

---

## TASK 4 — savestate round-trip (REAL output)

```
retro_serialize_size() = 59650 bytes
retro_serialize() -> true
state blob FNV1a = 0x3dbd603d8889012f
WRAM hash: snapshot=0xe61132a24af00435  after mutate+30 frames=0x141e5d7dc85f031a (changed: true)
retro_unserialize() -> true
WRAM hash after unserialize = 0xe61132a24af00435  (== snapshot: true)   ✓ REVERTED
serialize_size again = 59650 (stable: true)                            ✓ STABLE
wrote state blob to /tmp/gbprobe/pkred_state.bin (59650 bytes)
```

- `retro_serialize_size()` = **59650 bytes**, STABLE across calls and after restore.
  (Far larger than WRAM+SRAM exposed regions => the blob is full machine state incl. HRAM,
  CPU regs, PPU/APU, MBC banking — everything needed for an exact resume.)
- Round-trip works: serialize, scribble RAM + advance 30 frames (hash changes), unserialize,
  and **WRAM hash returns exactly to the snapshot value** — full revert confirmed.

### TASK 4b — cross-instance reload in a FRESH core (REAL output)
```
fresh core serialize_size = 59650 ; saved blob = 59650 (match: true)
fresh core retro_unserialize(saved blob) -> true
fresh core WRAM hash after reload = 0xe61132a24af00435
D163 (party count) in fresh core after reload = 3
```
A blob written to `/tmp/gbprobe/pkred_state.bin`, then loaded into a **brand-new `Library` +
`retro_init` + `retro_load_game` instance**, restores byte-identical WRAM (and even shows the
`D163=3` we had poked before saving). **State blobs are portable across core instances** —
this is the mechanism for the arena: snapshot a "ready-to-battle" state once, then reset every
match by `retro_unserialize` of that blob.

> ARENA WORKFLOW: serialize_size is fixed at 59650, so pre-allocate one `[u8; 59650]` buffer.
> Save a canonical "battle start" blob; each new agent battle = unserialize that blob, then drive
> moves with input + retro_run, reading WRAM each turn.

---

## TASK 5 — determinism (REAL output)

```
replay1 WRAM hash = 0xa1555158425a5d57
replay2 WRAM hash = 0xa1555158425a5d57
DETERMINISTIC (same inputs from same state -> same RAM): true   ✓
```

From an identical savestate, replaying the **same fixed input script** (press-A every other
4-frame block, 60 frames) twice yields **byte-identical WRAM**. `retro_run()` is deterministic
given (savestate, input sequence). Reproducible battles ARE achievable: pin a start blob + log
the per-frame joypad bitmask and you can replay any match exactly. (gambatte is a cycle-accurate
deterministic core; no audio/threading nondeterminism observed in WRAM.)

---

## Summary table

| Item | Result |
|---|---|
| SYSTEM_RAM (id 2) size | **8192** (GB WRAM 0xC000–0xDFFF) |
| SAVE_RAM (id 0) size | 32768 (cart SRAM; not live state) |
| VIDEO_RAM / RTC / id 4,5 | not exposed (size 0) |
| WRAM mapping | `SYSTEM_RAM[A - 0xC000]` for A in 0xC000..0xDFFF |
| WRAM read | plausible, correct (party=0, map=0x26, coords 6/3) |
| WRAM write | works (D163:=3 reads back 3, survives a frame) |
| HRAM (FFF3) live read | NOT reachable via memory API — use WRAM fields instead |
| serialize_size | **59650**, stable |
| serialize / unserialize | both return true; full WRAM revert confirmed |
| cross-instance reload | works (byte-identical WRAM in fresh core) |
| determinism | YES — same state+inputs => identical RAM |

## What to wire into the real frontend (`src/n64.rs`)
1. Load the 5 symbols above once after `retro_load_game`.
2. Battle-state reader: `let wram = slice::from_raw_parts(get_mem_data(2) as *const u8, 8192);`
   then read fields as `wram[addr - 0xC000]` (HP/level/moves are little-endian u16 pairs where 2 bytes).
3. Action API: set the joypad bitmask in the input-state callback, call `retro_run()` N frames to
   execute a menu navigation that selects the chosen move (preferred over poking CCDC directly,
   so the engine stays consistent); read WRAM after each turn.
4. Match reset / reproducibility: keep one canonical 59650-byte "battle start" blob; `unserialize`
   it to start each match. Log per-frame inputs for exact replay.
