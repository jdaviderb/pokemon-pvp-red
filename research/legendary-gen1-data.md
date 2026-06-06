# Legendary Gen-1 Data — ARTICUNO & ZAPDOS (ready-to-code)

Authoritative, cross-checked against **pret/pokered** disassembly (master), Bulbapedia, and
`docs/pokemon-red-ram-map.md`. All values verified for vanilla Pokémon Red/Blue (international).

Sources:
- pret/pokered `constants/pokemon_constants.asm` (internal species indices)
- pret/pokered `constants/type_constants.asm` (type ids)
- pret/pokered `data/pokemon/base_stats/{articuno,zapdos}.asm` (base stats)
- pret/pokered `data/moves/moves.asm` + `constants/move_constants.asm` (move ids + PP)
- pret/pokered `constants/charmap.asm` (text encoding)
- `docs/pokemon-red-ram-map.md` (struct layout, addresses, BIG-ENDIAN rule)

---

## 1. Internal species index byte (NOT Pokédex number)

This is the byte stored in `wPartySpecies` / `wPartyMon.species` (+0) / `wBattleMon.species`
(`D014`) / `wEnemyMon.species` (`CFE5`).

| Pokémon  | Internal index | hex   | Pokédex # |
|----------|---------------:|-------|----------:|
| ARTICUNO | **74**         | `0x4A`| 144       |
| ZAPDOS   | **75**         | `0x4B`| 145       |

Validation against live-verified starters (prompt-confirmed): CHARMANDER = 176 / `0xB0`,
SQUIRTLE = 177 / `0xB1`. The pokered `const` ordering places ARTICUNO/ZAPDOS at `$4A`/`$4B`
consecutively, far below the starters — confirms internal index != Pokédex number.

```rust
pub const SPECIES_ARTICUNO: u8 = 0x4A; // 74
pub const SPECIES_ZAPDOS:   u8 = 0x4B; // 75
```

---

## 2. Base stats + types

### Gen-1 type-id table (`constants/type_constants.asm`)

| Type     | id (dec) | hex   |
|----------|---------:|-------|
| NORMAL   | 0        | `0x00`|
| FIGHTING | 1        | `0x01`|
| FLYING   | 2        | `0x02`|
| POISON   | 3        | `0x03`|
| GROUND   | 4        | `0x04`|
| ROCK     | 5        | `0x05`|
| BIRD     | 6        | `0x06`| (unused on mons) |
| BUG      | 7        | `0x07`|
| GHOST    | 8        | `0x08`|
| (gap $09–$13: unused) |  |   |
| FIRE     | 20       | `0x14`|
| WATER    | 21       | `0x15`|
| GRASS    | 22       | `0x16`|
| ELECTRIC | 23       | `0x17`|
| PSYCHIC  | 24       | `0x18`|
| ICE      | 25       | `0x19`|
| DRAGON   | 26       | `0x1A`|

### Base stats

| Pokémon  | HP | Atk | Def | Spd | Spc | Type1            | Type2             | Catch | BaseExp |
|----------|---:|----:|----:|----:|----:|------------------|-------------------|------:|--------:|
| ARTICUNO | 90 | 85  | 100 | 85  | 125 | ICE (`0x19`/25)  | FLYING (`0x02`/2) | 3     | 215     |
| ZAPDOS   | 90 | 90  | 85  | 100 | 125 | ELECTRIC (`0x17`/23) | FLYING (`0x02`/2) | 3 | 216     |

```rust
// type1, type2 bytes for the struct (+5, +6)
pub const ARTICUNO_TYPE1: u8 = 0x19; // ICE
pub const ARTICUNO_TYPE2: u8 = 0x02; // FLYING
pub const ZAPDOS_TYPE1:   u8 = 0x17; // ELECTRIC
pub const ZAPDOS_TYPE2:   u8 = 0x02; // FLYING
```

---

## 3. Lv50 stats (DV=15 all, stat-EXP=0)

Formula (exactly as given, integer floor division):
```
stat = floor((base + DV) * 2 * level / 100) + 5
HP   = floor((base + DV) * 2 * level / 100) + level + 10     (level=50, DV=15)
```

At Lv50, `(base+15)*2*50/100 = (base+15)`. So `stat = base + 15 + 5 = base + 20`,
`HP = base + 15 + 50 + 10 = base + 75`. Verified by integer arithmetic.

| Pokémon  | HP  | Atk | Def | Spd | Spc |
|----------|----:|----:|----:|----:|----:|
| ARTICUNO | 165 | 105 | 120 | 105 | 145 |
| ZAPDOS   | 165 | 110 | 105 | 120 | 145 |

