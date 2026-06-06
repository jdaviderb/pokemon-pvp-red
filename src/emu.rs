//! tetanes-core wrapper: load the ROM (working around the NES 2.0 header bug),
//! run the APU at 48 kHz, expose per-frame video/audio, and apply controller input.
//!
//! Every API/behaviour here was verified by compiling + running against MK1.nes
//! (see research/emulator.md): frame_buffer() = 245_760 B RGBA 256x240, alpha always
//! 255; audio_samples() = mono f32 @48k (~799/frame); mapper 4 / MMC3 (Txrom).

use tetanes_core::control_deck::ControlDeck;
use tetanes_core::input::{JoypadBtn, Player};

/// Zero the NES 2.0 PRG-RAM / CHR-RAM size header bytes (10 & 11) to dodge the
/// tetanes-core 0.14.1 `64 << n` overflow on ROMs that declare battery NVRAM.
///
/// MK1.nes has byte10 = 0x70 -> `64 << 112` overflows -> `load_rom` aborts with
/// `InvalidHeader`. Battery RAM is irrelevant to headless rendering, so zeroing these
/// is safe; the ROM still loads as mapper 4 / Txrom / NTSC / battery_backed. The check
/// makes it a no-op for clean headers, so keep it unconditional.
fn sanitize_nes2_ram_header(bytes: &mut [u8]) {
    if bytes.len() >= 16 && &bytes[0..4] == b"NES\x1a" && (bytes[7] & 0x0C) == 0x08 {
        bytes[10] = 0x00; // PRG-RAM / PRG-NVRAM size (the 0x70 that overflows)
        bytes[11] = 0x00; // CHR-RAM / CHR-NVRAM size (defensive)
    }
}

pub struct Emu {
    deck: ControlDeck,
}

impl Emu {
    pub fn new(rom_bytes: &[u8], name: &str) -> anyhow::Result<Self> {
        let mut deck = ControlDeck::new();
        deck.set_sample_rate(48_000.0); // match Opus; default is 44_100
        deck.set_concurrent_dpad(true); // allow opposite D-pad directions (fighting game)

        let mut bytes = rom_bytes.to_vec();
        sanitize_nes2_ram_header(&mut bytes);
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        // load_rom auto-resets (running = true). clock_frame errs if not running.
        let loaded = deck
            .load_rom(name, &mut cur)
            .map_err(|e| anyhow::anyhow!("load_rom failed: {e:?}"))?;
        tracing::info!(
            "ROM '{}' loaded: mapper={:?} region={:?} battery={}",
            loaded.name,
            format!("{:?}", deck.mapper()),
            loaded.region,
            loaded.battery_backed,
        );
        debug_assert!(deck.is_running());
        Ok(Self { deck })
    }

    /// Advance exactly one NTSC frame. Returns Err if the CPU corrupts / ROM unloaded.
    pub fn clock_frame(&mut self) -> anyhow::Result<()> {
        self.deck
            .clock_frame()
            .map_err(|e| anyhow::anyhow!("clock_frame: {e:?}"))
    }

    /// RGBA8888, 256x240, 245_760 bytes, alpha always 255.
    pub fn frame_buffer(&mut self) -> &[u8] {
        self.deck.frame_buffer()
    }

    /// Mono f32 @ 48 kHz accumulated since last clear (~799 samples/frame).
    pub fn audio_samples(&self) -> &[f32] {
        self.deck.audio_samples()
    }

    /// MUST be called every frame or samples accumulate across frames.
    pub fn clear_audio_samples(&mut self) {
        self.deck.clear_audio_samples();
    }

    /// Apply a button edge to player 1 or 2 (sticky until released). Any value other
    /// than 2 is treated as player 1.
    pub fn set_button(&mut self, player: u8, btn: JoypadBtn, pressed: bool) {
        let slot = if player == 2 { Player::Two } else { Player::One };
        self.deck.joypad_mut(slot).set_button(btn, pressed);
    }
}

/// Browser wire button name -> tetanes JoypadBtn. Unknown -> None (ignored).
pub fn map_button(b: &str) -> Option<JoypadBtn> {
    Some(match b {
        "A" => JoypadBtn::A,
        "B" => JoypadBtn::B,
        "Up" => JoypadBtn::Up,
        "Down" => JoypadBtn::Down,
        "Left" => JoypadBtn::Left,
        "Right" => JoypadBtn::Right,
        "Start" => JoypadBtn::Start,
        "Select" => JoypadBtn::Select,
        _ => return None,
    })
}
