//! Room finite-state-machine, matchmaking, and the turn-based 2-player battle engine.
//!
//! Reuses the single-emulator engine verbatim: player 1 drives the YOU side (`action_tx`),
//! player 2 drives the enemy (`enemy_force`/CCDD), the slot machine result is injected via
//! `setup_tx`. v1 runs ONE concurrent battle (the one emulator); extra matched rooms wait in a
//! FIFO. See DESIGN-MULTIPLAYER.md.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use rand::seq::SliceRandom;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::auth::MeRoom;
use crate::battle::{AgentAction, BattlePokemon, BattleState, MenuPhase, SPECIES};
use crate::entities::{matches as e_matches, rooms as e_rooms, user_room as e_ur, users as e_users};
use crate::pipeline::{AppInner, SetupReq};
use crate::signaling::AppState;
use crate::ws::WsHub;

pub type RoomId = i32;
pub type UserId = i32;

const TURN_SECS: u64 = 15;
const LEVEL: u8 = 50;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoomPhase {
    Matched,
    SlotMachine,
    Setup,
    Battle,
    Result,
    Done,
}
impl RoomPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomPhase::Matched => "matched",
            RoomPhase::SlotMachine => "slot_machine",
            RoomPhase::Setup => "setup",
            RoomPhase::Battle => "battle",
            RoomPhase::Result => "result",
            RoomPhase::Done => "done",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    P1,
    P2,
}
fn seat_num(s: Seat) -> u8 {
    match s {
        Seat::P1 => 1,
        Seat::P2 => 2,
    }
}

pub struct PlayerSeat {
    pub user_id: UserId,
    pub username: String,
    pub species: u8, // internal index from the slot machine
    pub move_tx: Option<mpsc::UnboundedSender<u8>>, // armed during a turn
}

pub struct Room {
    pub id: RoomId,
    /// Public, unguessable room id (UUID) used in URLs / share links / the spectator API. The
    /// internal `id` stays an integer; this is the only id ever shown to clients.
    pub public_id: String,
    pub phase: RoomPhase,
    pub p1: PlayerSeat,
    pub p2: PlayerSeat,
    pub level: u8,
    pub last_alive: Seat,
    pub turn_deadline: Option<Instant>,
}

/// Random UUID v4 string (no extra crate; OsRng + the version/variant bits).
pub fn gen_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}
impl Room {
    fn seat_mut(&mut self, s: Seat) -> &mut PlayerSeat {
        match s {
            Seat::P1 => &mut self.p1,
            Seat::P2 => &mut self.p2,
        }
    }
    fn seat_of(&self, uid: UserId) -> Option<Seat> {
        if self.p1.user_id == uid {
            Some(Seat::P1)
        } else if self.p2.user_id == uid {
            Some(Seat::P2)
        } else {
            None
        }
    }
}

pub struct GameState {
    pub inner: Arc<AppInner>,
    pub db: DatabaseConnection,
    pub queue: Mutex<VecDeque<UserId>>,
    pub rooms: Mutex<HashMap<RoomId, Room>>,
    pub user_room: Mutex<HashMap<UserId, RoomId>>,
    pub pending: Mutex<VecDeque<RoomId>>,
    pub active_room: Mutex<Option<RoomId>>,
    pub emu_busy: AtomicBool,
    pub ws: WsHub,
    /// Presence heartbeats (client id -> last seen) for the "PLAYERS ONLINE" counter. std Mutex so
    /// the HTTP handler can touch it without awaiting.
    pub online: std::sync::Mutex<HashMap<String, Instant>>,
    /// Cached feature flags (key -> enabled), refreshed off the request path so /api/config (hit on
    /// every page load) does ZERO DB reads. Seeded at boot, refreshed every ~30s.
    pub flags_cache: std::sync::Mutex<HashMap<String, bool>>,
}

