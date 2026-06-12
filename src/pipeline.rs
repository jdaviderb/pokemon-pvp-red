//! One dedicated OS thread runs the Emu core at real-time core-fps, encodes each frame to
//! VP8 + stereo Opus, and fans the encoded media out over broadcast channels to every peer.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use crate::audio::OpusStreamer;
use crate::battle::{AgentAction, BattleState, MenuPhase, TapMachine};
use crate::libretro::{map_button, EmuAction, AXIS_MAX, Emu};
use crate::video::{frame_to_i420, i420_len, make_vp8_encoder};

/// Reply channels for emu-thread-only savestate ops (the thread owns `Emu`).
type SaveReq = tokio::sync::oneshot::Sender<Option<Vec<u8>>>;
type LoadReq = (Vec<u8>, tokio::sync::oneshot::Sender<bool>);
/// Read `len` bytes of SYSTEM_RAM from a (PSX) address on the emu thread. Used by the fighting
/// arena to poll HP/round state and by the `/fight/ram` debug dump.
type RamReadReq = (u32, u32, tokio::sync::oneshot::Sender<Vec<u8>>);

/// /battle/setup payload, resolved on the emu thread. Reply = Ok / why-not.
pub struct SetupReq {
    pub player: u8, // internal species index
    pub enemy: u8,  // internal species index
    pub level: u8,
    pub player_name: String, // custom nickname ("" = species name)
    pub enemy_name: String,
    pub reply: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct EncodedVideo {
    pub data: Bytes,
}

#[derive(Clone)]
pub struct EncodedAudio {
    pub data: Bytes,
    pub samples: u32, // per-channel (960)
}

/// Browser input event: {"type":"down"|"up","button":"A","player":1}.
#[derive(serde::Deserialize)]
pub struct InputEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub button: String,
    #[serde(default = "default_player")]
    pub player: u8,
}
fn default_player() -> u8 {
    1
}

pub struct AppInner {
    pub video_tx: broadcast::Sender<EncodedVideo>,
    pub audio_tx: broadcast::Sender<EncodedAudio>,
    pub input_tx: mpsc::UnboundedSender<InputEvent>,
    pub keyframe_req: Arc<AtomicBool>,
    // --- battle arena ---
    pub battle: Arc<Mutex<Option<BattleState>>>, // latest snapshot, refreshed every frame
    pub action_tx: mpsc::UnboundedSender<AgentAction>, // agent actions queued by HTTP
    pub save_tx: mpsc::UnboundedSender<SaveReq>,
    pub load_tx: mpsc::UnboundedSender<LoadReq>,
    pub setup_tx: mpsc::UnboundedSender<SetupReq>,
    /// Fighting arena: read SYSTEM_RAM (PSX) on the emu thread — HP/round polling + `/fight/ram`.
    pub ram_read_tx: mpsc::UnboundedSender<RamReadReq>,
    /// Manual enemy control: 0xFF = game AI decides; 0..3 = force that enemy move slot every turn.
    pub enemy_force: Arc<AtomicU8>,
    /// Force P1's chosen move into CCDC (wPlayerSelectedMove): 0xFF = off; 0..3 = that slot. Makes
    /// the executed player move reliable even if the menu-nav macro drops a Down (off-by-one).
    pub player_force: Arc<AtomicU8>,
    /// Fighting arena: total attack-button (A/B/X/Y) presses per pad, monotonically increasing.
    /// The fight room samples these to correlate HP drops with WHO was attacking (damage/winner
    /// attribution that doesn't depend on knowing which RAM struct belongs to which side).
    pub attack_press: Arc<[AtomicU32; 2]>,
}

