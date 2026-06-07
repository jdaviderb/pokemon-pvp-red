//! Pokémon Red (Gen-1, DMG) battle-arena layer: read battle state out of WRAM, inject party/HP
//! for scenario setup, and turn high-level agent actions into per-frame button taps that drive
//! the in-battle menu. All addresses are CPU addresses in 0xC000..0xDFFF; see
//! docs/pokemon-red-ram-map.md. **Every multi-byte value is BIG-ENDIAN (Gen-1 quirk).**

use crate::n64::{ID_A, ID_B, ID_DOWN, ID_LEFT, ID_RIGHT, ID_UP};

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
    pub status: u8,     // bitfield: 0 = healthy
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
    pub menu: MenuPhase,          // software state machine
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
    Move { slot: u8 },                // FIGHT -> nth move (0..=3)
    Switch { slot: u8 },              // PKMN menu -> party slot (0..=5)
    Run,                              // RUN (wild only)
    Buttons { presses: Vec<String> }, // raw taps for scripting/navigation
}

// ---------- reader (pure; addresses verified live) ----------
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
/// software state machine since no single RAM flag marks "FIGHT list open".
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

/// Inject the PLAYER party header: count + species list (+ 0xFF terminator). Does NOT build the
/// full mon structs — for full-roster scenarios prefer capturing a savestate; this is for
/// count/species smoke tests.
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
#[derive(Clone, Copy, Debug)]
pub struct Tap {
    pub button: usize,
    pub hold: u8,
    pub gap: u8,
}

fn tap(b: usize) -> Tap {
    Tap { button: b, hold: 4, gap: 8 }
}
/// A tap with explicit timing — for menu navigation that must NOT drop a press.
fn tap_g(b: usize, hold: u8, gap: u8) -> Tap {
    Tap { button: b, hold, gap }
}

