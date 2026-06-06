# Pokémon Red / Blue (Gen 1, DMG) RAM Map — Battle / Party / Turn State

Authoritative field → CPU address → size mapping needed to fill `BattleState` / `BattlePokemon`.
Source: https://datacrystal.tcrf.net/wiki/Pok%C3%A9mon_Red_and_Blue/RAM_map cross-referenced
with the `pret/pokered` disassembly symbol names. Addresses are **CPU addresses** (vanilla
Pokémon Red/Blue, international). The same battle RAM layout is used by the `.gbc` color hack.

> **ENDIANNESS — READ THIS FIRST.** Gen-1 stores every multi-byte value (current HP, max HP,
> Attack/Defense/Speed/Special, experience, trainer ID, EVs) **BIG-ENDIAN**: the high byte is at
> the lower address. So `HP = (ram[addr] as u16) << 8 | ram[addr+1] as u16`, i.e.
> `u16::from_be_bytes([ram[addr], ram[addr+1]])`. This is the opposite of normal Z80/GB
> little-endian and is the #1 reader bug. (Move IDs, PP, level, species are single bytes — no
> endian concern.)

---

## 0. How CPU addresses map into libretro memory

```
RETRO_MEMORY_SYSTEM_RAM (id = 2)  -> 8 KiB WRAM, CPU 0xC000..0xE000.
    wram_offset(addr) = addr - 0xC000        for addr in 0xC000..=0xDFFF
RETRO_MEMORY_SAVE_RAM   (id = 0)  -> cartridge SRAM (not needed for live battle reads).
```

Every battle/party address in this doc is in `0xC000..0xDFFF`, so they all live in SYSTEM_RAM
at `addr - 0xC000`. **Exception:** `FFF3` (battle turn) is in **HRAM** (`0xFF80..0xFFFE`), which is
**NOT** part of the 8 KiB WRAM region and is almost certainly **NOT** exposed by gambatte's
SYSTEM_RAM. Treat `FFF3` as unavailable unless a runtime probe proves otherwise (see §6); derive
turn state from a software state machine on `D057` instead.

---

## 1. Player party (out-of-battle roster)

| Field             | Address      | Size | Notes |
|-------------------|--------------|------|-------|
| `wPartyCount`     | `D163`       | 1    | number of mons in party (0–6) |
| `wPartySpecies`   | `D164`–`D169`| 6    | species id per slot |
| (list terminator) | `D16A`       | 1    | `0xFF` |
| `wPartyMons[0]`   | `D16B`       | 44   | full struct, slot 1 (see §3 layout) |
| `wPartyMons[1]`   | `D197`       | 44   | slot 2 (`D16B + 44`) |
| `wPartyMons[2]`   | `D1C3`       | 44   | |
| `wPartyMons[3]`   | `D1EF`       | 44   | |
| `wPartyMons[4]`   | `D21B`       | 44   | |
| `wPartyMons[5]`   | `D247`       | 44   | |

Full roster region: `D16B`–`D272` (6 × 44 bytes).

## 2. Enemy party

| Field              | Address       | Size | Notes |
|--------------------|---------------|------|-------|
| `wEnemyPartyCount` | `D89C`        | 1    | enemy mon count |
| `wEnemyPartySpecies`| `D89D`–`D8A2`| 6    | species id per slot |
| (list terminator)  | `D8A3`        | 1    | `0xFF` |
| `wEnemyMons[0]`    | `D8A4`        | 44   | slot 1 (same 44-byte layout as §3) |
| `wEnemyMons[1]`    | `D8D0`        | 44   | |
| `wEnemyMons[2]`    | `D8FC`        | 44   | |
| `wEnemyMons[3]`    | `D928`        | 44   | |
| `wEnemyMons[4]`    | `D954`        | 44   | |
| `wEnemyMons[5]`    | `D980`        | 44   | |

Full enemy region: `D8A4`–`DA2F`.

## 3. 44-byte party-mon struct (`box_struct` / `party_struct`, used by §1 and §2)

Offsets relative to the slot base address. **BE = big-endian u16/u24.**

