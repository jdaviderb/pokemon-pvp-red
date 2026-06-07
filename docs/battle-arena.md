# AI Battle Arena — Pokémon Red

Turn the server-side Pokémon Red emulator into an **AI-agent battle playground** over HTTP. The
**real Gen-1 battle engine** runs (no reimplementation); an external agent reads the battle state
from WRAM and chooses actions, which are executed by driving the in-battle menu via input.

> Run the server with the matching ROM: `cargo run --release -- "Pokemon Red.gb"`.
> The savestates are captured against `Pokemon Red.gb` (the default `.gbc` color hack would differ).

---

## 1. Memory model

The libretro core (`gambatte`) exposes the Game Boy's **work RAM** as
`RETRO_MEMORY_SYSTEM_RAM` (id 2) = **8 KiB**, CPU `0xC000..0xE000`. A CPU address `A` maps to
`SYSTEM_RAM[A - 0xC000]`. `src/libretro.rs` exposes it via `with_system_ram` / `with_system_ram_mut`.

- **All Gen-1 multi-byte values (HP, max HP, Attack/Defense/Speed/Special, exp) are BIG-ENDIAN**
  (high byte at the lower address): `u16::from_be_bytes([ram[o], ram[o+1]])`. This is the #1
  reader/writer bug; a little-endian read of HP=20 yields 5120.
- **HRAM is NOT exposed** (e.g. `FFF3` battle-turn). Turn/phase is derived from WRAM
  (`D057`, `CCD5`) + a small software `MenuPhase` state machine.

Authoritative addresses: see [`pokemon-red-ram-map.md`](pokemon-red-ram-map.md).

---

## 2. Savestates (bootstrap)

A battle is whole-machine state (CPU PC, RNG, text/menu engine, banking, …) — you **cannot** start
one by writing a few RAM bytes. Instead we **bootstrap from a savestate** captured inside a battle
(`retro_serialize`/`retro_unserialize`, ~59650 bytes for this ROM, deterministic & portable across
runs of the same core build).

| File | Captured at | Used by |
|---|---|---|
| `states/battle.state` | the **FIGHT menu** of a rival battle (Charmander vs Squirtle) | `POST /battle/load` |
| `states/legendary_intro.state` | the **battle intro**, pre-send-out (`D057=2, D014=0, CFE5=0`) | `POST /battle/setup` |

`states/` is **gitignored** (game-derived, ROM-specific). To capture your own: play to the FIGHT
menu in the browser, then `POST /battle/save`. The intro state is recaptured the same way, at the
pre-send-out battle intro.

---

## 3. HTTP API

| Method | Path | Body | Result |
|---|---|---|---|
| `GET` | `/battle/state` | — | `BattleState` JSON (`503` until the first snapshot) |
| `POST` | `/battle/action` | see §5 | `202 Accepted` (queued; played over frames) |
| `POST` | `/battle/save` | — | savestate blob; also writes `states/battle.state` |
| `POST` | `/battle/load` | raw bytes or empty | `200` / `404` (no file) / `500` |
| `GET` | `/battle/species` | — | `[[index,"NAME"],...]` |
| `POST` | `/battle/setup` | `{"player":74,"enemy":75,"level":50}` | `200` live / `400` reason |

---

## 4. `BattleState` JSON

```jsonc
{
  "in_battle": 2,            // D057: 0 none, 1 wild, 2 trainer, 255 special
  "battle_type": 0,          // D05A
  "turns_in_battle": 1,      // CCD5
  "player_selected_move": 10,// CCDC (move id)
  "enemy_selected_move": 33, // CCDD (move id)
  "menu": "MainMenu",        // software phase: Overworld|BattleIntro|MainMenu|MoveList|Animating|Idle
  "player": {                // wBattleMon (D009..)
    "species": 74, "level": 50, "hp": 165, "max_hp": 165, "status": 0,
    "moves": [58,59,64,97], "pp": [10,5,35,30],
    "stats": { "attack": 105, "defense": 120, "speed": 105, "special": 145 }
  },
  "enemy": { /* wEnemyMon (CFE5..), same shape */ },
  "cur_map": 0, "x": 0, "y": 0
}
```

Field → address mapping is in `src/battle.rs::read_battle_state` (player block `D009..D030`, enemy
block `CFE5..D008`). HP/stats use `from_be_bytes`.

---

## 5. Actions (`POST /battle/action`)

```jsonc
{"type":"move","slot":0}                 // FIGHT -> move 0..3   (A, Down×slot, A)
{"type":"switch","slot":2}               // PKMN  -> party slot
{"type":"run"}                           // RUN (wild only)
{"type":"buttons","presses":["A","Down","A"]}   // raw taps (scripting / advance text)
```