impl GameState {
    pub fn new(inner: Arc<AppInner>, db: DatabaseConnection) -> Self {
        Self {
            inner,
            db,
            queue: Mutex::new(VecDeque::new()),
            rooms: Mutex::new(HashMap::new()),
            user_room: Mutex::new(HashMap::new()),
            pending: Mutex::new(VecDeque::new()),
            active_room: Mutex::new(None),
            emu_busy: AtomicBool::new(false),
            ws: WsHub::new(),
            online: std::sync::Mutex::new(HashMap::new()),
            flags_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// WS entry points (called from ws.rs)
// ---------------------------------------------------------------------------

pub async fn on_connect(game: &Arc<GameState>, uid: UserId, uname: &str) {
    let (wins, losses) = user_stats(&game.db, uid).await;
    game.ws
        .send_to(
            uid,
            json!({"type":"hello","user":{"id":uid,"username":uname,"wins":wins,"losses":losses}}),
        )
        .await;
    send_current_state(game, uid).await;
}

pub async fn handle_client_msg(game: &Arc<GameState>, uid: UserId, uname: &str, v: serde_json::Value) {
    match v.get("type").and_then(|t| t.as_str()) {
        Some("find_match") => find_match(game, uid, uname).await,
        Some("cancel_queue") => cancel_queue(game, uid).await,
        Some("commit_move") => {
            if let Some(slot) = v.get("slot").and_then(|s| s.as_u64()) {
                commit_move(game, uid, slot as u8).await;
            }
        }
        Some("resume") => send_current_state(game, uid).await,
        _ => {}
    }
}

async fn send_current_state(game: &Arc<GameState>, uid: UserId) {
    if let Some(rid) = game.user_room.lock().await.get(&uid).copied() {
        send_room_state_to(game, rid, uid).await;
        push_live_phase(game, rid, uid).await;
    } else {
        let pos = queue_position(game, uid).await;
        match pos {
            Some(p) => game.ws.send_to(uid, json!({"type":"queued","position":p})).await,
            None => {
                let qs = game.queue.lock().await.len();
                game.ws
                    .send_to(uid, json!({"type":"lobby","queued":false,"queue_size":qs}))
                    .await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Matchmaking
// ---------------------------------------------------------------------------

async fn find_match(game: &Arc<GameState>, uid: UserId, _uname: &str) {
    if game.user_room.lock().await.contains_key(&uid) {
        game.ws
            .send_to(uid, json!({"type":"error","code":"in_room","message":"already in a room"}))
            .await;
        return;
    }
    {
        let mut q = game.queue.lock().await;
        if !q.contains(&uid) {
            q.push_back(uid);
        }
    }
    let pos = queue_position(game, uid).await.unwrap_or(1);
    game.ws.send_to(uid, json!({"type":"queued","position":pos})).await;
}

pub(crate) async fn cancel_queue(game: &Arc<GameState>, uid: UserId) {
    game.queue.lock().await.retain(|&u| u != uid);
    game.ws.send_to(uid, json!({"type":"lobby","queued":false,"queue_size":0})).await;
}

async fn queue_position(game: &Arc<GameState>, uid: UserId) -> Option<usize> {
    game.queue.lock().await.iter().position(|&u| u == uid).map(|i| i + 1)
}

/// Background task: pair queued users into rooms, then feed the single emulator one at a time.
/// Evict stale presence entries off the request path, so `/api/online` stays O(1) under load
/// (the hot path only inserts + reads len; this background sweep does the O(N) retain). Runs on the
/// coordinator/solo process.
pub fn spawn_presence_sweeper(game: Arc<GameState>) {
    tokio::spawn(async move {
        let window = Duration::from_secs(crate::signaling::ONLINE_WINDOW_SECS);
        loop {
            tokio::time::sleep(Duration::from_secs(4)).await;
            let now = std::time::Instant::now();
            game.online.lock().unwrap().retain(|_, t| now.duration_since(*t) < window);
        }
    });
}

pub fn spawn_matchmaker(game: Arc<GameState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            loop {
                let pair = {
                    let mut q = game.queue.lock().await;
                    if q.len() >= 2 {
                        Some((q.pop_front().unwrap(), q.pop_front().unwrap()))
                    } else {
                        None
                    }
                };
                match pair {
                    Some((a, b)) if a != b => {
                        create_room(&game, a, b).await;
                    }
                    Some(_) => {} // same user twice somehow; skip
                    None => break,
                }
            }
            maybe_start_next(&game).await;
        }
    });
}

async fn create_room(game: &Arc<GameState>, a: UserId, b: UserId) -> Option<String> {
    let ua = username_of(&game.db, a).await;
    let ub = username_of(&game.db, b).await;
    let inserted = e_rooms::ActiveModel {
        phase: Set(RoomPhase::Matched.as_str().to_string()),
        p1_user: Set(a),
        p2_user: Set(b),
        level: Set(LEVEL as i32),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&game.db)
    .await;
    let rid = match inserted {
        Ok(m) => m.id,
        Err(e) => {
            tracing::error!("room insert failed: {e}");
            return None;
        }
    };
    for u in [a, b] {
        let _ = e_ur::Entity::delete_by_id(u).exec(&game.db).await;
        let _ = e_ur::ActiveModel { user_id: Set(u), room_id: Set(rid) }.insert(&game.db).await;
    }
    {
        let mut ur = game.user_room.lock().await;
        ur.insert(a, rid);
        ur.insert(b, rid);
    }
    let public_id = gen_uuid();
    let room = Room {
        id: rid,
        public_id: public_id.clone(),
        phase: RoomPhase::Matched,
        p1: PlayerSeat { user_id: a, username: ua.clone(), species: 0, move_tx: None },
        p2: PlayerSeat { user_id: b, username: ub.clone(), species: 0, move_tx: None },
        level: LEVEL,
        last_alive: Seat::P1,
        turn_deadline: None,
    };
    game.rooms.lock().await.insert(rid, room);
    game.pending.lock().await.push_back(rid);
    game.ws.send_to(a, json!({"type":"matched","room_id":public_id,"seat":1,"opponent":ub})).await;
    game.ws.send_to(b, json!({"type":"matched","room_id":public_id,"seat":2,"opponent":ua})).await;
    tracing::info!("room {rid}: matched {a} vs {b}");
    Some(public_id)
}

/// Coordinator path: create a room for (a,b) on THIS worker and start it; returns the public UUID.
pub async fn assign_battle(game: &Arc<GameState>, a: UserId, b: UserId) -> Option<String> {
    let pid = create_room(game, a, b).await?;
    maybe_start_next(game).await;
    Some(pid)
}

async fn maybe_start_next(game: &Arc<GameState>) {
    let mut active = game.active_room.lock().await;
    if active.is_some() {
        return;
    }
    let next = game.pending.lock().await.pop_front();
    if let Some(rid) = next {
        *active = Some(rid);
        drop(active);
        let g = game.clone();
        tokio::spawn(async move { run_room(g, rid).await });
    }
}

// ---------------------------------------------------------------------------
// The room engine: slot machine -> setup -> turn-based battle -> result
// ---------------------------------------------------------------------------

async fn run_room(game: Arc<GameState>, rid: RoomId) {
    game.emu_busy.store(true, Ordering::Relaxed);
    let inner = game.inner.clone();

    // Fighting-game mode (e.g. Bloody Roar on PSX): no slot machine, no turn engine — load a versus
    // savestate and let both players drive their OWN controller (P1 = pad 0, P2 = pad 1). The match
    // runs live until a player leaves. Enabled with env FIGHT_MODE=1.
    if fight_mode() {
        run_fight_room(&game, rid).await;
        return;
    }

    // 1. Slot machine: roll a random species per seat.
    let (p1sp, p2sp) = {
        let mut rng = rand::thread_rng();
        (
            SPECIES.choose(&mut rng).map(|s| s.species).unwrap_or(0x99),
            SPECIES.choose(&mut rng).map(|s| s.species).unwrap_or(0x99),
        )
    };
    {
        let mut rooms = game.rooms.lock().await;
        if let Some(r) = rooms.get_mut(&rid) {
            r.p1.species = p1sp;
            r.p2.species = p2sp;
            r.phase = RoomPhase::SlotMachine;
        } else {
            game.emu_busy.store(false, Ordering::Relaxed);
            *game.active_room.lock().await = None;
            return;
        }
    }
    let _ = e_rooms::ActiveModel {
        id: Set(rid),
        p1_species: Set(Some(p1sp as i32)),
        p2_species: Set(Some(p2sp as i32)),
        phase: Set(RoomPhase::SlotMachine.as_str().to_string()),
        ..Default::default()
    }
    .update(&game.db)
    .await;
    broadcast_room_state(&game, rid).await;
    broadcast_slot_result(&game, rid).await;
    tokio::time::sleep(Duration::from_millis(2800)).await;

    // 2. Setup: inject the matchup and send out both mons (retry — a load_state can occasionally
    //    not take on a freshly-booted core, leaving the title screen).
    set_phase(&game, rid, RoomPhase::Setup).await;
    let (p, e, lvl, pn, en) = {
        let rooms = game.rooms.lock().await;
        let r = rooms.get(&rid).unwrap();
        // Show each mon's SPECIES name in-game ("PIKACHU"), not the player's username.
        let name_of = |idx: u8| {
            crate::battle::species_by_index(idx)
                .map(|s| s.name.to_string())
                .unwrap_or_default()
        };
        (r.p1.species, r.p2.species, r.level, name_of(r.p1.species), name_of(r.p2.species))
    };
    let mut setup_ok = false;
    for attempt in 0..3u32 {
        let (tx, rx) = oneshot::channel();
        let _ = inner.setup_tx.send(SetupReq {
            player: p,
            enemy: e,
            level: lvl,
            player_name: pn.clone(),
            enemy_name: en.clone(),
            reply: tx,
        });
        match rx.await {
            Ok(Ok(())) => {
                setup_ok = true;
                break;
            }
            other => {
                tracing::warn!("room {rid}: setup attempt {attempt} failed: {other:?}");
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        }
    }
    if !setup_ok {
        abort_room(&game, rid, "setup_failed").await;
        return;
    }
    if !advance_to_menu(&inner).await {
        tracing::warn!("room {rid}: battle never reached the FIGHT menu");
        abort_room(&game, rid, "battle_did_not_start").await;
        return;
    }
    tokio::time::sleep(Duration::from_millis(500)).await; // let the menu settle / become input-ready
    tracing::info!("room {rid}: at FIGHT menu, battle begins");

    // 3. Battle: round loop, 15s/seat with CPU-random fallback.
    set_phase(&game, rid, RoomPhase::Battle).await;
    broadcast_battle_state(&game, rid).await;
    let mut last_good = snap(&inner);
    let mut guard = 0u32;
    loop {
        guard += 1;
        if guard > 4000 {
            break; // safety net (~ longest plausible battle)
        }
        let s = match snap(&inner) {
            Some(s) => s,
            None => {
                tokio::time::sleep(Duration::from_millis(60)).await;
                continue;
            }
        };
        if s.in_battle == 0 {
            break;
        }
        if s.menu != MenuPhase::MainMenu {
            let _ = inner.action_tx.send(AgentAction::Buttons { presses: vec!["A".into()] });
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        last_good = Some(s.clone());
        update_last_alive(&game, rid, &s).await;

        // Both players choose within 15s (concurrently); CPU picks random on timeout.
        let mut rx1 = arm_turn(&game, rid, Seat::P1, &s.player).await;
        let mut rx2 = arm_turn(&game, rid, Seat::P2, &s.enemy).await;
        let (p1slot, p2slot) = tokio::join!(
            recv_or_cpu(&mut rx1, &s.player, &game, rid, Seat::P1),
            recv_or_cpu(&mut rx2, &s.enemy, &game, rid, Seat::P2),
        );
        disarm(&game, rid).await;

        let last_turn = s.turns_in_battle;
        tracing::info!("room {rid}: turn {last_turn} -> P1 move slot {p1slot}, P2 move slot {p2slot}");
        inner.enemy_force.store(p2slot, Ordering::Relaxed); // arm P2's move (CCDD) before the round
        inner.player_force.store(p1slot, Ordering::Relaxed); // force P1's move (CCDC) too -> no off-by-one
        let resolved = run_round(&inner, last_turn, p1slot).await;
        inner.enemy_force.store(0xFF, Ordering::Relaxed); // DISARM or P2 repeats forever
        inner.player_force.store(0xFF, Ordering::Relaxed);
        if let Some(r) = resolved {
            if r.in_battle != 0 {
                last_good = Some(r);
            }
        }
        broadcast_battle_state(&game, rid).await;
    }

    // 4. Result.
    let winner = decide_winner(&game, rid, last_good.as_ref()).await;
    record_result(&game, rid, winner).await;
    set_phase(&game, rid, RoomPhase::Result).await;
    broadcast_winner(&game, rid, winner).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    set_phase(&game, rid, RoomPhase::Done).await;
    broadcast_to_room(&game, rid, json!({"type":"room_closed","reason":"battle_ended"})).await;
    clear_user_room(&game, rid).await;
    game.rooms.lock().await.remove(&rid);
    game.emu_busy.store(false, Ordering::Relaxed);
    *game.active_room.lock().await = None;
    // The matchmaker's next tick (~250ms) starts the next pending room — no recursion here.
}

/// True when the arena runs a fighting game (raw 2-player pad input) instead of the Pokémon engine.
pub fn fight_mode() -> bool {
    std::env::var("FIGHT_MODE").map(|v| v != "0" && !v.is_empty()).unwrap_or(false)
}

/// A fighting-game match: load the versus savestate, go straight to Battle, and stream while both
/// players drive their own controller port. Ends when a player leaves (or a long safety cap).
async fn run_fight_room(game: &Arc<GameState>, rid: RoomId) {
    let inner = game.inner.clone();
    // Load the "versus" savestate (both fighters in the arena). FIGHT_STATE overrides the path.
    let path = std::env::var("FIGHT_STATE").unwrap_or_else(|_| "states/bloodyroar_vs.state".into());
    match std::fs::read(&path) {
        Ok(bytes) => {
            let (tx, rx) = oneshot::channel();
            let _ = inner.load_tx.send((bytes, tx));
            if !matches!(rx.await, Ok(true)) {
                tracing::warn!("fight room {rid}: load_state({path}) did not take");
            }
        }
        Err(e) => tracing::warn!("fight room {rid}: no savestate at {path}: {e}"),
    }
    set_phase(game, rid, RoomPhase::Battle).await;
    broadcast_room_state(game, rid).await;
    tracing::info!("fight room {rid}: live — both players control their own pad");

    let (p1, p2) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (r.p1.user_id, r.p2.user_id),
        None => return,
    };
    // Live loop @ ~4 Hz. When the BR2 HP addresses are pinned (fight::P1_HP != 0), detect a round KO
    // (a fighter's HP hits 0), tally best-of-3, and end with the winning seat. Otherwise (addresses
    // unknown) just run until BOTH players leave. A ~16s grace lets both open the room page first.
    let hp_known = crate::fight::P1_HP != 0 && crate::fight::P2_HP != 0;
    let mut ticks = 0u32;
    let mut empty = 0u32;
    let mut p1_wins = 0u8;
    let mut p2_wins = 0u8;
    let mut round_locked = false; // debounce: credit one round per KO
    let mut fight_started = false; // only arm KO detection once a real round is live
    let mut winner: Option<Seat> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        ticks += 1;
        if ticks < 64 {
            continue;
        }

        // Both-left detection (~every 2s).
        if ticks % 8 == 0 {
            let live = game.ws.connected(p1).await || game.ws.connected(p2).await;
            empty = if live { 0 } else { empty + 1 };
            if empty >= 3 || ticks > 4800 {
                break; // both gone, or ~20min safety cap -> end with no winner
            }
        }

        if !hp_known {
            continue;
        }
        // Read both fighters' HP off the emu thread (P1 = left/pad0, P2 = right/pad1).
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        let _ = inner.ram_read_tx.send((crate::fight::P1_HP, 2, tx1));
        let _ = inner.ram_read_tx.send((crate::fight::P2_HP, 2, tx2));
        let rd = |b: Vec<u8>| (b.len() >= 2).then(|| u16::from_le_bytes([b[0], b[1]]));
        let (p1_hp, p2_hp) = match (rx1.await.ok().and_then(rd), rx2.await.ok().and_then(rd)) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };

        // Arm KO detection only once a real round is live (both fighters at plausible HP) — avoids
        // false "round over" during the character select / loading, where the addresses read junk.
        if (50..=2000).contains(&p1_hp) && (50..=2000).contains(&p2_hp) {
            fight_started = true;
        }
        if !fight_started {
            continue;
        }

        if !round_locked && (p1_hp == 0 || p2_hp == 0) {
            round_locked = true;
            if p1_hp == 0 {
                p2_wins += 1;
            } else {
                p1_wins += 1;
            }
            broadcast_to_room(
                game,
                rid,
                json!({"type":"fight_round","p1_wins":p1_wins,"p2_wins":p2_wins}),
            )
            .await;
            if p1_wins >= 2 || p2_wins >= 2 {
                winner = Some(if p1_wins >= 2 { Seat::P1 } else { Seat::P2 });
                break;
            }
        } else if round_locked && p1_hp > 0 && p2_hp > 0 {
            round_locked = false; // next round started — re-arm
        }
    }

    // A decided match runs the full result flow (DB + winner banner); a walk-away just closes.
    if winner.is_some() {
        record_result(game, rid, winner).await;
        set_phase(game, rid, RoomPhase::Result).await;
        broadcast_winner(game, rid, winner).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    set_phase(game, rid, RoomPhase::Done).await;
    let reason = if winner.is_some() { "match_won" } else { "match_ended" };
    broadcast_to_room(game, rid, json!({"type":"room_closed","reason":reason})).await;
    clear_user_room(game, rid).await;
    game.rooms.lock().await.remove(&rid);
    game.emu_busy.store(false, Ordering::Relaxed);
    *game.active_room.lock().await = None;
}

async fn abort_room(game: &Arc<GameState>, rid: RoomId, reason: &str) {
    tracing::warn!("room {rid} aborted: {reason}");
    broadcast_to_room(game, rid, json!({"type":"room_closed","reason":reason})).await;
    clear_user_room(game, rid).await;
    let _ = e_rooms::ActiveModel {
        id: Set(rid),
        phase: Set(RoomPhase::Done.as_str().to_string()),
        ended_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .update(&game.db)
    .await;
    game.rooms.lock().await.remove(&rid);
    game.emu_busy.store(false, Ordering::Relaxed);
    *game.active_room.lock().await = None;
    // The matchmaker tick restarts the next pending room.
}

// ---------------------------------------------------------------------------
// Turn machinery
// ---------------------------------------------------------------------------

async fn arm_turn(
    game: &Arc<GameState>,
    rid: RoomId,
    seat: Seat,
    mon: &BattlePokemon,
) -> mpsc::UnboundedReceiver<u8> {
    let (tx, rx) = mpsc::unbounded_channel();
    let uid = {
        let mut rooms = game.rooms.lock().await;
        let r = rooms.get_mut(&rid).unwrap();
        r.turn_deadline = Some(Instant::now() + Duration::from_secs(TURN_SECS));
        let s = r.seat_mut(seat);
        s.move_tx = Some(tx);
        s.user_id
    };
    game.ws
        .send_to(
            uid,
            json!({"type":"your_turn","seat":seat_num(seat),"deadline_ms":TURN_SECS*1000,"moves":move_infos(mon)}),
        )
        .await;
    rx
}

async fn recv_or_cpu(
    rx: &mut mpsc::UnboundedReceiver<u8>,
    mon: &BattlePokemon,
    game: &Arc<GameState>,
    rid: RoomId,
    seat: Seat,
) -> u8 {
    match tokio::time::timeout(Duration::from_secs(TURN_SECS), rx.recv()).await {
        Ok(Some(slot)) => slot.min(3),
        _ => {
            let slot = random_legal_move(mon);
            broadcast_to_room(game, rid, json!({"type":"move_auto","seat":seat_num(seat),"slot":slot})).await;
            slot
        }
    }
}

async fn disarm(game: &Arc<GameState>, rid: RoomId) {
    let mut rooms = game.rooms.lock().await;
    if let Some(r) = rooms.get_mut(&rid) {
        r.p1.move_tx = None;
        r.p2.move_tx = None;
        r.turn_deadline = None;
    }
}

pub async fn commit_move(game: &Arc<GameState>, uid: UserId, slot: u8) {
    let rid = match game.user_room.lock().await.get(&uid).copied() {
        Some(r) => r,
        None => return,
    };
    let mut rooms = game.rooms.lock().await;
    let r = match rooms.get_mut(&rid) {
        Some(r) => r,
        None => return,
    };
    if r.phase != RoomPhase::Battle {
        return;
    }
    let seat = match r.seat_of(uid) {
        Some(s) => s,
        None => return,
    };
    let armed = r.seat_mut(seat).move_tx.is_some();
    tracing::info!("commit_move uid={uid} slot={slot} room={rid} armed={armed}");
    if let Some(tx) = &r.seat_mut(seat).move_tx {
        let _ = tx.send(slot.min(3));
    }
}

fn random_legal_move(mon: &BattlePokemon) -> u8 {
    let mut rng = rand::thread_rng();
    let legal: Vec<u8> = (0..4u8)
        .filter(|&i| mon.moves[i as usize] != 0 && (mon.pp[i as usize] & 0x3f) != 0)
        .collect();
    legal.choose(&mut rng).copied().unwrap_or(0)
}

fn move_infos(mon: &BattlePokemon) -> serde_json::Value {
    let v: Vec<serde_json::Value> = (0..4usize)
        .filter(|&i| mon.moves[i] != 0)
        .map(|i| json!({"slot": i, "id": mon.moves[i], "pp": mon.pp[i] & 0x3f}))
        .collect();
    serde_json::Value::Array(v)
}

// ---------------------------------------------------------------------------
// Emulator snapshot helpers
// ---------------------------------------------------------------------------

fn snap(inner: &AppInner) -> Option<BattleState> {
    inner.battle.lock().unwrap().clone()
}

/// Wait for the FIGHT menu after send-out. The 6 A-taps queued by `setup` usually drive the intro
/// to the menu, so we mostly just WAIT (extra early A's would open the move list and corrupt the
/// first move selection). After ~2.5s with no menu, sparsely nudge with A as a fallback for intro
/// variations. Returns true if the FIGHT menu was reached, false on timeout (battle never started).
async fn advance_to_menu(inner: &AppInner) -> bool {
    for i in 0..130 {
        if let Some(s) = snap(inner) {
            if s.in_battle != 0 && s.menu == MenuPhase::MainMenu {
                return true;
            }
            if i > 20 && i % 12 == 0 && s.in_battle != 0 {
                let _ = inner.action_tx.send(AgentAction::Buttons { presses: vec!["A".into()] });
            }
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    false
}

/// Drive one round to completion and return the resolving snapshot (or the last in-battle snapshot
/// if the battle ended — it carries the faint, for winner detection).
///
/// While the FIGHT top menu is still showing (`MainMenu`), the player's move hasn't been taken —
/// (re)issue it (the first A as the menu renders is occasionally dropped, and an idle open move
/// list also reads as MainMenu, so a fresh macro re-selects). While animating / showing result or
/// status text, advance it with A (status moves like Sing pause for an A or the turn never
/// completes). Returns the instant `turns_in_battle` changes, leaving the menu clean for next round.
async fn run_round(inner: &AppInner, last_turn: u8, p1slot: u8) -> Option<BattleState> {
    let mut last: Option<BattleState> = None;
    let mut tick = 0u32;
    for _ in 0..360 {
        if let Some(s) = snap(inner) {
            if s.in_battle == 0 {
                return last;
            }
            if s.turns_in_battle != last_turn {
                return Some(s);
            }
            last = Some(s.clone());
            if s.menu == MenuPhase::MainMenu {
                // move not taken yet: (re)issue it now and every ~1.25s if it keeps getting dropped
                if tick % 5 == 0 {
                    let _ = inner.action_tx.send(AgentAction::Move { slot: p1slot });
                }
                tick += 1;
            } else {
                // animating / result / status text: advance it
                tick = 0;
                let _ = inner.action_tx.send(AgentAction::Buttons { presses: vec!["A".into()] });
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    last
}

async fn update_last_alive(game: &Arc<GameState>, rid: RoomId, s: &BattleState) {
    let mut rooms = game.rooms.lock().await;
    if let Some(r) = rooms.get_mut(&rid) {
        if s.player.hp > 0 {
            r.last_alive = Seat::P1;
        } else if s.enemy.hp > 0 {
            r.last_alive = Seat::P2;
        }
    }
}

async fn decide_winner(game: &Arc<GameState>, rid: RoomId, last: Option<&BattleState>) -> Option<Seat> {
    let fallback = game.rooms.lock().await.get(&rid).map(|r| r.last_alive).unwrap_or(Seat::P1);
    match last {
        Some(s) if s.player.hp > 0 && s.enemy.hp == 0 => Some(Seat::P1),
        Some(s) if s.enemy.hp > 0 && s.player.hp == 0 => Some(Seat::P2),
        _ => Some(fallback),
    }
}

// ---------------------------------------------------------------------------
// Broadcasts
// ---------------------------------------------------------------------------

async fn broadcast_to_room(game: &Arc<GameState>, rid: RoomId, msg: serde_json::Value) {
    let users = {
        let rooms = game.rooms.lock().await;
        rooms.get(&rid).map(|r| (r.p1.user_id, r.p2.user_id))
    };
    if let Some((a, b)) = users {
        game.ws.send_to(a, msg.clone()).await;
        game.ws.send_to(b, msg).await;
    }
}

async fn set_phase(game: &Arc<GameState>, rid: RoomId, phase: RoomPhase) {
    {
        let mut rooms = game.rooms.lock().await;
        if let Some(r) = rooms.get_mut(&rid) {
            r.phase = phase;
        }
    }
    let _ = e_rooms::ActiveModel {
        id: Set(rid),
        phase: Set(phase.as_str().to_string()),
        ..Default::default()
    }
    .update(&game.db)
    .await;
    broadcast_room_state(game, rid).await;
}

async fn broadcast_room_state(game: &Arc<GameState>, rid: RoomId) {
    let (a, b) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (r.p1.user_id, r.p2.user_id),
        None => return,
    };
    send_room_state_to(game, rid, a).await;
    send_room_state_to(game, rid, b).await;
}

async fn send_room_state_to(game: &Arc<GameState>, rid: RoomId, uid: UserId) {
    let msg = {
        let rooms = game.rooms.lock().await;
        let r = match rooms.get(&rid) {
            Some(r) => r,
            None => return,
        };
        let seat = match r.seat_of(uid) {
            Some(s) => s,
            None => return,
        };
        let (you, opp) = match seat {
            Seat::P1 => (&r.p1, &r.p2),
            Seat::P2 => (&r.p2, &r.p1),
        };
        json!({
            "type":"room_state","room_id":rid,"phase":r.phase.as_str(),"seat":seat_num(seat),"level":r.level,
            "you":{"username":you.username,"species":you.species,"dex":dex_of(you.species)},
            "opp":{"username":opp.username,"species":opp.species,"dex":dex_of(opp.species)},
        })
    };
    game.ws.send_to(uid, msg).await;
}

async fn broadcast_slot_result(game: &Arc<GameState>, rid: RoomId) {
    let (a, b, p1sp, p2sp) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (r.p1.user_id, r.p2.user_id, r.p1.species, r.p2.species),
        None => return,
    };
    // Each player sees their own roll as "you".
    game.ws
        .send_to(a, json!({"type":"slot_result","you_species":p1sp,"opp_species":p2sp,"you_dex":dex_of(p1sp),"opp_dex":dex_of(p2sp)}))
        .await;
    game.ws
        .send_to(b, json!({"type":"slot_result","you_species":p2sp,"opp_species":p1sp,"you_dex":dex_of(p2sp),"opp_dex":dex_of(p1sp)}))
        .await;
}

async fn broadcast_battle_state(game: &Arc<GameState>, rid: RoomId) {
    let s = match snap(&game.inner) {
        Some(s) => s,
        None => return,
    };
    let (a, b) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (r.p1.user_id, r.p2.user_id),
        None => return,
    };
    let mon = |m: &BattlePokemon| json!({"species":m.species,"hp":m.hp,"max_hp":m.max_hp,"status":m.status});
    let menu = format!("{:?}", s.menu).to_lowercase();
    game.ws
        .send_to(a, json!({"type":"battle_state","in_battle":s.in_battle,"turn":s.turns_in_battle,"you":mon(&s.player),"opp":mon(&s.enemy),"menu":menu}))
        .await;
    game.ws
        .send_to(b, json!({"type":"battle_state","in_battle":s.in_battle,"turn":s.turns_in_battle,"you":mon(&s.enemy),"opp":mon(&s.player),"menu":menu}))
        .await;
}

async fn broadcast_winner(game: &Arc<GameState>, rid: RoomId, winner: Option<Seat>) {
    let (a, b) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (r.p1.user_id, r.p2.user_id),
        None => return,
    };
    let wseat = winner.map(seat_num).unwrap_or(0);
    game.ws.send_to(a, json!({"type":"winner","seat":wseat,"you_won":wseat==1})).await;
    game.ws.send_to(b, json!({"type":"winner","seat":wseat,"you_won":wseat==2})).await;
}

/// On WS (re)connect during a live phase, re-push the phase-specific message so the client paints.
async fn push_live_phase(game: &Arc<GameState>, rid: RoomId, uid: UserId) {
    let phase = match game.rooms.lock().await.get(&rid) {
        Some(r) => r.phase,
        None => return,
    };
    match phase {
        RoomPhase::SlotMachine => broadcast_slot_result(game, rid).await,
        RoomPhase::Battle => broadcast_battle_state(game, rid).await,
        _ => {}
    }
    let _ = uid;
}

// ---------------------------------------------------------------------------
// DB helpers + resume/recovery
// ---------------------------------------------------------------------------

pub(crate) async fn username_of(db: &DatabaseConnection, uid: UserId) -> String {
    e_users::Entity::find_by_id(uid)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|u| u.username)
        .unwrap_or_else(|| format!("user{uid}"))
}

async fn user_stats(db: &DatabaseConnection, uid: UserId) -> (i32, i32) {
    e_users::Entity::find_by_id(uid)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|u| (u.wins, u.losses))
        .unwrap_or((0, 0))
}

async fn record_result(game: &Arc<GameState>, rid: RoomId, winner: Option<Seat>) {
    let (p1u, p2u, p1sp, p2sp, public_id, p1n, p2n) = match game.rooms.lock().await.get(&rid) {
        Some(r) => (
            r.p1.user_id,
            r.p2.user_id,
            r.p1.species as i32,
            r.p2.species as i32,
            r.public_id.clone(),
            r.p1.username.clone(),
            r.p2.username.clone(),
        ),
        None => return,
    };
    let wseat = winner.map(|s| seat_num(s) as i32);
    if let Some(ws) = wseat {
        let (win_u, lose_u) = if ws == 1 { (p1u, p2u) } else { (p2u, p1u) };
        inc_stat(&game.db, win_u, true).await;
        inc_stat(&game.db, lose_u, false).await;
    }
    let _ = e_rooms::ActiveModel {
        id: Set(rid),
        winner_seat: Set(wseat),
        ended_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .update(&game.db)
    .await;
    let _ = e_matches::ActiveModel {
        room_id: Set(rid),
        p1_user: Set(p1u),
        p2_user: Set(p2u),
        p1_species: Set(p1sp),
        p2_species: Set(p2sp),
        winner_seat: Set(wseat),
        ended_at: Set(Utc::now()),
        public_id: Set(Some(public_id)),
        p1_name: Set(Some(p1n)),
        p2_name: Set(Some(p2n)),
        ..Default::default()
    }
    .insert(&game.db)
    .await;
}

async fn inc_stat(db: &DatabaseConnection, uid: UserId, won: bool) {
    if let Ok(Some(u)) = e_users::Entity::find_by_id(uid).one(db).await {
        let mut am: e_users::ActiveModel = u.clone().into();
        if won {
            am.wins = Set(u.wins + 1);
        } else {
            am.losses = Set(u.losses + 1);
        }
        let _ = am.update(db).await;
    }
}

async fn clear_user_room(game: &Arc<GameState>, rid: RoomId) {
    let users = game.rooms.lock().await.get(&rid).map(|r| (r.p1.user_id, r.p2.user_id));
    if let Some((a, b)) = users {
        let mut ur = game.user_room.lock().await;
        ur.remove(&a);
        ur.remove(&b);
    }
    let _ = e_ur::Entity::delete_many()
        .filter(e_ur::Column::RoomId.eq(rid))
        .exec(&game.db)
        .await;
}

/// Where is this user right now? `None` => Lobby. Used by GET /api/me for first-paint routing.
pub async fn current_room_for(st: &AppState, uid: UserId) -> Option<MeRoom> {
    // Coordinator: the user's battle runs on a worker, so read it from the pool (the local rooms
    // map is empty here). This is what tells the room page "you are the player in this room".
    if st.role == crate::signaling::Role::Coordinator {
        return st
            .pool
            .as_ref()
            .and_then(|p| p.room_for_user(uid))
            .map(|(id, seat)| MeRoom { id, phase: "battle".to_string(), seat });
    }
    let rid = *st.game.user_room.lock().await.get(&uid)?;
    let rooms = st.game.rooms.lock().await;
    let r = rooms.get(&rid)?;
    Some(MeRoom {
        id: r.public_id.clone(),
        phase: r.phase.as_str().to_string(),
        seat: r.seat_of(uid).map(seat_num).unwrap_or(0),
    })
}

/// Public spectator info for a room UUID. If the match already finished it's read from history
/// (winner + which Pokemon), so the result shows even after the room is gone. Otherwise the live
/// room (who's fighting + whether it's the active battle). None for an unknown link.
pub async fn spectate_info(st: &AppState, public_id: &str) -> Option<serde_json::Value> {
    // 1) Finished match? Show the recorded result.
    if let Ok(Some(m)) = e_matches::Entity::find()
        .filter(e_matches::Column::PublicId.eq(public_id))
        .order_by_desc(e_matches::Column::Id)
        .one(&st.db)
        .await
    {
        let mon = |sp: i32| crate::battle::species_by_index(sp as u8).map(|s| s.name).unwrap_or("?");
        return Some(json!({
            "found": true,
            "ended": true,
            "winner": m.winner_seat.map(|w| if w == 1 { "p1" } else { "p2" }),
            "p1": m.p1_name.unwrap_or_default(),
            "p2": m.p2_name.unwrap_or_default(),
            "p1_mon": mon(m.p1_species),
            "p2_mon": mon(m.p2_species),
        }));
    }
    // 2) Coordinator: a live battle runs on a worker — read it from the pool (local rooms is empty).
    if st.role == crate::signaling::Role::Coordinator {
        return st.pool.as_ref().and_then(|p| p.battle_names(public_id)).map(|(p1, p2)| {
            json!({ "found": true, "ended": false, "live": true, "phase": "battle", "p1": p1, "p2": p2 })
        });
    }
    // 3) Otherwise (solo/worker), a live / pending room in this process.
    let active = *st.game.active_room.lock().await;
    let rooms = st.game.rooms.lock().await;
    let r = rooms.values().find(|r| r.public_id == public_id)?;
    Some(json!({
        "found": true,
        "ended": false,
        "live": active == Some(r.id),
        "phase": r.phase.as_str(),
        "p1": r.p1.username,
        "p2": r.p2.username,
    }))
}

/// Live battles on THIS process (solo/worker: 0 or 1). The coordinator uses its pool instead.
pub async fn live_battles(game: &Arc<GameState>) -> Vec<serde_json::Value> {
    let active = *game.active_room.lock().await;
    let rooms = game.rooms.lock().await;
    active
        .and_then(|rid| rooms.get(&rid))
        .map(|r| vec![json!({ "id": r.public_id, "p1": r.p1.username, "p2": r.p2.username })])
        .unwrap_or_default()
}

/// Restart cleanup: any room still live in the DB had its in-process emulator state lost. Mark it
/// abandoned (winner NULL) and clear user_room so affected users resume to the Lobby.
pub async fn recover_abandoned(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    use sea_orm::sea_query::Expr;
    let now = Utc::now();
    e_rooms::Entity::update_many()
        .col_expr(e_rooms::Column::Phase, Expr::value(RoomPhase::Done.as_str()))
        .col_expr(e_rooms::Column::EndedAt, Expr::value(now))
        .filter(e_rooms::Column::EndedAt.is_null())
        .exec(db)
        .await?;
    e_ur::Entity::delete_many().exec(db).await?;
    Ok(())
}

// ---------------------------------------------------------------------------

fn dex_of(internal_index: u8) -> u16 {
    SPECIES
        .iter()
        .position(|s| s.species == internal_index)
        .map(|p| (p + 1) as u16)
        .unwrap_or(0)
}
