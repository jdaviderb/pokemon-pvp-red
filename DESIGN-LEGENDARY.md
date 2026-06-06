# DESIGN-LEGENDARY — Spawn a custom legendary matchup (Articuno vs Zapdos, Lv50)

Concrete, copy-pasteable plan to add a "Start Matchup" feature: pick two Gen-1 species + a level,
and the engine sends them out **with correct sprites + names + cries** in the Pokémon Red battle
arena. Built on the two probes:

- `research/legendary-gen1-data.md` — internal indices, base stats, struct, stat formula, nicknames.
- `research/legendary-injection.md` — empirical intro-capture + injection proof (Articuno vs Zapdos
  rendered byte-for-byte; trainer ROM-reread does NOT clobber the enemy injection).

All addresses are CPU addresses in `0xC000..0xDFFF`; the WRAM slice index is `addr - 0xC000`.
**Every multi-byte HP/stat value is BIG-ENDIAN (Gen-1 quirk).**

---

## 1. Verdict

**YES — the intro-savestate + party-injection produces a correct-sprite Articuno-vs-Zapdos fight,
on BOTH sides.** This is the approach the probe PROVED, and it is the one we ship.

The trainer-clobber worry is answered empirically (`legendary-injection.md` §"trainer-battle
clobber question"): the trainer's party is read from ROM into WRAM (`wEnemyPartyMons @ D8A4`,
`wEnemyPartySpecies @ D89D`) **very early** in battle init — already present at the first frame
`D057 != 0`. The per-mon send-out routine (`LoadEnemyMon`) builds `wEnemyMon (CFE5..)` from
`wEnemyPartyMons[wWhichPokemon]` **in WRAM**, NOT by re-reading ROM. So overwriting
`wEnemyPartySpecies[0]` + `wEnemyMon[0]` in the captured intro state (after the trainer load,
before send-out) sticks: `CFE5` became `0x4B` (Zapdos) and never reverted to `0xB1` (Squirtle).

Proof readback from the probe (deterministic, reproduced across a fresh process AND a second
species pair Moltres/Dragonite):

```
cand_018 (intro) loaded:  D014=0  CFE5=0   D89D=177  D164=176   (pre send-out)
inject:                   D163=1 D164=0x4A(Articuno) | D89C=1 D89D=0x4B(Zapdos)
run send-out (tap A):
  CFE5 0 -> 75 (0x4B Zapdos)     enemy sent out first
  D014 0 -> 74 (0x4A Articuno)   player sent out
POST:  D014=74 MATCH=true | CFE5=75 MATCH=true | pLvl=50 pHP=150 eHP=150
```

**Sprite/name/cry behavior (must-do, from the probe):**

- **Sprite** (enemy front, player back) and **cry** are drawn from the species **internal index**
  at send-out. Inject the index -> correct sprite + cry. Proven visually for both sides.
- **Enemy name** auto-corrects from the species' base name string — no nickname write needed.
- **Player name** comes from `wPartyMonNicks[0] (D2B5)`. You **MUST** write a matching nickname,
  or a stale nick is displayed (probe saw species=Moltres but stale "ARTICUNO" on screen). So:
  always write `wPartyMonNicks[0]` to match the player species. Writing `wEnemyMonNicks (D9EE)` is
  harmless and we do it for consistency.

**No fallback is needed.** Wild-battle intro would also work (single enemy, no trainer list) but is
NOT required — the proven trainer-battle intro handles both sides. We do not use "active-only"
injection (the inferior path with the stale-sprite caveat); we inject the full party slot + species
list + count + nickname at the verified pre-send-out window, exactly as proven.

**Hard precondition (Risk, see §6):** the savestate is ROM-specific to `Pokemon Red.gb`. The server
must be launched with that ROM (`cargo run --release -- "Pokemon Red.gb"`), NOT the default
`Pokemon Red Color.gbc`. `/battle/setup` returns an error if the loaded ROM's WRAM doesn't match.

---

## 2. `src/battle.rs` additions — full code

Append the following to `src/battle.rs`. It reuses the existing `wr8` / `wr16_be` helpers already
in the file. It adds: a `Gen1Species` table (extensible), the Lv-aware Gen-1 stat formula (DV=15),
`build_party_mon`, the nickname encoder, and `setup_matchup` that writes both sides at the verified
offsets.