These are both `Current HP` (+1) AND `Max HP` (+34) (set current = max), and the stat fields
(+36 Atk, +38 Def, +40 Spd, +42 Spc). All written **BIG-ENDIAN** (e.g. 165 = `[0x00, 0xA5]`).

```rust
// (hp, atk, def, spd, spc) at Lv50, DV=15, EV=0
pub const ARTICUNO_STATS: [u16; 5] = [165, 105, 120, 105, 145];
pub const ZAPDOS_STATS:   [u16; 5] = [165, 110, 105, 120, 145];
```

---

## 4. Lv50 movesets (move id + base PP)

Move ids from `constants/move_constants.asm`; base PP from `data/moves/moves.asm`.

### ARTICUNO

| Slot | Move      | id (dec) | hex   | PP |
|------|-----------|---------:|-------|---:|
| 1    | ICE BEAM  | 58       | `0x3A`| 10 |
| 2    | BLIZZARD  | 59       | `0x3B`| 5  |
| 3    | PECK      | 64       | `0x40`| 35 |
| 4    | AGILITY   | 97       | `0x61`| 30 |

### ZAPDOS

| Slot | Move        | id (dec) | hex   | PP |
|------|-------------|---------:|-------|---:|
| 1    | THUNDERBOLT | 85       | `0x55`| 15 |
| 2    | THUNDER     | 87       | `0x57`| 10 |
| 3    | DRILL PECK  | 65       | `0x41`| 20 |
| 4    | AGILITY     | 97       | `0x61`| 30 |

PP byte at +29..+32 = current PP in low 6 bits, top 2 bits = PP-Ups (use 0). So just store the
raw PP value (all < 64, no masking needed).

```rust
// (move_id, base_pp)
pub const ARTICUNO_MOVES: [(u8, u8); 4] =
    [(0x3A, 10), (0x3B, 5), (0x40, 35), (0x61, 30)];
pub const ZAPDOS_MOVES: [(u8, u8); 4] =
    [(0x55, 15), (0x57, 10), (0x41, 20), (0x61, 30)];
```

---

## 5. wPartyMon 44-byte struct layout + addresses

Stride = **44 bytes** per slot. Multi-byte = **BIG-ENDIAN** (`u16::from_be_bytes` / write hi first).

### Base addresses (CPU; SYSTEM_RAM offset = addr − 0xC000)

Player party:
| Slot | Addr   |        | Slot | Addr   |
|------|--------|--------|------|--------|
| 1    | `D16B` |        | 4    | `D1EF` |
| 2    | `D197` |        | 5    | `D21B` |
| 3    | `D1C3` |        | 6    | `D247` |

Enemy party:
| Slot | Addr   |        | Slot | Addr   |
|------|--------|--------|------|--------|
| 1    | `D8A4` |        | 4    | `D928` |
| 2    | `D8D0` |        | 5    | `D954` |
| 3    | `D8FC` |        | 6    | `D980` |

Also set list headers before the structs:
- Player: `wPartyCount` `D163` = count; `wPartySpecies` `D164`+ = species per slot;
  terminator `0xFF` after last species.
- Enemy: `wEnemyPartyCount` `D89C` = count; `wEnemyPartySpecies` `D89D`+ = species;
  terminator `0xFF`.

### Per-field offsets within a slot

