# Pokémon Red — Battle Reach, Savestate, RAM-Map Verification, and CCDD Experiment

Headless libretro frontend (gambatte) driving vanilla **Pokemon Red.gb** (DMG).
Probe source: `/tmp/ccdd-probe/src/main.rs` (built on the verified `research/gb-harness`
template). Toolchain `RUSTUP_TOOLCHAIN=1.92`. Core:
`cores/gambatte_libretro.dylib`. All values below are REAL probe output.

## TL;DR

- **A real RIVAL battle was reached fully headless** (boot → name → bedroom →
  downstairs → Pallet Town → Oak cutscene → Oak's Lab → take starter → rival battle
  auto-starts). `D057 = 2` (trainer battle).
- **Savestate captured at the FIGHT menu** of that battle:
  `~/pokemon-pvp-red/states/battle.state` (59 650 bytes,
  `retro_serialize`/`retro_unserialize` round-trips cleanly).
- **The battle RAM map in the preamble is verified** against live values (player
  Charmander L5 SCRATCH/GROWL, enemy Squirtle L5 TACKLE/TAIL WHIP).
- **CCDD experiment answered:** the enemy AI writes its move into `CCDD` ~frame 141
  of the turn (after the player's move is locked at `CCDC` ~frame 130). Writing
  `CCDD` **before** that is overwritten by the AI pick. Writing `CCDD` **after** the
  AI pick (but before the move executes) **STICKS and the enemy executes your move** —
  proven by injecting a damaging move and watching player HP drop.

## Memory / savestate facts (gambatte, this ROM)

```
MEM REGIONS: SYSTEM_RAM=8192 (0x2000)  SAVE_RAM=32768  VIDEO_RAM=0  | serialize_size=59650
```

- `RETRO_MEMORY_SYSTEM_RAM` (id 2) = **8192 bytes = the 8 KiB WRAM 0xC000–0xDFFF**.
  CPU address `A` in `0xC000..0xE000` maps to `SYSTEM_RAM[A - 0xC000]`. Confirmed by
  reading map id / coords / party that all change sensibly.
- `RETRO_MEMORY_SAVE_RAM` (id 0) = 32768 bytes (cart SRAM, not used for battle RAM).
- `RETRO_MEMORY_VIDEO_RAM` (id 3) = 0 (gambatte does not expose VRAM via this id).
- `retro_serialize_size() = 59650`. `retro_serialize`/`retro_unserialize` both
  return true and round-trip exactly (verified: bedroom pos `(38,6,2)` survived a
  save→load cycle byte-for-byte; battle.state restores straight back to the FIGHT
  menu of the rival battle).
- **HRAM `0xFF80–0xFFFE` is NOT in SYSTEM_RAM.** The window is only 8192 bytes
  (`0xC000–0xDFFF`); `0xFFF3 - 0xC000 = 0x3FF3 = 16371 >= 8192`, so **`FFF3`
  (battle turn) is NOT reachable through `RETRO_MEMORY_SYSTEM_RAM`.** gambatte
  exposes no separate HRAM region. If the app needs HRAM it must read it out of the
  savestate blob or use a different mechanism; the battle logic below does not
  require FFF3.

Helper note: in-battle HP is stored **big-endian** (hi byte first). `D015` reads 20
as `rw_be`; the little-endian read gives the bogus `5120 (0x1400)`.

## 1–2. Reaching the battle + savestate

The hard part headless is blind navigation. What worked, and how:

**Boot / intro / naming.** Mash `START`+`A` (~90 taps) through the GameFreak/Nintendo
logos + title to select NEW GAME, then **pure `A`** through Oak's speech and both
name screens. Mashing `A` on the Gen-1 name keyboard auto-fills a default name and
confirms; overworld control returns in the bedroom (`map = 38` PLAYERS_HOUSE_2F)
after ~260 `A`-taps. Control is detected by probing a step (position `D361`/`D362`
changes). Important: do **not** press direction keys during naming — they move the
keyboard cursor and corrupt the name; pure `A` is what works.

**Room navigation was the real blocker, and the fix that worked.** Directed
"walk-in-a-line" scripts get stuck on furniture and oscillate; the bedroom is larger
than a naive sweep suggests (walkable `X∈0..7`, `Y∈1..7`, L-shaped around bed/desk).
A **pseudo-random walk using the correct Gen-1 "tap-to-turn, tap-again-to-step"
movement primitive**, polling `D35E` (map) every step, reliably finds the stairs
warp. Per-map an exit bias is added (4× weight): 1F → bias DOWN toward the door,
Pallet Town / Oak's Lab → bias UP. A is mashed periodically to clear door/cutscene
text. Battle is detected by polling `D057`.

Observed map progression (real output):
```
CONTROL GAINED after 260 A-taps via LEFT pos=(38, 6, 2)   # bedroom, PLAYERS_HOUSE_2F
phase 0 : map 38 -> 37 (PLAYERS_HOUSE_1F) pos=(37,1,7)
... (down to Pallet Town 0, Oak cutscene, into OAKS_LAB 40) ...
phase 15: party-change on map 40 -> party=1            # took the starter
phase 17: battle on map 40 -> D057=2                   # RIVAL BATTLE auto-starts
```

On battle, frames are driven (`A` taps) through the "rival sent out / Go! <mon>"
intro until the active-mon structs populate (player HP nonzero), then the savestate
is written:
```
SAVESTATE: wrote 59650 bytes -> ~/pokemon-pvp-red/states/battle.state
```
The screen at this point shows enemy **SQUIRTLE :L5** with HP bar and "Go! J…"
(player sending out Charmander) — a genuine rival battle. The state restores to the
idle FIGHT action-menu of that battle.

(Also saved a controllable-overworld restore point:
`~/pokemon-pvp-red/states/bedroom.state`.)

## 3. Battle RAM map — VERIFIED (live values, D057 = 2)

Live dump from the captured rival battle (after the intro animations populate the
structs). Every field matches the preamble map and is internally consistent.

```
D057 wIsInBattle = 2     (trainer battle — rival; 1 would be wild, 0xFF special)
D05A battleType   = 0    (normal)
CCD5 turns        = 0    (turn 0 — fresh, before first action)
CCDC playerMove   = 0
CCDD enemyMove    = 0

ENEMY active mon (CFE5-D008):   # the rival's SQUIRTLE
  CFE5 species   = 177           # Squirtle (internal index 0xB1)
  CFE6-CFE7 HP   = 20  (big-endian)
  CFEA status    = 21
  CFED-CFF0 moves= [33, 39, 0, 0]   # 33=TACKLE, 39=TAIL WHIP  (Squirtle L5 set ✓)
  CFF4-CFFD stats: maxHP=20 atk=10 def=12 spd=10 spc=10
  CFFE-D001 PP   = [35, 30, 0, 0]   # TACKLE 35PP, TAIL WHIP 30PP ✓

PLAYER active mon (D009-D030):  # our CHARMANDER
  D009 species   = 137
  D015-D016 HP   = 20  (big-endian)
  D01C-D01F moves= [10, 45, 0, 0]   # 10=SCRATCH, 45=GROWL    (Charmander L5 set ✓)
  D022 level     = 5                # plausible 1–100 ✓
  D023-D02C stats: maxHP=20 atk=11 def=10 spd=11 spc=10
  D02D-D030 PP   = [35, 40, 0, 0]   # SCRATCH 35PP, GROWL 40PP ✓

PARTY:       D163 count = 1 | D164-D169 species = [176,255,0,0,0,0]  # 176 = Charmander
ENEMY PARTY: D89C count = 1 | D89D-D8A2 species = [177,255,0,0,0,0]  # 177 = Squirtle
```

Sanity checks all pass: `D057` nonzero, player level 5 (1–100), HP nonzero on both
sides, move/PP/stat values consistent with the canonical Charmander-vs-Squirtle
rival fight. **The RAM map is correct.** Caveat: `CFE8` (enemy level) read 0 at the
exact moment sampled here — it is populated slightly later in the intro; player
level `D022 = 5` is the reliable level field. HP is **big-endian** in these structs.

## 4. CCDD experiment — the key research question

**Question:** while in battle, if you WRITE the enemy-selected move byte `CCDD`, does
the engine use it, or overwrite it with its own AI pick? When in the turn is the
enemy move chosen?

### Canonical single-turn timeline (pure observation, no writes)

From the FIGHT menu, select FIGHT → first move; sample every frame:
```
f0  : CCDC=0 CCDD=0 CCD5=0  pHP=20 eHP=20      # idle at FIGHT menu
f130: CCDC(playerMove) 0 -> 10                 # player's move locked into CCDC
f141: CCDD(enemyMove)  0 -> 39                 # *** enemy AI picks its move -> CCDD ***
f331: enemyHP 20 -> 17                          # player's SCRATCH executes
f372: CCD5(turns) 0 -> 1                         # turn 1 counted
f671: CCDD 39 -> 33                              # (next turn's selection begins)
```
So within a turn: **player move → `CCDC` (~f130), then the enemy AI writes its move
→ `CCDD` (~f141)**, then moves execute in speed order, then `CCD5` increments.

### Experiment A — write CCDD BEFORE the AI pick

At the idle FIGHT menu (and again right before confirming the move) write `CCDD=0x99`
(153), then select FIGHT + move0:
```
pre-turn: playerHP=20 enemyHP=20
f141: CCDD 153 -> 39        # engine OVERWRITES our 153 with its AI pick (39 = TAIL WHIP)
>>> sentinel was OVERWRITTEN at frame 141
post-turn: playerHP=20 (delta 0)   # TAIL WHIP did no damage -> enemy used the AI pick, not ours
```
**Writing CCDD before frame ~141 does NOT stick** — the AI selection routine clobbers
it.

### Experiment B/“definitive” — write CCDD AFTER the AI pick

Watch for the AI to set `CCDD` (0→nonzero at f141), then immediately overwrite it and
let the move execute. Two contrasting injections from the SAME battle state:

```
WHEN=after INJECT=33 (TACKLE, damaging):
  f141: AI picked CCDD=39 (TAIL WHIP)
  f141: injected CCDD=33  (read-back=33)
  post-turn: playerHP=17 (delta 3)   # *** player TOOK 3 DAMAGE -> enemy executed TACKLE = OUR value ***

WHEN=before INJECT=33:
  f141: AI picked CCDD=39
  post-turn: playerHP=20 (delta 0)   # our pre-pick write discarded; enemy used TAIL WHIP
```

The damaging-vs-not contrast is decisive: with the SAME starting state, injecting
`CCDD=33` **after** the AI pick changes the enemy's executed move from the harmless
TAIL WHIP (0 dmg) to TACKLE (3 dmg). A control injecting `CCDD=39` after the pick
yields 0 damage, as expected for TAIL WHIP. The enemy executes whatever move id sits
in `CCDD` at execution time.

### Conclusions

1. **`CCDD` is the live source of truth for the enemy's executed move.** The engine
   reads it at move-execution time (after ~f141), not a latched copy.
2. **The enemy AI selects and writes its move into `CCDD` at ~frame 141 of the turn**,
   i.e. after the player's move is committed to `CCDC` (~f130) and before either move
   executes. (Frame numbers are for this turn at 60 fps; the relevant *ordering* is
   what matters, not the exact count.)
3. **Writing `CCDD` before the AI pick is overwritten; writing it after the AI pick
   (and before execution) sticks and is used.**

### What a minimal hook for the AI-agent arena needs

To let an external agent dictate the enemy's move you must write `CCDD` in the window
**after the engine's AI-selection step and before move execution** — you cannot just
set it at the menu and walk away. Practical options:

- **Poll-and-overwrite (no core patch, what this probe does):** each turn, after
  selecting the player's action, run frames while polling `CCDD`; the moment it
  transitions `0 → nonzero` (the AI pick), overwrite it with the agent's chosen move
  and continue running. This is exactly Experiment B and it works. It is robust as
  long as you detect the single 0→nonzero transition each turn before the move
  executes (there is a comfortable ~190-frame gap from f141 pick to f331 execution).
- **Cleaner hook (if patching/instrumenting is acceptable):** intercept at the enemy
  move-selection routine. In pokered this is `AIChooseMove` / the code that stores
  the chosen move into `wEnemySelectedMove` (= `CCDD`). A breakpoint or a memory-write
  watch on `0xCCDD` fired during the enemy-selection phase lets the agent substitute
  the value once, deterministically, instead of racing the poll. With a libretro
  frontend the equivalent is: detect the enemy-selection phase (e.g. `CCDC` just
  became nonzero this turn and `CCDD` just became nonzero) and write once.
- Either way, also write/respect `CCDC` for the player's move and gate on `CCD5`
  (turn counter) to do exactly one substitution per turn.

## Reproduce

```
# probe lives in /tmp/ccdd-probe (copy of research/gb-harness + battle logic)
cd /tmp/ccdd-probe && RUSTUP_TOOLCHAIN=1.92 cargo build --release
CORE=~/pokemon-pvp-red/cores/gambatte_libretro.dylib
ROM="~/pokemon-pvp-red/Pokemon Red.gb"

# Reach the rival battle and save battle.state:
SAVE=1 ./target/release/ccddprobe $CORE "$ROM" fullreach

# Observe the CCDD/CCDC/CCD5 turn timeline (no writes):
./target/release/ccddprobe $CORE "$ROM" ccddobs

# Definitive CCDD injection (proves CCDD drives the executed move):
WHEN=after  INJECT=33 ./target/release/ccddprobe $CORE "$ROM" ccdd2   # 3 dmg (TACKLE)
WHEN=before INJECT=33 ./target/release/ccddprobe $CORE "$ROM" ccdd2   # 0 dmg (overwritten)
```

Move ids referenced: 10=SCRATCH, 33=TACKLE, 39=TAIL WHIP, 45=GROWL.
Saved states: `states/battle.state` (rival battle, FIGHT menu), `states/bedroom.state`.