pub fn start(core_path: String, rom_path: String) -> Arc<AppInner> {
    // Generous buffers so a slow spectator lags (drops + asks for a keyframe) instead of stalling
    // the whole fan-out; the emulator (single producer) never blocks on a lagging viewer.
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(240);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(256);
    let (input_tx, input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let keyframe_req = Arc::new(AtomicBool::new(false));

    let (action_tx, action_rx) = mpsc::unbounded_channel::<AgentAction>();
    let (save_tx, save_rx) = mpsc::unbounded_channel::<SaveReq>();
    let (load_tx, load_rx) = mpsc::unbounded_channel::<LoadReq>();
    let (setup_tx, setup_rx) = mpsc::unbounded_channel::<SetupReq>();
    let (ram_read_tx, ram_read_rx) = mpsc::unbounded_channel::<RamReadReq>();
    let enemy_force = Arc::new(AtomicU8::new(0xFF)); // default: game AI
    let player_force = Arc::new(AtomicU8::new(0xFF)); // default: use the menu pick
    let battle: Arc<Mutex<Option<BattleState>>> = Arc::new(Mutex::new(None));
    let attack_press: Arc<[AtomicU32; 2]> = Arc::new([AtomicU32::new(0), AtomicU32::new(0)]);

    let v = video_tx.clone();
    let a = audio_tx.clone();
    let kf = keyframe_req.clone();
    let battle_thread = battle.clone();
    let ef = enemy_force.clone();
    let pf = player_force.clone();
    let ap = attack_press.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_loop(
            core_path, rom_path, v, a, input_rx, kf, action_rx, save_rx, load_rx, setup_rx,
            ram_read_rx, battle_thread, ef, pf, ap,
        ) {
            tracing::error!("emulator loop ended: {e:?}");
        }
    });

    Arc::new(AppInner {
        video_tx,
        audio_tx,
        input_tx,
        keyframe_req,
        battle,
        action_tx,
        save_tx,
        load_tx,
        setup_tx,
        ram_read_tx,
        enemy_force,
        player_force,
        attack_press,
    })
}