| Off | Field            | Size | Endian | Notes |
|----:|------------------|-----:|--------|-------|
| +0  | Species          | 1    | —      | internal index byte (`0x4A`/`0x4B`) |
| +1  | Current HP       | 2    | **BE** | set = max HP |
| +3  | box-level scratch| 1    | —      | not live level; can mirror level or 0 |
| +4  | Status condition | 1    | —      | 0x00 = healthy |
| +5  | Type 1           | 1    | —      | |
| +6  | Type 2           | 1    | —      | |
| +7  | Catch rate / held item | 1 | —    | base catch rate (3) for both; harmless |
| +8  | Move 1           | 1    | —      | |
| +9  | Move 2           | 1    | —      | |
| +10 | Move 3           | 1    | —      | |
| +11 | Move 4           | 1    | —      | |
| +12 | OT / Trainer ID  | 2    | **BE** | any value (match player's) |
| +14 | Experience       | 3    | **BE** | 24-bit; ≥ exp-for-Lv50 (slow group ≈ 125000); see note |
| +17 | HP stat-EXP (EV) | 2    | **BE** | 0 |
| +19 | Attack stat-EXP  | 2    | **BE** | 0 |
| +21 | Defense stat-EXP | 2    | **BE** | 0 |
| +23 | Speed stat-EXP   | 2    | **BE** | 0 |
| +25 | Special stat-EXP | 2    | **BE** | 0 |
| +27 | Atk/Def DV       | 1    | —      | `0xFF` = both 15 (hi nibble Atk, lo Def) |
| +28 | Spd/Spc DV       | 1    | —      | `0xFF` = both 15 |
| +29 | PP move 1        | 1    | —      | low6 = PP, top2 = PP-Up (0) |
| +30 | PP move 2        | 1    | —      | |
| +31 | PP move 3        | 1    | —      | |
| +32 | PP move 4        | 1    | —      | |
| +33 | **Level (live)** | 1    | —      | 50 / `0x32` |
| +34 | Max HP           | 2    | **BE** | |
| +36 | Attack           | 2    | **BE** | |
| +38 | Defense          | 2    | **BE** | |
| +40 | Speed            | 2    | **BE** | |
| +42 | Special          | 2    | **BE** | |

Total = 44 bytes (+42..+43 = last 2 bytes of Special).

> **Exp note (+14, 3 bytes BE):** Articuno & Zapdos are Slow growth group; exp for Lv50 is
> `floor(5*50^3/4) = 156250` = `0x026259`. Set +14..+16 = `[0x02, 0x62, 0x59]` so the engine
> doesn't see an "underleveled" mon. (Not strictly required for a one-off injected battle, but
> safe — keeps level consistent if exp recalculation runs.)

---

## 6. Nicknames — encoding + byte arrays

`wPartyMonNicks` / `wEnemyMonNicks`: **11 bytes per name**, terminator `0x50` (`@`), pad
remaining bytes with `0x50`. Gen-1 charmap (`constants/charmap.asm`):
- Uppercase `A` = `0x80`, sequential through `Z` = `0x99` (`letter = 0x80 + (c - 'A')`).
- Lowercase `a` = `0xA0` .. `z` = `0xB9`.
- Space = `0x7F`.
- Digits `0`=`0xF6` .. `9`=`0xFF`.
- Name/string terminator `@` = **`0x50`**.

### Nickname table addresses

pokered symbols (vanilla R/B): `wPartyMonNicks = D2B5`, `wEnemyMonNicks = D9EE`,
11 bytes/slot. (If your RAM map pins a different value for this ROM build, probe once — but
these are the pokered vanilla addresses.) Slot N name = base + N*11.

| Table              | Addr   |
|--------------------|--------|
| `wPartyMonNicks`   | `D2B5` |
| `wEnemyMonNicks`   | `D9EE` |

### Encoded names (11 bytes each, terminator + pad = `0x50`)

**ARTICUNO** = A R T I C U N O:
```
[0x80, 0x91, 0x93, 0x88, 0x82, 0x94, 0x8D, 0x8E, 0x50, 0x50, 0x50]
//  A     R     T     I     C     U     N     O     @     @     @
```

**ZAPDOS** = Z A P D O S:
```
[0x99, 0x80, 0x8F, 0x83, 0x8E, 0x92, 0x50, 0x50, 0x50, 0x50, 0x50]
//  Z     A     P     D     O     S     @     @     @     @     @
```

```rust
pub const NICK_ARTICUNO: [u8; 11] =
    [0x80, 0x91, 0x93, 0x88, 0x82, 0x94, 0x8D, 0x8E, 0x50, 0x50, 0x50];
pub const NICK_ZAPDOS: [u8; 11] =
    [0x99, 0x80, 0x8F, 0x83, 0x8E, 0x92, 0x50, 0x50, 0x50, 0x50, 0x50];

#[inline]
pub fn encode_name(s: &str) -> [u8; 11] {
    let mut out = [0x50u8; 11];
    for (i, c) in s.bytes().enumerate().take(10) {
        out[i] = match c {
            b'A'..=b'Z' => 0x80 + (c - b'A'),
            b'a'..=b'z' => 0xA0 + (c - b'a'),
            b' '        => 0x7F,
            b'0'..=b'9' => 0xF6 + (c - b'0'),
            _           => 0x50,
        };
    }
    out
}
```

---

## 7. Ready Rust species table