```rust
// ======================================================================================
// Legendary / custom-matchup injection (proven in research/legendary-injection.md).
// Inject into states/legendary_intro.state (D057=2, neither side sent out) THEN resume:
// the engine's send-out routine draws the real sprites/names/cries from the injected data.
// ======================================================================================

/// Gen-1 internal species data for injection (NOT the Pokédex number).
/// `base_stats` are the species BASE stats [HP, Atk, Def, Spd, Spc] — live stats are computed
/// from these + level via the Gen-1 formula (DV=15, stat-EXP=0). Extend `SPECIES` to add more.
#[derive(Clone, Copy)]
pub struct Gen1Species {
    pub name: &'static str,        // for the picker / nickname encoding
    pub species: u8,               // internal index byte (struct +0)
    pub type1: u8,
    pub type2: u8,
    pub catch_rate: u8,            // struct +7
    pub base_stats: [u16; 5],      // [HP, Atk, Def, Spd, Spc] BASE (not level-scaled)
    pub moves: [(u8, u8); 4],      // (move_id, base_pp) -> +8..+11 ids, +29..+32 PP
    pub growth: GrowthRate,        // for the +14 experience field
}

/// Gen-1 EXP growth groups (only what we need to seed a sane exp for `level`).
#[derive(Clone, Copy)]
pub enum GrowthRate {
    Fast,        // 4n^3/5
    MediumFast,  // n^3
    MediumSlow,  // 6/5 n^3 - 15 n^2 + 100 n - 140
    Slow,        // 5n^3/4
}

/// Gen-1 type ids (constants/type_constants.asm).
pub mod gen1_type {
    pub const NORMAL: u8 = 0x00;
    pub const FLYING: u8 = 0x02;
    pub const FIRE: u8 = 0x14;
    pub const WATER: u8 = 0x15;
    pub const GRASS: u8 = 0x16;
    pub const ELECTRIC: u8 = 0x17;
    pub const PSYCHIC: u8 = 0x18;
    pub const ICE: u8 = 0x19;
    pub const DRAGON: u8 = 0x1A;
}

/// The selectable species table. Index here is the dropdown value the browser POSTs.
/// Add rows to grow the picker — everything downstream is data-driven off this table.
pub static SPECIES: &[Gen1Species] = &[
    Gen1Species {
        name: "ARTICUNO",
        species: 0x4A,
        type1: gen1_type::ICE,
        type2: gen1_type::FLYING,
        catch_rate: 3,
        base_stats: [90, 85, 100, 85, 125],
        // IceBeam, Blizzard, Peck, Agility
        moves: [(0x3A, 10), (0x3B, 5), (0x40, 35), (0x61, 30)],
        growth: GrowthRate::Slow,
    },
    Gen1Species {
        name: "ZAPDOS",
        species: 0x4B,
        type1: gen1_type::ELECTRIC,
        type2: gen1_type::FLYING,
        catch_rate: 3,
        base_stats: [90, 90, 85, 100, 125],
        // Thunderbolt, Thunder, DrillPeck, Agility
        moves: [(0x55, 15), (0x57, 10), (0x41, 20), (0x61, 30)],
        growth: GrowthRate::Slow,
    },
    Gen1Species {
        name: "MOLTRES",
        species: 0x49,
        type1: gen1_type::FIRE,
        type2: gen1_type::FLYING,
        catch_rate: 3,
        base_stats: [90, 100, 90, 90, 125],
        // FireSpin, Leer, Agility, Sky Attack
        moves: [(0x53, 15), (0x2B, 30), (0x61, 30), (0x8F, 5)],
        growth: GrowthRate::Slow,
    },
    Gen1Species {
        name: "DRAGONITE",
        species: 0x42,
        type1: gen1_type::DRAGON,
        type2: gen1_type::FLYING,
        catch_rate: 9,
        base_stats: [91, 134, 95, 80, 100],
        // Wrap, Leer, Thunder Wave, Agility
        moves: [(0x23, 20), (0x2B, 30), (0x56, 20), (0x61, 30)],
        growth: GrowthRate::Slow,
    },
];

/// Find a species row by its internal index (the value the browser POSTs).
pub fn species_by_index(idx: u8) -> Option<&'static Gen1Species> {
    SPECIES.iter().find(|s| s.species == idx)
}

/// Gen-1 stat formula, DV=15, stat-EXP=0 (research/legendary-gen1-data.md §3).
/// stat = floor((base + DV) * 2 * level / 100) + 5
/// HP   = floor((base + DV) * 2 * level / 100) + level + 10
const DV: u32 = 15;

fn calc_hp(base: u16, level: u8) -> u16 {
    let l = level as u32;
    ((((base as u32 + DV) * 2 * l) / 100) + l + 10) as u16
}
fn calc_stat(base: u16, level: u8) -> u16 {
    let l = level as u32;
    ((((base as u32 + DV) * 2 * l) / 100) + 5) as u16
}

/// Experience needed to *be* `level` in each growth group (24-bit; written BE at struct +14).
/// Keeps the engine from treating the mon as underleveled if it recalculates.
fn exp_for_level(growth: GrowthRate, level: u8) -> u32 {
    let n = level as i64;
    let e = match growth {
        GrowthRate::Fast => (4 * n * n * n) / 5,
        GrowthRate::MediumFast => n * n * n,
        GrowthRate::MediumSlow => (6 * n * n * n) / 5 - 15 * n * n + 100 * n - 140,
        GrowthRate::Slow => (5 * n * n * n) / 4,
    };
    e.max(0) as u32
}

/// Encode a Gen-1 nickname: 11 bytes, 0x50 terminator, 0x50 padding
/// (constants/charmap.asm). 'A'..'Z' = 0x80.., 'a'..'z' = 0xA0.., ' ' = 0x7F, '0'..'9' = 0xF6..
pub fn encode_name(s: &str) -> [u8; 11] {
    let mut out = [0x50u8; 11];
    for (i, c) in s.bytes().enumerate().take(10) {
        out[i] = match c {
            b'A'..=b'Z' => 0x80 + (c - b'A'),
            b'a'..=b'z' => 0xA0 + (c - b'a'),
            b' ' => 0x7F,
            b'0'..=b'9' => 0xF6 + (c - b'0'),
            _ => 0x50,
        };
    }
    out
}

/// Build a full 44-byte wPartyMon / wEnemyMon struct for `sp` at `level`.
/// Layout per research/legendary-gen1-data.md §5; all multibyte fields BIG-ENDIAN.
pub fn build_party_mon(sp: &Gen1Species, level: u8) -> [u8; 44] {
    let mut m = [0u8; 44];
    let be = |v: u16| v.to_be_bytes();

    let hp = calc_hp(sp.base_stats[0], level);
    let atk = calc_stat(sp.base_stats[1], level);
    let def = calc_stat(sp.base_stats[2], level);
    let spd = calc_stat(sp.base_stats[3], level);
    let spc = calc_stat(sp.base_stats[4], level);

    m[0] = sp.species; // +0 species (internal index)
    let h = be(hp);
    m[1] = h[0];
    m[2] = h[1]; // +1 current HP (= max)
    m[3] = level; // +3 box-level scratch (cosmetic)
    m[4] = 0x00; // +4 status = healthy
    m[5] = sp.type1; // +5 type1
    m[6] = sp.type2; // +6 type2
    m[7] = sp.catch_rate; // +7 catch rate / held item
    for i in 0..4 {
        m[8 + i] = sp.moves[i].0; // +8..+11 move ids
    }
    m[12] = 0x00;
    m[13] = 0x00; // +12 OT id (BE) — any value
    let e = exp_for_level(sp.growth, level).to_be_bytes(); // u32 -> [hi..lo]
    m[14] = e[1];
    m[15] = e[2];
    m[16] = e[3]; // +14..+16 experience (24-bit BE)
    // +17..+26 stat-EXP all 0 (already zeroed)
    m[27] = 0xFF; // +27 Atk/Def DV = 15/15
    m[28] = 0xFF; // +28 Spd/Spc DV = 15/15
    for i in 0..4 {
        m[29 + i] = sp.moves[i].1; // +29..+32 PP (no PP-Up)
    }
    m[33] = level; // +33 LIVE level
    let mx = be(hp);
    m[34] = mx[0];
    m[35] = mx[1]; // +34 max HP
    let a = be(atk);
    m[36] = a[0];
    m[37] = a[1]; // +36 attack
    let d = be(def);
    m[38] = d[0];
    m[39] = d[1]; // +38 defense
    let s = be(spd);
    m[40] = s[0];
    m[41] = s[1]; // +40 speed
    let p = be(spc);
    m[42] = p[0];
    m[43] = p[1]; // +42 special
    m
}

/// Write a prebuilt 44-byte struct into `ram` at CPU `base` (e.g. 0xD16B / 0xD8A4).
fn write_party_mon(ram: &mut [u8], base: u16, mon: &[u8; 44]) {
    let off = (base - 0xC000) as usize;
    ram[off..off + 44].copy_from_slice(mon);
}

/// Write an 11-byte nickname into `ram` at CPU `base` (D2B5 player / D9EE enemy, slot 0).
fn write_nick(ram: &mut [u8], base: u16, nick: &[u8; 11]) {
    let off = (base - 0xC000) as usize;
    ram[off..off + 11].copy_from_slice(nick);
}

// --- verified single-mon party offsets (slot 0) from the probe ---
const P_PARTY_COUNT: u16 = 0xD163;
const P_PARTY_SPECIES: u16 = 0xD164;
const P_MON0: u16 = 0xD16B;
const P_NICK0: u16 = 0xD2B5;
const E_PARTY_COUNT: u16 = 0xD89C;
const E_PARTY_SPECIES: u16 = 0xD89D;
const E_MON0: u16 = 0xD8A4;
const E_NICK0: u16 = 0xD9EE;

/// Set up a 1-vs-1 custom matchup in WRAM. Call this AFTER load_state(legendary_intro.state)
/// and BEFORE resuming (the intro is pre-send-out: D057=2, D014=0, CFE5=0). The engine then
/// sends out both mons with correct sprites/names/cries (proof: research/legendary-injection.md).
pub fn setup_matchup(
    ram: &mut [u8],
    player: &Gen1Species,
    enemy: &Gen1Species,
    level: u8,
) {
    // Player side -> drives wBattleMon.species (D014) after send-out.
    wr8(ram, P_PARTY_COUNT, 1);
    wr8(ram, P_PARTY_SPECIES, player.species);
    wr8(ram, P_PARTY_SPECIES + 1, 0xFF); // list terminator
    write_party_mon(ram, P_MON0, &build_party_mon(player, level));
    write_nick(ram, P_NICK0, &encode_name(player.name)); // REQUIRED: player name is the nick

    // Enemy side -> drives wEnemyMon.species (CFE5) after send-out.
    wr8(ram, E_PARTY_COUNT, 1);
    wr8(ram, E_PARTY_SPECIES, enemy.species);
    wr8(ram, E_PARTY_SPECIES + 1, 0xFF);
    write_party_mon(ram, E_MON0, &build_party_mon(enemy, level));
    write_nick(ram, E_NICK0, &encode_name(enemy.name)); // harmless; engine uses species name
}

/// Lightweight species list for the browser dropdowns: (internal_index, NAME).
pub fn species_menu() -> Vec<(u8, &'static str)> {
    SPECIES.iter().map(|s| (s.species, s.name)).collect()
}
```

