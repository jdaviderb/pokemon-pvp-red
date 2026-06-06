# Legendary Battle Injection — Empirical Proof (Articuno vs Zapdos)

Headless gambatte + vanilla `Pokemon Red.gb`. Goal: pick the two Pokémon that fight,
with **correct on-screen sprites + names + cries** (not just mechanics). The mechanism:
capture a savestate at the battle INTRO (before send-out), inject full party structs for
both sides, and let the engine's own send-out routine draw the real species.

**RESULT: PROVEN AND WORKING.** Injecting `wPartySpecies`/`wPartyMon[0]` (player) and
`wEnemyPartySpecies`/`wEnemyMon[0]` (enemy) into a pre-send-out intro savestate makes the
engine send out the injected species. Post-send-out `wBattleMon.species (D014)` and
`wEnemyMon.species (CFE5)` equal the injected internal indices, and the framebuffer shows
the injected legendaries' sprites + names + levels — NOT the original Charmander/Squirtle.

Proof screenshots (in `research/`):
- `legendary-injection-proof.png` — ZAPDOS :L50 (front sprite) vs ARTICUNO :L50 (back sprite), "Go! ARTICUNO!".
- `legendary-intro-preinject.png` — the clean pre-send-out intro (rival sprite, empty text box) that we inject into.
- `legendary-injection-moltres-dragonite.png` — generality: DRAGONITE vs Moltres back-sprite (different species pair).

Probe source + build files copied to `research/legendary-probe/` (a copy of `research/gb-harness`
extended with memory + savestate + scripted input + two new modes `introcap` / `injtest`).

---

## TL;DR — the recipe that works

1. **Capture point:** the battle INTRO frame where the rival trainer sprite is shown and the
   text box is empty (the "<RIVAL> wants to fight!" moment), BEFORE either mon is sent out.
   In the run this was **candidate k17** (`cand_018.state`), saved to
   `states/legendary_intro.state`. Markers at that frame: `D057=2` (trainer battle),
   `wBattleMon.species D014=0`, `wEnemyMon.species CFE5=0` (neither sent out yet),
   `wEnemyPartyCount D89C=1`, `wEnemyPartySpecies D89D=177` (the trainer party is ALREADY
   loaded into WRAM — this is why enemy injection survives, see below).
2. **Order:** `retro_unserialize(intro.state)` → write both party structs → `retro_run()` through
   the send-out (tap A to advance the intro text).
3. **Read back:** after send-out, `D014` (player active species) and `CFE5` (enemy active
   species) equal the injected indices. The cry/sprite/name are drawn from those indices.

```
cand_018 (k17) loaded:   D014=0  CFE5=0   D89D=177  D164=176       (Squirtle/Charmander)
inject:                  D163=1 D164=0x4A(Articuno) | D89C=1 D89D=0x4B(Zapdos)
run send-out:
  i0  : CFE5 0 -> 75 (0x4B Zapdos)     <- enemy sent out FIRST, from injected wEnemyPartyMon
  i17 : D014 0 -> 74 (0x4A Articuno)   <- player sent out, from injected wPartyMon
POST:  D014=74 (0x4A) MATCH=true | CFE5=75 (0x4B) MATCH=true | pLvl=50 pHP=150 eHP=150
```

---

## The injection offsets that took effect (CPU addrs; WRAM idx = addr-0xC000)

All multibyte HP/stats are **BIG-ENDIAN** (Gen-1 quirk).

### Player side (took effect → `wBattleMon.species` D014)
| Addr  | Field | Wrote |
|-------|-------|-------|
| `D163` | wPartyCount | `1` |
| `D164` | wPartySpecies[0] | `0x4A` (Articuno internal index) |
| `D165` | list terminator | `0xFF` |
| `D16B` | wPartyMon[0] struct base (44 bytes) | see struct below |
| `D2B5` | wPartyMonNicks[0] (11 bytes, 0x50-terminated) | `"ARTICUNO"` |

### Enemy side (took effect → `wEnemyMon.species` CFE5)
| Addr  | Field | Wrote |
|-------|-------|-------|
| `D89C` | wEnemyPartyCount | `1` |
| `D89D` | wEnemyPartySpecies[0] | `0x4B` (Zapdos internal index) |
| `D89E` | list terminator | `0xFF` |
| `D8A4` | wEnemyMon[0] struct base (44 bytes) | see struct below |
| `D9EE` | wEnemyMonNicks[0] | `"ZAPDOS"` (see name note) |