```rust
/// Gen-1 internal species data for injection. HP/stat values are Lv50, DV=15, EV=0.
pub struct Gen1Species {
    pub species: u8,         // internal index byte (struct +0)
    pub type1: u8,
    pub type2: u8,
    pub catch_rate: u8,      // struct +7 (held item slot); base catch rate
    pub stats: [u16; 5],     // [HP, Atk, Def, Spd, Spc] -> +1/+34, +36, +38, +40, +42
    pub moves: [(u8, u8); 4],// (move_id, base_pp) -> +8..+11 ids, +29..+32 PP
    pub nick: [u8; 11],      // wPartyMonNicks / wEnemyMonNicks entry
    pub exp_be: [u8; 3],     // 24-bit BE exp for Lv50 (struct +14) — Slow group
}

pub const ARTICUNO: Gen1Species = Gen1Species {
    species: 0x4A,
    type1: 0x19,  // ICE
    type2: 0x02,  // FLYING
    catch_rate: 3,
    stats: [165, 105, 120, 105, 145],
    moves: [(0x3A, 10), (0x3B, 5), (0x40, 35), (0x61, 30)], // IceBeam,Blizzard,Peck,Agility
    nick: [0x80, 0x91, 0x93, 0x88, 0x82, 0x94, 0x8D, 0x8E, 0x50, 0x50, 0x50],
    exp_be: [0x02, 0x62, 0x59], // 156250 = Lv50 Slow
};

pub const ZAPDOS: Gen1Species = Gen1Species {
    species: 0x4B,
    type1: 0x17,  // ELECTRIC
    type2: 0x02,  // FLYING
    catch_rate: 3,
    stats: [165, 110, 105, 120, 145],
    moves: [(0x55, 15), (0x57, 10), (0x41, 20), (0x61, 30)], // Tbolt,Thunder,DrillPeck,Agility
    nick: [0x99, 0x80, 0x8F, 0x83, 0x8E, 0x92, 0x50, 0x50, 0x50, 0x50, 0x50],
    exp_be: [0x02, 0x62, 0x59],
};

pub const LEVEL: u8 = 50; // 0x32
pub const DV_ATKDEF: u8 = 0xFF; // Atk=15, Def=15
pub const DV_SPDSPC: u8 = 0xFF; // Spd=15, Spc=15
```

### Writing a slot (helper sketch)

```rust
/// Write a 44-byte party struct for `sp` into `ram` at CPU `base` (e.g. 0xD16B / 0xD8A4).
fn write_party_mon(ram: &mut [u8], base: u16, sp: &Gen1Species, level: u8) {
    let mut w8  = |off: u16, v: u8|  ram[(base + off - 0xC000) as usize] = v;
    let mut put = |o: usize, v: u8|  ram[(base as usize - 0xC000) + o] = v;
    let be = |v: u16| v.to_be_bytes();

    put(0, sp.species);
    let hp = be(sp.stats[0]); put(1, hp[0]); put(2, hp[1]);   // current HP = max
    put(3, level);                                            // box-level scratch (cosmetic)
    put(4, 0x00);                                             // status = healthy
    put(5, sp.type1); put(6, sp.type2);
    put(7, sp.catch_rate);
    for i in 0..4 { put(8 + i, sp.moves[i].0); }
    // +12 OT id (2 BE) — copy player's; here 0
    put(12, 0x00); put(13, 0x00);
    put(14, sp.exp_be[0]); put(15, sp.exp_be[1]); put(16, sp.exp_be[2]);
    for o in 17..27 { put(o, 0x00); }                         // stat-EXP all 0
    put(27, 0xFF); put(28, 0xFF);                             // DVs = all 15
    for i in 0..4 { put(29 + i, sp.moves[i].1); }             // PP (no PP-Up)
    put(33, level);                                           // LIVE level
    let mx  = be(sp.stats[0]); put(34, mx[0]);  put(35, mx[1]);
    let atk = be(sp.stats[1]); put(36, atk[0]); put(37, atk[1]);
    let def = be(sp.stats[2]); put(38, def[0]); put(39, def[1]);
    let spd = be(sp.stats[3]); put(40, spd[0]); put(41, spd[1]);
    let spc = be(sp.stats[4]); put(42, spc[0]); put(43, spc[1]);
    let _ = &mut w8; // (w8 unused in this sketch; offsets via put)
}
```

> Remember: the on-screen sprite/name/cry are drawn by the engine's **send-out** routine. Inject
> these party structs + species lists + nicks into a savestate captured **at the battle INTRO
> (before send-out)**, then resume — the engine reads `wPartySpecies`/`wEnemyPartySpecies` and the
> structs to draw the real Articuno/Zapdos.