| Off   | Field            | Size | Endian | Notes |
|-------|------------------|------|--------|-------|
| +0    | Species          | 1    | —      | |
| +1    | Current HP       | 2    | **BE** | |
| +3    | (box level / "level scratch") | 1 | — | not the live level; use +33 |
| +4    | Status condition | 1    | —      | bitfield (see §7) |
| +5    | Type 1           | 1    | —      | |
| +6    | Type 2           | 1    | —      | |
| +7    | Catch rate / held item | 1 | —    | |
| +8    | Move 1           | 1    | —      | move id |
| +9    | Move 2           | 1    | —      | |
| +10   | Move 3           | 1    | —      | |
| +11   | Move 4           | 1    | —      | |
| +12   | Trainer ID       | 2    | **BE** | original-trainer id |
| +14   | Experience       | 3    | **BE** | 24-bit |
| +17   | HP EV            | 2    | **BE** | |
| +19   | Attack EV        | 2    | **BE** | |
| +21   | Defense EV       | 2    | **BE** | |
| +23   | Speed EV         | 2    | **BE** | |
| +25   | Special EV       | 2    | **BE** | |
| +27   | Atk/Def IV (DV)  | 1    | —      | packed nibbles (hi=Atk, lo=Def) |
| +28   | Spd/Spc IV (DV)  | 1    | —      | packed nibbles (hi=Spd, lo=Spc) |
| +29   | PP move 1        | 1    | —      | low 6 bits = PP, top 2 = PP-Up |
| +30   | PP move 2        | 1    | —      | |
| +31   | PP move 3        | 1    | —      | |
| +32   | PP move 4        | 1    | —      | |
| +33   | **Level (live)** | 1    | —      | the real level |
| +34   | Max HP           | 2    | **BE** | |
| +36   | Attack           | 2    | **BE** | |
| +38   | Defense          | 2    | **BE** | |
| +40   | Speed            | 2    | **BE** | |
| +42   | Special          | 2    | **BE** | |

## 4. In-battle ACTIVE PLAYER mon (`wBattleMon`, `D009`–`D030`)

This is the live copy the battle engine reads/writes each turn — read THIS during a battle, not
the party slot. **HP/stats are big-endian.**

| Field            | Address       | Size | Endian | Notes |
|------------------|---------------|------|--------|-------|
| `wBattleMonNick` | `D009`–`D013` | ~11  | —      | name (not needed) |
| Species          | `D014`        | 1    | —      | |
| Current HP       | `D015`–`D016` | 2    | **BE** | |
| (box level)      | `D017`        | 1    | —      | ignore; use `D022` |
| Status           | `D018`        | 1    | —      | §7 bitfield |
| Type 1 / Type 2  | `D019` / `D01A` | 1+1 | —     | |
| Moves 1–4        | `D01C`–`D01F` | 4    | —      | move ids |
| Atk/Def DV       | `D020`        | 1    | —      | |
| Spd/Spc DV       | `D021`        | 1    | —      | |
| **Level**        | `D022`        | 1    | —      | live level |
| Max HP           | `D023`–`D024` | 2    | **BE** | |
| Attack           | `D025`–`D026` | 2    | **BE** | |
| Defense          | `D027`–`D028` | 2    | **BE** | |
| Speed            | `D029`–`D02A` | 2    | **BE** | |
| Special          | `D02B`–`D02C` | 2    | **BE** | |
| PP moves 1–4     | `D02D`–`D030` | 4    | —      | low 6 bits = current PP |

`wPlayerMonNumber` (`D05D`) = which party slot is currently sent out (0-based).

## 5. In-battle ACTIVE ENEMY mon (`wEnemyMon`, ~`CFE5`–`D008`)

| Field            | Address       | Size | Endian | Notes |
|------------------|---------------|------|--------|-------|
| Species          | `CFE5`        | 1    | —      | active enemy species |
| Current HP       | `CFE6`–`CFE7` | 2    | **BE** | |
| (box level)      | `CFE8`        | 1    | —      | per prompt = level here; DataCrystal lists CFE8 as level — see note |
| Status           | `CFE9`        | 1    | —      | §7 bitfield |
| Type 1 / Type 2  | `CFEA` / `CFEB` | 1+1 | —     | |
| Catch rate       | `CFEC`        | 1    | —      | |
| Moves 1–4        | `CFED`–`CFF0` | 4    | —      | move ids |
| DVs              | `CFF1`–`CFF3` | —    | —      | packed |
| Max HP           | `CFF4`–`CFF5` | 2    | **BE** | |
| Attack           | `CFF6`–`CFF7` | 2    | **BE** | |
| Defense          | `CFF8`–`CFF9` | 2    | **BE** | |
| Speed            | `CFFA`–`CFFB` | 2    | **BE** | |
| Special          | `CFFC`–`CFFD` | 2    | **BE** | |
| PP moves 1–4     | `CFFE`–`D001` | 4    | —      | |