### The 44-byte party-mon struct we built (offsets from base, BE multibyte)
`+0 species` · `+1 curHP(BE)` · `+4 status=0` · `+5 type1` · `+6 type2` ·
`+8..+11 moves` · `+27/+28 DV=0xFF` · `+29..+32 PP` · `+33 LEVEL(=50)` ·
`+34 maxHP(BE)` · `+36 atk(BE)` · `+38 def(BE)` · `+40 spd(BE)` · `+42 spc(BE)`.
A minimal struct (species, level, hp/maxhp, one move+PP, types, sane stats) is enough for the
engine to send the mon out and draw it. EVs/exp/OT-id left zero with no ill effect on the intro.

---

## Internal species indices (VERIFIED live, NOT dex numbers)

The species byte the engine sends out is the **Gen-1 internal index**, which differs from the
Pokédex number. Verified by reading back `D014`/`CFE5` after send-out:

| Pokémon  | Internal index | Confirmed at |
|----------|----------------|--------------|
| Articuno | `0x4A` (74)    | D014=74 after player send-out |
| Zapdos   | `0x4B` (75)    | CFE5=75 after enemy send-out |
| Moltres  | `0x49` (73)    | D014=73 (generality test) |
| Dragonite| `0x42` (66)    | CFE5=66 (generality test) |
| Charmander | `0xB0` (176) | original `D164`/`D014` |
| Squirtle | `0xB1` (177)   | original `D89D`/`CFE5` |

(The Gen-1 internal index table is the canonical pokered ordering; the four above are the
empirically verified values for this ROM.)

---

## The trainer-battle clobber question — ANSWERED

The prompt warned trainer-battle enemy data might be re-read from ROM and clobber the enemy
injection. **It does NOT clobber, because the trainer's party is read from ROM into WRAM
(`wEnemyPartyMons` at `D8A4`, `wEnemyPartySpecies` at `D89D`) very early in the battle init —
already present at the FIRST frame `D057!=0` (cand_000 showed `D89D=177`).** The per-mon
send-out routine (`LoadEnemyMon`) builds `wEnemyMon` (`CFE5..`) from `wEnemyPartyMons[wWhichPokemon]`
in **WRAM**; it does not re-read trainer data from ROM at send-out. So overwriting
`wEnemyPartySpecies[0]` + `wEnemyMon[0]` in the intro state (after the trainer load, before
send-out) sticks: `CFE5` became `0x4B` (our Zapdos), never reverting to `0xB1` (Squirtle).

=> **A trainer-battle intro savestate works for BOTH sides.** A wild-battle intro is therefore
**not required**, though it would work identically (and is simpler since there's only one enemy
mon and no trainer party list). For the rival/trainer scenario captured here, trainer ROM
re-read is a non-issue as long as you inject AFTER `D057!=0` and BEFORE send-out — which is
exactly the captured intro point.

---

## Name vs sprite vs cry — important behavioral detail

Empirically (from the Articuno/Zapdos run and the Moltres/Dragonite generality run):

- **Sprite** (front for enemy, back for player) is drawn from the **species internal index**
  at send-out. Inject the index → correct sprite. PROVEN visually for both sides.
- **Cry** is played by the same species-indexed send-out routine (`PlayCry`/`LoadMonData`),
  so it tracks the species index identically. (Not audio-captured in this headless probe, but
  it is the same code path and same index that select the sprite, which IS proven — high
  confidence the cry is correct.)
- **Enemy name** is taken from the species' base name string → it AUTO-CORRECTS from the
  injected species. (Generality run: enemy nick was hardcoded "ZAPDOS" but the screen correctly
  showed "DRAGONITE" for species 0x42.)