**Verification of the formula at Lv50, DV=15** (matches `legendary-gen1-data.md` §3):
`calc_hp(90,50) = (90+15)*2*50/100 + 50 + 10 = 105 + 60 = 165` ✓;
`calc_stat(125,50) = (125+15)*2*50/100 + 5 = 140 + 5 = 145` ✓ (Articuno/Zapdos Special).
Articuno -> `[165,105,120,105,145]`, Zapdos -> `[165,110,105,120,145]` ✓.

> **Move data note.** Articuno/Zapdos movesets + PP are probe-verified
> (`legendary-gen1-data.md` §4). Moltres/Dragonite movesets in the table above are sane Lv50
> placeholders (valid move ids + PP); verify against pret if you want their exact level-up sets.
> The matchup renders correctly regardless — moves only affect mechanics, not the sprite/name/cry.

---

## 3. Bootstrap: the intro savestate

The probe already captured and shipped the winning intro state:

- **File:** `states/legendary_intro.state`
  (verified present, **md5 `f28cea749f3ff4e83a640c8487d8ee8f`, 59650 bytes** — identical to the
  probe's `cand_018.state`).
- **Markers at that frame:** `D057=2` (trainer battle), `D014=0` & `CFE5=0` (neither sent out),
  `D89C=1`, `D89D=177` (trainer party already loaded into WRAM).

> **`states/` is gitignored** (see `.gitignore`). To ship this state, either (a) commit it with a
> `!states/legendary_intro.state` un-ignore rule, or (b) document that the operator must keep the
> file locally. The file already exists on this machine.

**The `/battle/setup` flow on the emu thread** (same ownership model as save/load — runs on the one
thread that owns `N64`):

```
1. load_state(read("states/legendary_intro.state"))   // teleport to pre-send-out intro
2. with_system_ram_mut(|ram| setup_matchup(ram, player, enemy, level))   // inject both sides
3. taps.clear(&emu)                                    // drop any in-flight macro
4. enqueue ~6 "A" taps over the next frames to advance "<RIVAL> wants to fight!" / "Go!"
5. resume the normal loop; send-out fires -> D014/CFE5 become the injected indices,
   sprites/names/cries are correct, FIGHT menu follows.
```

To recreate the state from scratch if it is ever lost, re-run the probe (`research/legendary-probe`,
mode `introcap`) per `legendary-injection.md` §Reproduce, then copy `cand_018.state` to
`states/legendary_intro.state`.

---

## 4. `src/pipeline.rs` + `src/signaling.rs` — diff points

Add a new emu-thread channel that carries a (player, enemy, level) request, handled right next to
`save_rx` / `load_rx` so it stays on the thread that owns `N64`.

### 4a. `src/pipeline.rs`

**(i) Add a request type + reply channel alias** (near `SaveReq` / `LoadReq`, ~line 18):

```rust
/// /battle/setup payload, resolved on the emu thread. Reply = Ok / why-not.
pub struct SetupReq {
    pub player: u8, // internal species index
    pub enemy: u8,  // internal species index
    pub level: u8,
    pub reply: tokio::sync::oneshot::Sender<Result<(), String>>,
}
```

**(ii) Add the sender to `AppInner`** (in the struct, after `load_tx`, ~line 54):

```rust
    pub setup_tx: mpsc::UnboundedSender<SetupReq>,
```

**(iii) Create the channel + pass it into the thread** (in `start`, ~line 65):

```rust
    let (setup_tx, setup_rx) = mpsc::unbounded_channel::<SetupReq>();
```

Then add `setup_rx` to the `run_loop(...)` call arg list and add `setup_tx` to the returned
`AppInner { ... }`.

**(iv) Extend `run_loop`'s signature** (~line 122) with:

```rust
    mut setup_rx: mpsc::UnboundedReceiver<SetupReq>,
```

**(v) Handle setup requests in the loop** — place it in step "0a" right after the `load_rx` block
(~line 171), so it runs on the emu thread and clears taps like load does:

```rust
        // 0a'. Custom-matchup setup: load the intro state, inject both party slots, then
        //      queue A-taps to drive the send-out. Emu-thread only (owns `N64`).
        while let Ok(req) = setup_rx.try_recv() {
            let res = (|| -> Result<(), String> {
                let player = crate::battle::species_by_index(req.player)
                    .ok_or_else(|| format!("unknown player species {}", req.player))?;
                let enemy = crate::battle::species_by_index(req.enemy)
                    .ok_or_else(|| format!("unknown enemy species {}", req.enemy))?;
                let level = req.level.clamp(1, 100);

                let state = std::fs::read("states/legendary_intro.state")
                    .map_err(|e| format!("no states/legendary_intro.state: {e}"))?;
                if !emu.load_state(&state) {
                    return Err("load_state failed (wrong ROM? need Pokemon Red.gb)".into());
                }
                // Sanity: the intro must be pre-send-out (D057=2, D014=0, CFE5=0). If not, the
                // ROM/state mismatch (e.g. server launched with the .gbc romhack).
                let ok_intro = emu
                    .with_system_ram(|ram| {
                        ram[(0xD057 - 0xC000) as usize] != 0
                            && ram[(0xD014 - 0xC000) as usize] == 0
                            && ram[(0xCFE5 - 0xC000) as usize] == 0
                    })
                    .unwrap_or(false);
                if !ok_intro {
                    return Err("intro markers wrong — savestate/ROM mismatch".into());
                }
                emu.with_system_ram_mut(|ram| {
                    crate::battle::setup_matchup(ram, player, enemy, level);
                });
                Ok(())
            })();

            if res.is_ok() {
                taps.clear(&emu);
                menu = MenuPhase::BattleIntro;
                // Drive the intro text to the send-out: a handful of A taps.
                let advance = AgentAction::Buttons {
                    presses: vec!["A".into(), "A".into(), "A".into(), "A".into(), "A".into(), "A".into()],
                };
                taps.enqueue(&advance);
            }
            let _ = req.reply.send(res);
        }
```

> The A-taps reuse the existing `TapMachine` + `action_to_taps` path. If a particular intro needs
> more/fewer presses to clear "wants to fight!" before the FIGHT menu, tune the count here; six 4f
> taps with 6f gaps span ~60 frames, comfortably covering the send-out per the probe.

### 4b. `src/signaling.rs`

**(i) Add the route** (in `router`, after `/battle/load`, ~line 46):

```rust
        .route("/battle/setup", post(battle_setup_handler))
```

**(ii) Request struct + handler** (append near the other battle handlers):

```rust
#[derive(Deserialize)]
pub struct SetupRequest {
    pub player: u8, // internal species index
    pub enemy: u8,  // internal species index
    #[serde(default = "default_level")]
    pub level: u8,
}
fn default_level() -> u8 {
    50
}

/// GET /battle/species -> the selectable species table for the dropdowns: [[index,"NAME"], ...].
async fn battle_species_handler() -> Json<Vec<(u8, &'static str)>> {
    Json(crate::battle::species_menu())
}

/// POST /battle/setup  body: {"player":74,"enemy":75,"level":50}
/// Loads the intro savestate, injects both party slots, drives the send-out. 200 = matchup live.
async fn battle_setup_handler(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.setup_tx.send(crate::pipeline::SetupReq {
        player: req.player,
        enemy: req.enemy,
        level: req.level,
        reply: tx,
    });
    match rx.await {
        Ok(Ok(())) => Ok(StatusCode::OK),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, e)),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR, "emu thread gone".into())),
    }
}
```

**(iii) Register the species route too** (in `router`):

```rust
        .route("/battle/species", get(battle_species_handler))
```

(`get` and `post` are already imported.)

---

## 5. `static/index.html` — two dropdowns + level + "Start Matchup"

**(i) Markup** — add a second row inside the `.arena-bar` (after the existing buttons, ~line 274):

```html
      <span id="arenaMsg"></span>
    </div>
    <!-- custom matchup picker -->
    <div class="arena-bar" style="margin-top:10px">
      <span class="arena-title" style="font-size:12px">MATCHUP</span>
      YOU <select id="selPlayer"></select>
      vs <select id="selEnemy"></select>
      Lv <input id="selLevel" type="number" min="1" max="100" value="50"
               style="width:54px;background:#0b0b11;color:#e8e8f0;border:1px solid #2c2c3a;border-radius:6px;padding:5px" />
      <button id="btnMatchup">Start Matchup</button>
    </div>
```

Style the `<select>`s once (add to the `.arena` CSS block, ~line 208):

```css
    .arena select {
      background: #0b0b11; color: #e8e8f0; border: 1px solid #2c2c3a;
      border-radius: 6px; padding: 5px 8px; font: inherit;
    }
```

**(ii) JS** — populate the dropdowns from `/battle/species` and wire the button. Add to the AI
battle-console `<script>` (after the `act(...)` helper, ~line 467):

```javascript
  // Populate species dropdowns from the server table, then wire "Start Matchup".
  async function loadSpecies() {
    const r = await api("/battle/species");
    if (!r || !r.ok) return;
    const list = await r.json(); // [[index,"NAME"], ...]
    const opts = list.map(([idx, name]) =>
      `<option value="${idx}">${name}</option>`).join("");
    $("selPlayer").innerHTML = opts;
    $("selEnemy").innerHTML = opts;
    // Default to Articuno (player) vs Zapdos (enemy) if present.
    $("selPlayer").value = "74";
    $("selEnemy").value = "75";
    // Let the species table also feed the readout name map.
    list.forEach(([idx, name]) => { SPECIES[idx] = name.charAt(0) + name.slice(1).toLowerCase(); });
  }
  $("btnMatchup").onclick = async () => {
    const player = +$("selPlayer").value;
    const enemy = +$("selEnemy").value;
    const level = Math.max(1, Math.min(100, +$("selLevel").value || 50));
    msg("starting matchup…");
    const r = await api("/battle/setup", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ player, enemy, level }),
    });
    if (!r) msg("no server");
    else if (r.ok) msg("matchup live ✓");
    else msg("setup failed (" + r.status + "): " + (await r.text()));
  };
  loadSpecies();
```

> Note: `SPECIES` in the existing JS is `const`, but it's a mutable object literal, so assigning
> new keys (`SPECIES[idx] = ...`) is fine and makes the readout label the legendaries correctly.

---

## 6. Build / run / try it + Risks

### Build & run

```bash
cd ~/pokemon-pvp-red
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
export LIBRARY_PATH=/opt/homebrew/lib
# IMPORTANT: launch with the grayscale ROM the savestate was captured against.
cargo run --release -- "Pokemon Red.gb"
# (or:  ./run.sh "Pokemon Red.gb")
```

### Try Articuno vs Zapdos

1. Open http://localhost:3000, click **POWER**, wait for "connected · playing".
2. In the AGENT BATTLE CONSOLE, MATCHUP row: YOU = **ARTICUNO**, vs = **ZAPDOS**, Lv **50**.
3. Click **Start Matchup**. Expect "matchup live ✓".
4. On screen: enemy **ZAPDOS** front sprite sent out, then **Go! ARTICUNO!** back sprite. The
   readout shows `YOU ARTICUNO L50 / ENEMY ZAPDOS L50`, HP 165/165 both. FIGHT menu follows; the
   move buttons drive the battle.

CLI smoke test (no browser):

```bash
curl -s localhost:3000/battle/species          # [[74,"ARTICUNO"],[75,"ZAPDOS"],...]
curl -s -XPOST localhost:3000/battle/setup \
  -H 'content-type: application/json' \
  -d '{"player":74,"enemy":75,"level":50}'      # 200 = live; 400 body = why-not
curl -s localhost:3000/battle/state | jq '.player.species, .enemy.species, .player.hp'
# expect 74, 75, 165 once send-out completes
```

### Risks (and mitigations)

1. **Savestate ROM-specificity (highest risk).** `legendary_intro.state` was captured against
   `Pokemon Red.gb`. The default server ROM is `Pokemon Red Color.gbc` (a GBC romhack) — loading the
   state there will fail or land in garbage WRAM. **Mitigation:** the handler returns a 400 with a
   clear message if `load_state` fails or the intro markers (`D057!=0 && D014==0 && CFE5==0`) are
   wrong; always launch with `Pokemon Red.gb`. (A separate `.gbc` intro state could be captured later
   with the same probe if color is desired.)
2. **`states/` is gitignored.** The state won't ship via git by default. **Mitigation:** add a
   `!states/legendary_intro.state` un-ignore rule, or document keeping it locally (it exists now,
   md5 `f28cea749f3ff4e83a640c8487d8ee8f`).
3. **Internal-index correctness.** Indices are NOT Pokédex numbers. Articuno=0x4A, Zapdos=0x4B,
   Moltres=0x49, Dragonite=0x42 are **empirically verified** (read back from `D014`/`CFE5`). Any
   NEW species added to `SPECIES` must use the pokered internal index, not the dex number.
4. **Struct-field exactness.** The 44-byte layout is verified; the load-bearing fields for a clean
   send-out are species(+0), curHP/maxHP(+1/+34, BE), type1/2(+5/+6), moves(+8..+11), DV(+27/+28),
   PP(+29..+32), level(+33), stats(+34..+43, BE). Endianness mistakes show as absurd HP/stats — the
   `read_battle_state` BE readers + the `hp_is_big_endian` test guard this.
5. **Stat formula.** Uses integer floor division, DV=15, EV=0. Verified equal to the probe's Lv50
   table. Levels other than 50 are computed correctly by the same formula (e.g. Lv70 Articuno HP =
   `(105)*2*70/100 + 70 + 10 = 147 + 80 = 227`); they are NOT in the proven matchup but follow the
   same code path.
6. **Send-out timing for injection.** Inject ONLY at the pre-send-out window (`D014==0 && CFE5==0`),
   which is exactly what `legendary_intro.state` captures. Injecting after send-out (cand_019+) is
   too late for a clean enemy injection. The six A-taps after injection advance the intro text into
   the send-out; tune the count if a given intro needs more/fewer presses. The player nickname write
   (`D2B5`) is mandatory — skipping it shows a stale on-screen name.
7. **Single emulator / shared globals.** Setup runs on the emu thread (like save/load), so there's
   no data race on `N64` or WRAM; the HTTP handler just awaits a oneshot reply.