/// An `AppInner` with NO emulator thread — for the coordinator process, which owns auth/lobby/
/// matchmaking but never runs a battle (those run on worker processes). The channels exist only so
/// `AppState`/`GameState` type-check; their receivers are dropped, so any send is a harmless no-op.
pub fn dummy() -> Arc<AppInner> {
    let (video_tx, _) = broadcast::channel::<EncodedVideo>(1);
    let (audio_tx, _) = broadcast::channel::<EncodedAudio>(1);
    let (input_tx, _input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let (action_tx, _action_rx) = mpsc::unbounded_channel::<AgentAction>();
    let (save_tx, _save_rx) = mpsc::unbounded_channel::<SaveReq>();
    let (load_tx, _load_rx) = mpsc::unbounded_channel::<LoadReq>();
    let (setup_tx, _setup_rx) = mpsc::unbounded_channel::<SetupReq>();
    let (ram_read_tx, _ram_read_rx) = mpsc::unbounded_channel::<RamReadReq>();
    Arc::new(AppInner {
        video_tx,
        audio_tx,
        input_tx,
        keyframe_req: Arc::new(AtomicBool::new(false)),
        battle: Arc::new(Mutex::new(None)),
        action_tx,
        save_tx,
        load_tx,
        setup_tx,
        ram_read_tx,
        enemy_force: Arc::new(AtomicU8::new(0xFF)),
        player_force: Arc::new(AtomicU8::new(0xFF)),
        attack_press: Arc::new([AtomicU32::new(0), AtomicU32::new(0)]),
    })
}

/// Recompute an analog vector from currently-held direction keys (8-way).
fn stick_vec(held: &HashSet<String>, up: &str, down: &str, left: &str, right: &str) -> (i16, i16) {
    let mut x = 0i32;
    let mut y = 0i32;
    if held.contains(left) {
        x -= AXIS_MAX as i32;
    }
    if held.contains(right) {
        x += AXIS_MAX as i32;
    }
    if held.contains(up) {
        y -= AXIS_MAX as i32;
    }
    if held.contains(down) {
        y += AXIS_MAX as i32;
    }
    (x.clamp(-32768, 32767) as i16, y.clamp(-32768, 32767) as i16)
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    core_path: String,
    rom_path: String,
    video_tx: broadcast::Sender<EncodedVideo>,
    audio_tx: broadcast::Sender<EncodedAudio>,
    mut input_rx: mpsc::UnboundedReceiver<InputEvent>,
    keyframe_req: Arc<AtomicBool>,
    mut action_rx: mpsc::UnboundedReceiver<AgentAction>,
    mut save_rx: mpsc::UnboundedReceiver<SaveReq>,
    mut load_rx: mpsc::UnboundedReceiver<LoadReq>,
    mut setup_rx: mpsc::UnboundedReceiver<SetupReq>,
    mut ram_read_rx: mpsc::UnboundedReceiver<RamReadReq>,
    battle: Arc<Mutex<Option<BattleState>>>,
    enemy_force: Arc<AtomicU8>,
    player_force: Arc<AtomicU8>,
    attack_press: Arc<[AtomicU32; 2]>,
) -> anyhow::Result<()> {
    let mut emu = Emu::new(&core_path, &rom_path)?;
    let mut opus =
        OpusStreamer::new(emu.sample_rate).map_err(|e| anyhow::anyhow!("opus init: {e:?}"))?;

    // Warm up until the core delivers a real framebuffer, then size the VP8 canvas to it
    // (VP8 can't resize mid-stream; later size changes are letterboxed onto this canvas).
    let frame_period = Duration::from_nanos((1_000_000_000.0 / emu.fps).max(1.0) as u64);
    let (mut cw, mut ch) = {
        let mut dims = (0u32, 0u32);
        for _ in 0..240 {
            emu.clock_frame();
            dims = emu.with_frame(|f| (f.w, f.h));
            if dims.0 >= 2 && dims.1 >= 2 {
                break;
            }
        }
        ((dims.0 & !1).max(2), (dims.1 & !1).max(2)) // even dims for the encoder
    };
    tracing::info!("VP8 canvas {cw}x{ch} (initial; re-inits on resolution change)");

    let mut vpx = make_vp8_encoder(cw, ch).map_err(|e| anyhow::anyhow!("vpx init: {e:?}"))?;
    let mut i420 = vec![0u8; i420_len(cw as usize, ch as usize)];

    emu.audio_drain(); // discard audio accumulated during warmup (avoid a startup burst)

    let mut next = Instant::now();
    let mut frame_idx: u64 = 0;
    let mut held: HashSet<String> = HashSet::new();
    let mut taps = TapMachine::default(); // agent action -> button taps
    let mut menu = MenuPhase::Overworld; // software battle-menu state machine
    // Hide the FIGHT/ITEM/PKMN/RUN menu in battle (PvP injects both moves, so it's noise). ON by
    // default; set env HIDE_BATTLE_MENU=0 to show the real menu again. See src/hud.rs.
    let hide_menu = std::env::var("HIDE_BATTLE_MENU").map(|v| v != "0" && !v.is_empty()).unwrap_or(true);

    let mut stat_t = Instant::now();
    let mut stat_frames: u64 = 0;
    let mut stat_vpkts: u64 = 0;
    let mut stat_apkts: u64 = 0;

    loop {
        // 0a. Savestate commands run on THIS thread (the only owner of `Emu`).
        while let Ok(reply) = save_rx.try_recv() {
            let _ = reply.send(emu.save_state());
        }
        while let Ok((data, reply)) = load_rx.try_recv() {
            let ok = emu.load_state(&data);
            if ok {
                taps.clear(&emu); // abandon any in-flight macro; we just teleported state
            }
            let _ = reply.send(ok);
        }
        // 0a''. Fighting arena / debug: copy a window of SYSTEM_RAM (PSX) back to the caller.
        while let Ok((addr, len, reply)) = ram_read_rx.try_recv() {
            let data = emu
                .with_system_ram(|ram| {
                    let o = (addr & 0x1F_FFFF) as usize;
                    if o < ram.len() {
                        ram[o..(o + len as usize).min(ram.len())].to_vec()
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_default();
            let _ = reply.send(data);
        }

        // 0a'. Custom matchup: load the intro savestate, inject both party slots, then queue
        //      A-taps to drive the send-out (engine draws the injected sprites/names/cries).
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
                // Intro must be pre-send-out (D057!=0, D014==0, CFE5==0) or the ROM/state mismatch.
                let ok_intro = emu
                    .with_system_ram(|ram| {
                        ram[(0xD057 - 0xC000) as usize] != 0
                            && ram[(0xD014 - 0xC000) as usize] == 0
                            && ram[(0xCFE5 - 0xC000) as usize] == 0
                    })
                    .unwrap_or(false);
                if !ok_intro {
                    return Err("intro markers wrong — savestate/ROM mismatch (run Pokemon Red.gb)".into());
                }
                emu.with_system_ram_mut(|ram| {
                    crate::battle::setup_matchup(
                        ram, player, enemy, level, &req.player_name, &req.enemy_name,
                    );
                });
                Ok(())
            })();
            if res.is_ok() {
                taps.clear(&emu);
                menu = MenuPhase::BattleIntro;
                taps.enqueue(&AgentAction::Buttons {
                    presses: vec!["A".into(), "A".into(), "A".into(), "A".into(), "A".into(), "A".into()],
                });
                tracing::info!("matchup set up: player={} enemy={}", req.player, req.enemy);
            }
            let _ = req.reply.send(res);
        }

        // 0b. Queue any new agent actions, then advance the tap macro one frame (sets PAD bits).
        while let Ok(action) = action_rx.try_recv() {
            taps.enqueue(&action);
        }
        taps.tick(&emu);

        // 1. Apply pending input (P1). Direction keys feed analog vectors; rest are digital.
        let mut stick_dirty = false;
        while let Ok(ev) = input_rx.try_recv() {
            let pressed = ev.kind == "down";
            match map_button(&ev.button) {
                Some(EmuAction::Btn(id)) => {
                    let pad = (ev.player.max(1) - 1).min(1) as usize; // P1 -> port 0, P2 -> port 1
                    // Fighting arena: count attack presses per pad; the fight room correlates HP
                    // drops with who was attacking to attribute damage (and the winner).
                    if pressed && matches!(ev.button.as_str(), "A" | "B" | "X" | "Y") {
                        attack_press[pad].fetch_add(1, Ordering::Relaxed);
                    }
                    emu.set_button_p(pad, id, pressed);
                }
                Some(EmuAction::Stick) | Some(EmuAction::CStick) => {
                    tracing::info!("input P{}: {} {} (analog)", ev.player, ev.kind, ev.button);
                    if pressed {
                        held.insert(ev.button.clone());
                    } else {
                        held.remove(&ev.button);
                    }
                    stick_dirty = true;
                }
                None => {}
            }
        }
        if stick_dirty {
            let (sx, sy) = stick_vec(&held, "StickUp", "StickDown", "StickLeft", "StickRight");
            emu.set_stick(sx, sy);
            let (cx, cy) = stick_vec(&held, "CUp", "CDown", "CLeft", "CRight");
            emu.set_cstick(cx, cy);
        }

        // 2. New viewer -> reset encoder for a fresh keyframe.
        if keyframe_req.swap(false, Ordering::Relaxed) {
            if let Ok(e) = make_vp8_encoder(cw, ch) {
                vpx = e;
                tracing::info!("vp8 encoder reset -> keyframe for new viewer");
            }
        }

        // 3. Advance one frame.
        emu.clock_frame();

        // 3a. Refresh the battle snapshot from WRAM (negligible vs VP8 encode) + menu phase.
        let busy = taps.busy();
        let snap = emu.with_system_ram(|ram| {
            menu = crate::battle::next_menu_phase(menu, ram, busy);
            crate::battle::read_battle_state(ram, menu)
        });
        if let Some(s) = snap {
            *battle.lock().unwrap() = Some(s);
        }

        // 3a'. Manual enemy control: force wEnemySelectedMove (CCDD) to the chosen enemy move so
        //      YOU drive the opponent too. Written AFTER clock_frame each frame, so it's the value
        //      the engine reads when it executes the enemy's move. 0xFF = let the game AI decide.
        let ef = enemy_force.load(Ordering::Relaxed);
        if ef < 4 {
            emu.with_system_ram_mut(|ram| {
                let in_battle = ram[(0xD057 - 0xC000) as usize];
                let ccdd = (0xCCDD - 0xC000) as usize;
                if in_battle != 0 && ram[ccdd] != 0 {
                    let mv = ram[(0xCFED - 0xC000) as usize + ef as usize]; // enemy move[slot] id
                    if mv != 0 {
                        ram[ccdd] = mv;
                    }
                }
            });
        }

        // 3a''. Symmetric for P1: force the chosen move into wPlayerSelectedMove (CCDC). P1 picks via
        //       the menu-nav macro which can drop a Down (off-by-one); this guarantees the move that
        //       actually executes is the one the player chose. 0xFF = trust the menu pick.
        let pf = player_force.load(Ordering::Relaxed);
        if pf < 4 {
            emu.with_system_ram_mut(|ram| {
                let in_battle = ram[(0xD057 - 0xC000) as usize];
                let ccdc = (0xCCDC - 0xC000) as usize;
                if in_battle != 0 && ram[ccdc] != 0 {
                    let mv = ram[(0xD01C - 0xC000) as usize + pf as usize]; // player move[slot] id
                    if mv != 0 {
                        ram[ccdc] = mv;
                    }
                }
            });
        }

        // 3b. Resolution change? (Emu games switch lo-res 640x240 <-> hi-res 640x480, e.g. Pokémon
        //     Stadium menus). VP8 needs fixed dims per encoder, so re-init to match — a fresh
        //     encoder starts with a keyframe and the browser adapts (CSS stretches to the 4:3 box).
        let (fw, fh) = emu.with_frame(|f| ((f.w & !1).max(2), (f.h & !1).max(2)));
        if (fw, fh) != (cw, ch) {
            if let Ok(e) = make_vp8_encoder(fw, fh) {
                vpx = e;
                i420 = vec![0u8; i420_len(fw as usize, fh as usize)];
                cw = fw;
                ch = fh;
                tracing::info!("resolution change -> VP8 canvas now {cw}x{ch}");
            }
        }

        // 3c. Hide the FIGHT/ITEM menu: in PvP we inject both sides' moves, so the menu is noise.
        //     When it's up, paint over it with the game's bg on the RGB565 frame. The game's own
        //     battle text at other phases is untouched (we only blank during the menu). Toggle:
        //     HIDE_BATTLE_MENU=0 disables this (read once into `hide_menu` above).
        // Only people WATCHING a battle need its video, so skip the whole RGB565->I420->VP8 pipeline
        // (the dominant per-battle CPU cost) when no one is subscribed. An unwatched battle runs
        // essentially for free; encoding resumes (with a keyframe, via keyframe_req) the instant a
        // viewer/TV connects. This is what lets MANY concurrent battles run on one node.
        let watching = video_tx.receiver_count() > 0;
        let menu_up = hide_menu
            && watching
            && battle
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| matches!(s.menu, crate::battle::MenuPhase::MainMenu))
                .unwrap_or(false);
        if menu_up {
            emu.with_frame_mut(|f| {
                if !f.bytes.is_empty() && f.fmt == crate::video::PIXFMT_RGB565 {
                    crate::hud::hide_battle_menu(&mut f.bytes, f.pitch, f.w as usize, f.h as usize);
                }
            });
        }

        // 4. VIDEO (only when watched): framebuffer (XRGB8888 for Emu/SameBoy, RGB565 for gambatte)
        //    -> I420 -> VP8 -> broadcast. Skipped entirely for unwatched battles (see `watching`).
        if watching {
            emu.with_frame(|f| {
                if !f.bytes.is_empty() {
                    frame_to_i420(
                        &f.bytes,
                        f.w as usize,
                        f.h as usize,
                        f.pitch,
                        f.fmt,
                        &mut i420,
                        cw as usize,
                        ch as usize,
                    );
                }
            });
            let pts_ms = (frame_idx as f64 * 1000.0 / emu.fps) as i64;
            match vpx.encode(pts_ms, &i420) {
                Ok(packets) => {
                    for frame in packets {
                        stat_vpkts += 1;
                        let _ = video_tx.send(EncodedVideo {
                            data: Bytes::copy_from_slice(frame.data),
                        });
                    }
                }
                Err(e) => tracing::warn!("vpx encode: {e:?}"),
            }
        }

        // 5. AUDIO: always drain (so the core's audio buffer can't back up), but only resample +
        //    Opus-encode + broadcast when someone is actually listening.
        let pcm = emu.audio_drain();
        if audio_tx.receiver_count() > 0 {
            opus.push_i16_stereo(&pcm);
            match opus.take_packets() {
                Ok(pkts) => {
                    for p in pkts {
                        stat_apkts += 1;
                        let _ = audio_tx.send(EncodedAudio {
                            data: Bytes::from(p.data),
                            samples: p.samples,
                        });
                    }
                }
                Err(e) => tracing::warn!("opus encode: {e:?}"),
            }
        }

        // 6. Stats.
        stat_frames += 1;
        if stat_t.elapsed() >= Duration::from_secs(5) {
            let secs = stat_t.elapsed().as_secs_f64();
            tracing::info!(
                "fps: {:.1} | {} video pkts | {} audio pkts (last {:.0}s) | viewers v={} a={}",
                stat_frames as f64 / secs,
                stat_vpkts,
                stat_apkts,
                secs,
                video_tx.receiver_count(),
                audio_tx.receiver_count(),
            );
            stat_t = Instant::now();
            stat_frames = 0;
            stat_vpkts = 0;
            stat_apkts = 0;
        }

        // 7. Drift-compensated pacing to the next core-fps deadline.
        frame_idx += 1;
        next += frame_period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // fell behind; resync to avoid a burst
        }
    }
}