/// Expand a high-level action into concrete taps for the Gen-1 battle menu. Assumes the main
/// FIGHT/PKMN/ITEM/RUN menu is showing with the cursor defaulted to FIGHT (true at turn start).
/// 2x2 grid: FIGHT top-left, PKMN bottom-left, ITEM top-right, RUN bottom-right.
pub fn action_to_taps(a: &AgentAction) -> Vec<Tap> {
    match a {
        AgentAction::Move { slot } => {
            // Robust from ANY menu state (a stray cursor on ITEM/RUN, or stuck in a sub-menu like
            // the empty bag showing "CANCEL", would otherwise loop):
            //   B      -> back out of any open sub-menu to the top battle menu (no-op if already there)
            //   Left,Up-> home the 2x2 menu cursor to FIGHT (the menu doesn't wrap, so this is safe)
            //   A      -> open FIGHT (this resets the move-list cursor to slot 0)
            //   Down*n -> to the requested move; A confirms.
            // No Up-homing on the move list (it wraps; opening FIGHT already resets it to the top).
            // Generous, well-separated taps so the Gen-1 menu (edge-triggered cursor) never drops a
            // Down. A big "settle" gap after opening FIGHT lets the move list finish drawing before
            // the first Down, and each Down gets a wide gap so two presses can't land too close.
            let mut v = vec![tap(ID_B), tap(ID_LEFT), tap(ID_UP), tap_g(ID_A, 4, 20)];
            for _ in 0..(*slot).min(3) {
                v.push(tap_g(ID_DOWN, 5, 13));
            }
            v.push(tap_g(ID_A, 4, 8));
            v
        }
        AgentAction::Switch { slot } => {
            let mut v = vec![tap(ID_DOWN), tap(ID_A)];
            for _ in 0..(*slot).min(5) {
                v.push(tap(ID_DOWN));
            }
            v.push(tap(ID_A));
            v.push(tap(ID_A));
            v
        }
        AgentAction::Run => vec![tap(ID_DOWN), tap(ID_RIGHT), tap(ID_A)],
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
/// Owned by run_loop; driven once per frame. Reuses N64::set_button — the exact path the browser
/// input channel uses, so a human can still co-drive the same PAD bits.
#[derive(Default)]
pub struct TapMachine {
    queue: std::collections::VecDeque<Tap>,
    cur: Option<Tap>,
    left: i32,     // frames remaining in the current phase
    pressed: bool, // true = in hold phase, false = in gap phase
}

impl TapMachine {
    /// Queue a high-level action's taps.
    pub fn enqueue(&mut self, a: &AgentAction) {
        for t in action_to_taps(a) {
            self.queue.push_back(t);
        }
    }
    /// True while a macro is mid-flight (used to gate "ready for next action").
    pub fn busy(&self) -> bool {
        self.cur.is_some() || !self.queue.is_empty()
    }
    /// Drop everything (e.g. when leaving battle / after a state teleport).
    pub fn clear(&mut self, emu: &crate::n64::N64) {
        if let Some(t) = self.cur.take() {
            emu.set_button(t.button, false);
        }
        self.queue.clear();
        self.left = 0;
        self.pressed = false;
    }

    /// Advance one frame: call ONCE per frame, BEFORE emu.clock_frame().
    pub fn tick(&mut self, emu: &crate::n64::N64) {
        match &self.cur {
            Some(t) => {
                self.left -= 1;
                if self.left <= 0 {
                    if self.pressed {
                        emu.set_button(t.button, false);
                        self.pressed = false;
                        self.left = t.gap as i32;
                    } else {
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
/// Derive the menu phase each frame from D057 + whether a macro is in flight. v1 heuristic.
pub fn next_menu_phase(_prev: MenuPhase, ram: &[u8], macro_busy: bool) -> MenuPhase {
    if rd8(ram, 0xD057) == 0 {
        return MenuPhase::Overworld;
    }
    if macro_busy {
        return MenuPhase::Animating; // a player action / send-out is being played out
    }
    // wBattleMon species (D014) is 0 until the player mon is actually sent out — the send-out
    // animation runs for ~4 s, well past the input macro. Only call it MainMenu once the player
    // is on the field, so "wait for MainMenu" truly means the FIGHT menu is ready.
    if rd8(ram, 0xD014) == 0 {
        return MenuPhase::BattleIntro;
    }
    MenuPhase::MainMenu
}

// ======================================================================================
// Legendary / custom-matchup injection (proven in research/legendary-injection.md).
// Inject into states/legendary_intro.state (D057=2, neither side sent out) THEN resume:
// the engine's send-out routine draws the real sprites/names/cries from the injected data.
// ======================================================================================

/// Gen-1 internal species data for injection (NOT the Pokédex number).
#[derive(Clone, Copy)]
pub struct Gen1Species {
    pub name: &'static str,
    pub species: u8,          // internal index byte (struct +0)
    pub type1: u8,
    pub type2: u8,
    pub catch_rate: u8,       // struct +7
    pub base_stats: [u16; 5], // [HP, Atk, Def, Spd, Spc] BASE
    pub moves: [(u8, u8); 4], // (move_id, base_pp)
    pub growth: GrowthRate,
}

/// Gen-1 EXP growth groups (to seed a sane exp for `level` at struct +14).
#[derive(Clone, Copy)]
pub enum GrowthRate {
    Fast,
    MediumFast,
    MediumSlow,
    Slow,
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

/// Selectable species table. The internal index is the dropdown value the browser POSTs.
/// The full 151-species Gen-1 table is auto-generated in `src/species_data.rs` (from pret/pokered).
pub use crate::species_data::SPECIES;

/// Find a species row by its internal index (the value the browser POSTs).
pub fn species_by_index(idx: u8) -> Option<&'static Gen1Species> {
    SPECIES.iter().find(|s| s.species == idx)
}

/// Gen-1 stat formula, DV=15, stat-EXP=0:
/// stat = floor((base+DV)*2*level/100) + 5 ; HP = same + level + 10.
const DV: u32 = 15;
fn calc_hp(base: u16, level: u8) -> u16 {
    let l = level as u32;
    ((((base as u32 + DV) * 2 * l) / 100) + l + 10) as u16
}
fn calc_stat(base: u16, level: u8) -> u16 {
    let l = level as u32;
    ((((base as u32 + DV) * 2 * l) / 100) + 5) as u16
}

/// Experience to *be* `level` in each growth group (24-bit, written BE at struct +14).
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

/// Encode a Gen-1 nickname: 11 bytes, 0x50 terminator/padding (constants/charmap.asm).
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

/// Build a full 44-byte wPartyMon / wEnemyMon struct for `sp` at `level` (all multibyte fields BE).
pub fn build_party_mon(sp: &Gen1Species, level: u8) -> [u8; 44] {
    let mut m = [0u8; 44];
    let be = |v: u16| v.to_be_bytes();
    let hp = calc_hp(sp.base_stats[0], level);
    let atk = calc_stat(sp.base_stats[1], level);
    let def = calc_stat(sp.base_stats[2], level);
    let spd = calc_stat(sp.base_stats[3], level);
    let spc = calc_stat(sp.base_stats[4], level);

    m[0] = sp.species;
    let h = be(hp);
    m[1] = h[0];
    m[2] = h[1]; // +1 current HP (= max)
    m[3] = level; // +3 box-level (cosmetic)
    m[4] = 0x00; // +4 status
    m[5] = sp.type1;
    m[6] = sp.type2;
    m[7] = sp.catch_rate;
    for i in 0..4 {
        m[8 + i] = sp.moves[i].0; // +8..+11 move ids
    }
    m[12] = 0x00;
    m[13] = 0x00; // +12 OT id
    let e = exp_for_level(sp.growth, level).to_be_bytes(); // u32 -> [hi..lo]
    m[14] = e[1];
    m[15] = e[2];
    m[16] = e[3]; // +14..+16 exp (24-bit BE)
    // +17..+26 stat-EXP all 0
    m[27] = 0xFF; // +27 Atk/Def DV = 15/15
    m[28] = 0xFF; // +28 Spd/Spc DV = 15/15
    for i in 0..4 {
        m[29 + i] = sp.moves[i].1; // +29..+32 PP
    }
    m[33] = level; // +33 LIVE level
    let mx = be(hp);
    m[34] = mx[0];
    m[35] = mx[1];
    let a = be(atk);
    m[36] = a[0];
    m[37] = a[1];
    let d = be(def);
    m[38] = d[0];
    m[39] = d[1];
    let s = be(spd);
    m[40] = s[0];
    m[41] = s[1];
    let p = be(spc);
    m[42] = p[0];
    m[43] = p[1];
    m
}

fn write_party_mon(ram: &mut [u8], base: u16, mon: &[u8; 44]) {
    let off = (base - 0xC000) as usize;
    ram[off..off + 44].copy_from_slice(mon);
}
fn write_nick(ram: &mut [u8], base: u16, nick: &[u8; 11]) {
    let off = (base - 0xC000) as usize;
    ram[off..off + 11].copy_from_slice(nick);
}

// verified single-mon party offsets (slot 0)
const P_PARTY_COUNT: u16 = 0xD163;
const P_PARTY_SPECIES: u16 = 0xD164;
const P_MON0: u16 = 0xD16B;
const P_NICK0: u16 = 0xD2B5;
const E_PARTY_COUNT: u16 = 0xD89C;
const E_PARTY_SPECIES: u16 = 0xD89D;
const E_MON0: u16 = 0xD8A4;
const E_NICK0: u16 = 0xD9EE;

/// Set up a 1-vs-1 custom matchup in WRAM. Call AFTER load_state(legendary_intro.state) and BEFORE
/// resuming (intro is pre-send-out: D057=2, D014=0, CFE5=0). The engine then sends out both mons
/// with correct sprites/names/cries.
pub fn setup_matchup(
    ram: &mut [u8],
    player: &Gen1Species,
    enemy: &Gen1Species,
    level: u8,
    player_nick: &str,
    enemy_nick: &str,
) {
    // Custom names override the species name; empty falls back to the species name.
    let pnick = if player_nick.is_empty() { player.name } else { player_nick };
    let enick = if enemy_nick.is_empty() { enemy.name } else { enemy_nick };

    wr8(ram, P_PARTY_COUNT, 1);
    wr8(ram, P_PARTY_SPECIES, player.species);
    wr8(ram, P_PARTY_SPECIES + 1, 0xFF);
    write_party_mon(ram, P_MON0, &build_party_mon(player, level));
    write_nick(ram, P_NICK0, &encode_name(pnick)); // REQUIRED: player name is the nick

    // OBEDIENCE: an injected Lv mon whose OT (trainer) doesn't match the player counts as "traded"
    // and disobeys above the badge level cap ("POKéMON won't obey!"). Two belt-and-suspenders fixes:
    //   1) grant all 8 badges (wObtainedBadges D356 = 0xFF) -> obedience cap rises to Lv100, and
    //   2) stamp the mon's OT id (struct +12, BE) with the player's trainer id (wPlayerID D359),
    //      so it's considered caught-by-you, not traded.
    wr8(ram, 0xD356, 0xFF);
    let pid_hi = rd8(ram, 0xD359);
    let pid_lo = rd8(ram, 0xD35A);
    wr8(ram, P_MON0 + 12, pid_hi);
    wr8(ram, P_MON0 + 13, pid_lo);

    wr8(ram, E_PARTY_COUNT, 1);
    wr8(ram, E_PARTY_SPECIES, enemy.species);
    wr8(ram, E_PARTY_SPECIES + 1, 0xFF);
    write_party_mon(ram, E_MON0, &build_party_mon(enemy, level));
    write_nick(ram, E_NICK0, &encode_name(enick));
}

/// Lightweight species list for the browser dropdowns: (internal_index, NAME).
pub fn species_menu() -> Vec<(u8, &'static str)> {
    SPECIES.iter().map(|s| (s.species, s.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // slot 2 => B, Left, Up, A, Down, Down, A  (home to FIGHT, then 2 downs, confirm)
        let taps = action_to_taps(&AgentAction::Move { slot: 2 });
        assert_eq!(taps.first().unwrap().button, ID_B); // backs out of any sub-menu first
        assert_eq!(taps.last().unwrap().button, ID_A); // confirm
        assert_eq!(taps.iter().filter(|t| t.button == ID_DOWN).count(), 2); // exactly `slot` downs
    }
    #[test]
    fn articuno_lv50_struct() {
        let a = species_by_index(0x4A).unwrap();
        assert_eq!(calc_hp(a.base_stats[0], 50), 165);
        assert_eq!(calc_stat(a.base_stats[4], 50), 145); // special
        let m = build_party_mon(a, 50);
        assert_eq!(m[0], 0x4A); // species index
        assert_eq!(m[33], 50); // live level
        assert_eq!(u16::from_be_bytes([m[34], m[35]]), 165); // max HP (BE)
        assert_eq!(m[27], 0xFF); // Atk/Def DVs = 15
    }
}