Actions become **button taps** (`TapMachine` in `battle.rs`): each tap holds a button ~4 frames,
releases ~6. They're pushed through `emu.set_button` — the **same PAD path** the browser uses — so
the original engine state stays consistent (no engine poking). The macro assumes the cursor starts
on FIGHT at turn start (true for the captured menu state).

---

## 6. Agent loop

```sh
curl -XPOST localhost:3000/battle/load                      # bootstrap a battle
while true; do
  S=$(curl -s localhost:3000/battle/state)
  # decide from S (player/enemy hp, moves, ...), then act on slot N:
  curl -XPOST localhost:3000/battle/action -H 'content-type: application/json' -d '{"type":"move","slot":0}'
  # poll until the turn resolves; if result text waits, advance it:
  curl -XPOST localhost:3000/battle/action -H 'content-type: application/json' -d '{"type":"buttons","presses":["A"]}'
done
```

- Only act when `in_battle != 0` **and** `menu == "MainMenu"` (no macro in flight).
- **Status-move / result text may pause for an A** — send `{"type":"buttons","presses":["A"]}` to
  advance until `turns_in_battle` increments and `menu` returns to `MainMenu`.
- **Win/Loss**: `enemy.hp == 0` (win) / `player.hp == 0` (loss); the battle is fully over when
  `in_battle == 0`.
- **Reproducibility**: `retro_run()` is deterministic, so `POST /battle/load` + a fixed action log
  replays a match exactly.

Verified: from the FIGHT menu, `{"type":"move","slot":0}` (SCRATCH) executed in-engine → enemy
HP `20→17`, `turns_in_battle 0→1`.

---

## 7. Custom matchup — pick the two Pokémon (`/battle/setup`)

`POST /battle/setup {"player":<idx>,"enemy":<idx>,"level":N}` spawns a chosen 1-v-1 with **correct
sprites, names, and cries**. On the emu thread it:

1. `load_state("states/legendary_intro.state")` — teleport to the pre-send-out intro
   (validated: `D057!=0 && D014==0 && CFE5==0`, else `400`).
2. `setup_matchup(ram, player, enemy, level)` — write **full 44-byte party structs** for both sides
   (`build_party_mon`) + species lists + counts + the player **nickname** (mandatory, else stale
   on-screen name), at the verified offsets:
   - Player: count `D163`, species `D164`, mon `D16B`, nick `D2B5`.
   - Enemy: count `D89C`, species `D89D`, mon `D8A4`, nick `D9EE`.
3. Tap `A` through the send-out → the engine draws the injected species (proven: the enemy is built
   from WRAM by `LoadEnemyMon`, not re-read from ROM, so the injection sticks).

**Species table** (`SPECIES` in `battle.rs`; values are Gen-1 **INTERNAL indices**, not Pokédex):

| Name | index | base HP/Atk/Def/Spd/Spc | type | Lv50 HP / stats |
|---|---|---|---|---|
| Articuno | 0x4A (74) | 90/85/100/85/125 | Ice/Flying | HP 165 · 105/120/105/145 |
| Zapdos | 0x4B (75) | 90/90/85/100/125 | Electric/Flying | HP 165 · 110/105/120/145 |
| Moltres | 0x49 (73) | 90/100/90/90/125 | Fire/Flying | HP 165 · … |
| Dragonite | 0x42 (66) | 91/134/95/80/100 | Dragon/Flying | HP 166 · … |

**Gen-1 stat formula** (DV=15, stat-EXP=0): `stat = floor((base+15)*2*level/100) + 5`;
`HP = same + level + 10`. Add species by appending a `Gen1Species` row.

Verified: **Articuno vs Zapdos @ Lv50** renders with the real Zapdos/Articuno sprites, "Go!
ARTICUNO" nickname, HP 165, and the correct movesets.

---

## 8. Enemy control (CCDD) — advanced

The enemy's chosen move lives in `CCDD`. Writing it **overrides** the enemy move, but only in a
narrow window: the AI writes `CCDD` (~mid-turn), then the move executes. Writing `CCDD` **after**
the AI pick and **before** execution sticks (proven: injecting TACKLE made the player take damage).
To drive the enemy, poll for the `0→nonzero` transition each turn and overwrite once — **no ROM
patch needed**. The MVP drives only the player (via the menu macro); enemy override is an optional
add-on inside the emu thread.

---

## 9. Caveats

- ROM-specific savestates (run `Pokemon Red.gb`); `states/` gitignored.
- Gen-1 big-endian stats; HRAM not exposed.
- Menu macro timing is heuristic (cursor assumed on FIGHT; status-move text may need an extra A).
- Internal indices ≠ Pokédex numbers.
- Cosmetic: enemy `level`/`max_hp` may read slightly off (engine recomputes at send-out) — sprites,
  moves, and mechanics are correct.

Design provenance: `DESIGN-BATTLE.md` (see DOCS.md).
```