- **Player name** is taken from `wPartyMonNicks[0]` (`D2B5`) → you MUST write a matching
  nickname. (Generality run: species=Moltres but the stale nick "ARTICUNO" was displayed; the
  back-sprite was Moltres's. In the primary run, nick "ARTICUNO" + species Articuno matched.)

**Action for the arena:** set the player species index AND write the matching uppercase name
into `wPartyMonNicks[0]` (`D2B5`, Gen-1 charset: 'A'..'Z' = 0x80.., terminator 0x50). The enemy
name needs no nickname write (engine uses the species name), but writing `wEnemyMonNicks`
(`D9EE`) to match is harmless.

---

## Capture-point map (introcap output, k = capture index after D057!=0)

| Candidate | When | D014 (player) | CFE5 (enemy) | D89D (enemy party) | Notes |
|-----------|------|---------------|--------------|--------------------|-------|
| cand_000  | first D057!=0 | 0 | 0 | 177 | trainer party already loaded |
| cand_018 (k17) | rival sprite, empty box | 0 | 0 | 177 | **INJECT HERE** (pre both send-outs) |
| cand_019 (k18) | enemy appearing | 0 | 177 | 177 | enemy already sent out — too late for clean enemy inject |
| cand_037 (k36) | SQUIRTLE :L5 on field | 176 | 177 | 177 | both sent out (original FIGHT-menu region) |

The safe injection window is **cand_000 .. cand_018** (any frame with `D014==0 && CFE5==0`).
We chose cand_018 (latest pre-send-out) and saved it as `states/legendary_intro.state`.
Injecting at cand_018 reproduced byte-for-byte (gambatte is deterministic) across a fresh
process and across two different species pairs.

---

## Files

- **Winning intro savestate:** `states/legendary_intro.state` (== `cand_018.state`,
  md5 `f28cea749f3ff4e83a640c8487d8ee8f`, 59650 bytes). Load this, inject, run send-out.
- Proof PNGs: `research/legendary-injection-proof.png`,
  `research/legendary-intro-preinject.png`,
  `research/legendary-injection-moltres-dragonite.png`.
- Probe (reproduce): `research/legendary-probe/` (modes `introcap`, `injtest`).

### Reproduce
```
# build (copy of research/gb-harness + battle/savestate/input + introcap/injtest modes)
cd /tmp/legendary   # or research/legendary-probe (cp to a writable dir + cargo build)
RUSTUP_TOOLCHAIN=1.92 cargo build --release
CORE=~/pokemon-pvp-red/cores/gambatte_libretro.dylib
ROM="~/pokemon-pvp-red/Pokemon Red.gb"

# 1) reach rival battle, save intro candidates cand_000..cand_039 to /tmp/legendary
RUSTUP_TOOLCHAIN=1.92 ./target/release/ccddprobe "$CORE" "$ROM" introcap

# 2) inject Articuno(0x4A) vs Zapdos(0x4B) at the intro, drive send-out, dump PNG:
STATE_PATH=states/legendary_intro.state PLAYER_SPECIES=0x4A ENEMY_SPECIES=0x4B LVL=50 \
  TAG=proof TAG2=proof_field EXTRA_FRAMES=12 \
  RUSTUP_TOOLCHAIN=1.92 ./target/release/ccddprobe "$CORE" "$ROM" injtest
# -> prints D014/CFE5 MATCH=true; writes proof.ppm / proof_field.ppm
# convert: sips -s format png proof_field.ppm --out proof_field.png
```

---

## Integration notes for `src/battle.rs`

The existing `inject_player_party` / `inject_enemy_party` only write the count+species list.
For correct send-out you ALSO need the full 44-byte `wPartyMon[0]`/`wEnemyMon[0]` struct and
(player only) the nickname. Add an `inject_full_mon(ram, base, species, level, hp, maxhp,
moves, pp, type1, type2, stats)` that writes the struct per §"44-byte struct" above, plus a
`write_nick(ram, D2B5, name)` for the player. The arena flow becomes:

```
load_state(legendary_intro.state)   // D057=2, neither side sent out, enemy trainer party in WRAM
inject player: D163=1, D164=species, D165=0xFF, struct@D16B, nick@D2B5
inject enemy:  D89C=1, D89D=species, D89E=0xFF, struct@D8A4   (nick@D9EE optional)
run frames + tap A through "wants to fight" / "Go!" until D014!=0 && CFE5!=0
// now both legendaries are on the field with correct sprite/name/cry; FIGHT menu follows
```

Internal indices for the four legendary birds + a few mons are in the table above; the full
Gen-1 internal-index table (pokered `BaseStats`/`MonsterNames` order) should be embedded for the
arena's species picker.