> **CFED vs CFEE / CFE8 vs the +offset table — VERIFY AT RUNTIME.** The prompt spec gives
> enemy moves at `CFED`–`CFF0`, level at `CFE8`, stats at `CFF4`–`CFFD`, PP at `CFFE`–`D001`.
> The DataCrystal HTML table is internally off-by-one in a few enemy rows (it lists `CFEE` for
> moves, `CFED` as "catch rate"). **Trust the prompt's canonical anchors** (`CFE6` HP,
> `CFE8` level, `CFED` moves, `CFF4` stats, `CFFE` PP) — they match the player struct's relative
> spacing. Confirm with a one-time probe: in a known battle, the enemy max-HP at `CFF4` (BE)
> should equal the on-screen enemy HP bar max, and `CFED` should equal the enemy's first move id.

## 6. Battle state variables

| Field                 | Address | Size | Notes |
|-----------------------|---------|------|-------|
| `wIsInBattle`         | `D057`  | 1    | **0 = not in battle**, 1 = wild, 2 = trainer, `0xFF`/`-1` = special/lost. This is the master "are we in battle" flag. |
| `wBattleType`         | `D05A`  | 1    | battle variant (0 normal, safari, old-man tutorial, etc.) |
| `wPlayerMoveListIndex`| `CC2E`  | 1    | cursor index within the FIGHT move list (useful to confirm selection) |
| `wCurrentMenuItem`    | `CC26`  | 1    | generic menu cursor (FIGHT/PKMN/ITEM/RUN; and move list) |
| `wTopMenuItemY/X`     | `CC24`/`CC25` | 1+1 | menu cursor position; helps detect which menu is open |
| Turns-in-battle       | `CCD5`  | 1    | incremented each turn |
| `wPlayerSelectedMove` | `CCDC`  | 1    | move id the player committed this turn |
| `wEnemySelectedMove`  | `CCDD`  | 1    | move id the enemy committed this turn |
| (battle turn, HRAM)   | `FFF3`  | 1    | 0 = player's action, 1 = enemy's. **HRAM — likely NOT in SYSTEM_RAM.** Do not rely on it. |

> **D057 / D05A label conflict.** DataCrystal's wiki labels `D057` as "Battle type" and `D05A`
> as a variant. The `pret/pokered` disassembly (the authoritative symbol source) names
> **`wIsInBattle` = `D057`** and **`wBattleType` = `D05A`**, which is the convention this project
> uses. The reader keys "in battle?" off `D057 != 0`. If a runtime probe ever shows the bytes
> swapped for this specific ROM build, flip them — but pokered's naming is correct for vanilla R/B.

## 7. Status-condition bitfield (the `Status` byte at +4 / `D018` / `CFE9`)

```
bit 0-2 : sleep counter (nonzero = asleep, value = turns remaining)
bit 3   : poison
bit 4   : burn
bit 5   : freeze
bit 6   : paralysis
bit 7   : (badly poisoned toxic flag in some contexts)
```
A value of `0x00` means healthy.

## 8. Navigation helpers (to script reaching a battle)

| Field      | Address | Size | Notes |
|------------|---------|------|-------|
| `wCurMap`  | `D35E`  | 1    | current map id |
| `wYCoord`  | `D361`  | 1    | player Y (tile/block) |
| `wXCoord`  | `D362`  | 1    | player X (tile/block) |

---

## 9. Reader cheat-sheet (Rust)

```rust
// SYSTEM_RAM slice `ram` covers CPU 0xC000..0xE000.
#[inline] fn rd8(ram: &[u8], addr: u16) -> u8 { ram[(addr - 0xC000) as usize] }
// Gen-1 multi-byte values are BIG-ENDIAN.
#[inline] fn rd16_be(ram: &[u8], addr: u16) -> u16 {
    let o = (addr - 0xC000) as usize;
    u16::from_be_bytes([ram[o], ram[o + 1]])
}

let in_battle   = rd8(ram, 0xD057);                 // 0 = no battle
let player_hp   = rd16_be(ram, 0xD015);
let player_lvl  = rd8(ram, 0xD022);
let player_moves = [rd8(ram,0xD01C), rd8(ram,0xD01D), rd8(ram,0xD01E), rd8(ram,0xD01F)];
let player_pp    = [rd8(ram,0xD02D), rd8(ram,0xD02E), rd8(ram,0xD02F), rd8(ram,0xD030)];
let enemy_hp    = rd16_be(ram, 0xCFE6);
let enemy_lvl   = rd8(ram, 0xCFE8);
let enemy_max_hp = rd16_be(ram, 0xCFF4);
```
