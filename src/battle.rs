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
    Tap { button: b, hold: 4, gap: 6 }
}

/// Expand a high-level action into concrete taps for the Gen-1 battle menu. Assumes the main
/// FIGHT/PKMN/ITEM/RUN menu is showing with the cursor defaulted to FIGHT (true at turn start).
/// 2x2 grid: FIGHT top-left, PKMN bottom-left, ITEM top-right, RUN bottom-right.
pub fn action_to_taps(a: &AgentAction) -> Vec<Tap> {
    match a {
        AgentAction::Move { slot } => {
            let mut v = vec![tap(ID_A)];
            for _ in 0..(*slot).min(3) {
                v.push(tap(ID_DOWN));
            }
            v.push(tap(ID_A));
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
