//! Bloody Roar 2 (PlayStation) RAM reader + match-state detection for the fighting-game arena.
//!
//! PSX main RAM is 2 MiB and is exposed by the libretro core as `RETRO_MEMORY_SYSTEM_RAM`
//! (`Emu::with_system_ram`). Unlike the Gen-1 Game Boy path in `battle.rs` (big-endian), the MIPS
//! R3000 is **little-endian**, so multi-byte values use `from_le_bytes`. GameShark-style PSX
//! addresses (`0x800XXXXX`, KSEG0/KSEG1/KUSEG) all alias the same physical RAM — mask the low 21
//! bits to get the offset.

pub const PSX_RAM_SIZE: usize = 0x20_0000; // 2 MiB

/// Map a PSX virtual address to an offset within the RETRO_MEMORY_SYSTEM_RAM buffer.
#[inline]
fn off(addr: u32) -> usize {
    (addr & 0x1F_FFFF) as usize
}

#[inline]
pub fn rd8(ram: &[u8], a: u32) -> u8 {
    ram.get(off(a)).copied().unwrap_or(0)
}

#[inline]
pub fn rd16(ram: &[u8], a: u32) -> u16 {
    let o = off(a);
    if o + 1 < ram.len() {
        u16::from_le_bytes([ram[o], ram[o + 1]])
    } else {
        0
    }
}

#[inline]
pub fn rd32(ram: &[u8], a: u32) -> u32 {
    let o = off(a);
    if o + 3 < ram.len() {
        u32::from_le_bytes([ram[o], ram[o + 1], ram[o + 2], ram[o + 3]])
    } else {
        0
    }
}

// ---- Bloody Roar 2 (NTSC-U) addresses --------------------------------------------------------
// EMPIRICAL: no fixed public HP/round address exists for BR2 — these are recovered with the
// `/fight/ram` debug dump + a RAM diff (snapshot, land a hit, diff for the byte that dropped).
// 0 = not yet pinned; `MatchState::read` returns None until P1_HP/P2_HP are set, so the fight room
// falls back to its disconnect-based end.

pub const P1_HP: u32 = 0;
pub const P2_HP: u32 = 0;
/// Best-of-3 rounds-won counters (0 = use the single-round-decides fallback).
pub const P1_ROUNDS: u32 = 0;
pub const P2_ROUNDS: u32 = 0;

/// Live match state read from RAM. HP is the current health of each fighter (left = P1/pad 0,
/// right = P2/pad 1); `rounds_*` is the best-of-3 tally when those addresses are known.
#[derive(Clone, Copy, Debug)]
pub struct MatchState {
    pub p1_hp: u16,
    pub p2_hp: u16,
    pub p1_rounds: u8,
    pub p2_rounds: u8,
}

impl MatchState {
    /// Read the match state from a RAM slice, or None if the HP addresses aren't pinned yet.
    pub fn read(ram: &[u8]) -> Option<Self> {
        if P1_HP == 0 || P2_HP == 0 {
            return None;
        }
        Some(MatchState {
            p1_hp: rd16(ram, P1_HP),
            p2_hp: rd16(ram, P2_HP),
            p1_rounds: if P1_ROUNDS != 0 { rd8(ram, P1_ROUNDS) } else { 0 },
            p2_rounds: if P2_ROUNDS != 0 { rd8(ram, P2_ROUNDS) } else { 0 },
        })
    }
}
