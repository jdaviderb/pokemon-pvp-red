# Multiplayer Online Battle — Architecture & Integration Design (v1)

Turn the single-player Pokémon-Red-over-WebRTC server into a **2-player online game**:
register/login -> Find Match -> matched into a **room** -> slot-machine assigns each player a
random Pokémon -> turn-based battle (15s/move, CPU auto-picks on timeout) -> winner -> Home.
F5/refresh resumes you into your room until the battle ends.

This document is a concrete build plan: it reuses the existing engine (`pipeline.rs` +
`battle.rs`) verbatim and adds an authoritative game/room layer, auth, a DB, and a per-client
WebSocket. It is written so that the v1 "one shared emulator" can later become "one emulator
worker-process per room" with no client/protocol changes.

---

## 0. What we reuse vs. what is new

### Reused unchanged (read in `src/{pipeline,battle,signaling,webrtc}.rs`)
- **`pipeline::AppInner`** — the one emulator on one OS thread. The libretro core uses
  process-GLOBAL static buffers, so there is exactly **one** `AppInner` per process. Already
  exposes everything the room engine needs:
  - `setup_tx: SetupReq{player, enemy, level, player_name, enemy_name, reply}` — injects a fresh
    1v1 matchup (loads `states/legendary_intro.state`, injects both party slots, drives send-out).
  - `action_tx: AgentAction` — the **YOU** side move: `Move{slot}` / `Switch` / `Run` / `Buttons`.
  - `enemy_force: Arc<AtomicU8>` — forces `wEnemySelectedMove` (CCDD) every turn = the **enemy**
    side move; `0xFF` = game AI. This is how **player 2** drives the opponent.
  - `battle: Arc<Mutex<Option<BattleState>>>` — latest WRAM snapshot, refreshed every frame.
  - `video_tx` / `audio_tx` broadcast channels + `keyframe_req`.
- **`battle::BattleState`** — `in_battle` (D057), `menu` (`MainMenu` = FIGHT menu ready),
  `turns_in_battle` (CCD5), `player`/`enemy` `BattlePokemon{ hp, max_hp, moves[4], pp[4], ... }`.
- **`battle::SPECIES`** — 151 Gen-1 rows; `s.species` is the internal index byte the API POSTs.
- **`webrtc::build_peer_and_answer`** — one peer per browser `POST /offer`. Both players in a room
  just call `/offer` and subscribe to the same broadcast => both watch the **same** screen. KEEP.

### New modules / files
```
src/db.rs        SQLite pool + migrations + row structs + queries (users, sessions, rooms, matches)
src/auth.rs      register/login handlers, password hashing (argon2), session-cookie middleware
src/game.rs      Room FSM + Matchmaker + RoomEngine (the authoritative game loop) + turn timer
src/ws.rs        per-client WebSocket: server->client events, client->server intents; hub/registry
src/signaling.rs (EDIT) merge new routers; AppState gains GameState; keep /battle/* gated to admin
src/main.rs      (EDIT) build DB pool + GameState, spawn matchmaker + room-engine tasks
static/login.html    register/login page
static/lobby.html    Home/Lobby: "Find Match" + queue status (the new landing page)
static/room.html     room view: shared WebRTC <video> + slot machine + move buttons + timer
static/sprites/001.png ... 151.png   (151 front sprites, by NATIONAL DEX number — see §9)
```

### New crates (add to `Cargo.toml`)
```toml
tokio-tungstenite = "0.24"          # OR use axum 0.8 built-in `axum::extract::ws` (preferred; no extra dep)
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
argon2 = "0.5"                       # password hashing
rand = "0.8"                         # slot-machine RNG, session tokens, CPU random move
tower-cookies = "0.10"               # OR hand-roll Set-Cookie; cookies carry the session token
time = "0.3"                         # timestamps (sqlx already needs a time/chrono feature)
```
> **Preferred:** use `axum::extract::ws::WebSocketUpgrade` (axum 0.8 already in tree) for the WS,
> so the only genuinely new deps are `sqlx`, `argon2`, `rand`, `tower-cookies`. axum 0.8's `ws`
> feature is on by default. This keeps the dependency surface minimal.

---

## 1. Room finite-state machine (server-authoritative)

A **Room** is the unit of a single shared battle. Its lifecycle:

```
                 (player presses Find Match)
   Home/Lobby ───────────────────────────────► Queue
       ▲                                          │ matchmaker pairs 2 waiting players
       │                                          ▼
       │                                    Matched(room)   <-- room row created, both users -> room
       │                                          │ when emulator is FREE (v1: single active room)
       │                                          ▼
       │                                    SlotMachine      <-- server rolls 2 random species,
       │                                          │              streams reel frames, then locks
       │                                          ▼
       │                                      Setup           <-- POST setup_tx{p1species,p2species,level}
       │                                          │              engine injects + drives send-out
       │                                          ▼
       │                                      Battle          <-- turn loop: 15s/side, CPU on timeout
       │                                          │              ends when in_battle==0
       │                                          ▼
       └──────────────────────────────────── Result(winner)  <-- 5s banner, then both -> Home
```

### Room phase enum (authoritative; lives in `game.rs`)
```rust
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomPhase {
    Matched,          // both assigned, waiting for the emulator slot (or other player to (re)connect)
    SlotMachine,      // reels spinning; server has already decided the result
    Setup,            // engine injecting + send-out animation playing (BattleIntro)
    Battle,           // turn loop active
    Result,           // winner decided; result banner
    Done,             // room closed; users sent Home
}
```

### Room in-memory state
```rust
pub struct Room {
    pub id: RoomId,                       // u64 / uuid; matches DB row
    pub phase: RoomPhase,
    pub p1: PlayerSeat,                   // YOU side  -> action_tx
    pub p2: PlayerSeat,                   // ENEMY side-> enemy_force
    pub level: u8,                        // both mons same level (default 50)
    pub turn: Seat,                       // whose move is currently NEEDED (P1 or P2)
    pub turn_deadline: Option<Instant>,   // 15s wall clock for the current move
    pub winner: Option<Seat>,
    pub created_at: Instant,
    pub last_turns_in_battle: u8,         // CCD5 edge-detector for "turn resolved"
}

pub struct PlayerSeat {
    pub user_id: UserId,
    pub species: u8,                      // internal index assigned by the slot machine
    pub committed_move: Option<u8>,       // slot 0..3 the player chose this turn (None = waiting)
    pub connected: bool,                  // has a live WS right now (F5 toggles this)
}

#[derive(Clone, Copy, PartialEq, Eq)] pub enum Seat { P1, P2 }
```

The Room transitions are driven ONLY by the server (matchmaker + room-engine tick), never by a
client message. Clients send **intents** (`find_match`, `commit_move`); the server validates them
against the current phase and the player's seat, mutates the Room, and broadcasts the new state.

---

## 2. Matchmaking

### Data model (DB rows + in-memory)

DB (SQLite via sqlx; file `data/game.db`). Migrations in `migrations/0001_init.sql`:

```sql
CREATE TABLE users (
  id            INTEGER PRIMARY KEY,
  username      TEXT UNIQUE NOT NULL,
  pw_hash       TEXT NOT NULL,           -- argon2id PHC string
  created_at    INTEGER NOT NULL,        -- unix secs
  wins          INTEGER NOT NULL DEFAULT 0,
  losses        INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sessions (
  token         TEXT PRIMARY KEY,        -- random 32-byte hex; stored in cookie `sid`
  user_id       INTEGER NOT NULL REFERENCES users(id),
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL
);

CREATE TABLE rooms (
  id            INTEGER PRIMARY KEY,
  phase         TEXT NOT NULL,           -- mirrors RoomPhase (for F5 resume across restarts)
  p1_user       INTEGER NOT NULL REFERENCES users(id),
  p2_user       INTEGER NOT NULL REFERENCES users(id),
  p1_species    INTEGER,                 -- NULL until slot machine locks
  p2_species    INTEGER,
  level         INTEGER NOT NULL DEFAULT 50,
  winner_seat   INTEGER,                 -- 1 or 2, NULL until Result
  created_at    INTEGER NOT NULL,
  ended_at      INTEGER                  -- NULL while live
);

-- user -> current room mapping for F5 resume. One active room per user max.
CREATE TABLE user_room (
  user_id       INTEGER PRIMARY KEY REFERENCES users(id),
  room_id       INTEGER NOT NULL REFERENCES rooms(id)
);
```

In-memory matchmaking (in `GameState`, behind a `tokio::sync::Mutex`):
```rust
pub struct GameState {
    pub inner: Arc<AppInner>,             // the one emulator
    pub db: SqlitePool,
    pub queue: Mutex<VecDeque<UserId>>,   // FIFO of users who pressed Find Match (not yet roomed)
    pub rooms: Mutex<HashMap<RoomId, Room>>,
    pub user_room: Mutex<HashMap<UserId, RoomId>>,  // hot cache of the user_room table
    pub active_room: Mutex<Option<RoomId>>,         // v1: the ONE room currently using the emulator
    pub ws_hub: WsHub,                    // user_id -> live ws sender(s) (see §6)
    pub emu_busy: AtomicBool,             // is the single emulator currently driving a battle?
}
```

### The matchmaker task (one `tokio::spawn` loop, ~250 ms tick)
1. Lock `queue`. While `queue.len() >= 2`: pop two distinct user ids `(a, b)`.
   - Create a `rooms` DB row (phase `Matched`, p1=a, p2=b, level=50, species NULL).
   - Insert `user_room` rows for both; update the in-memory `user_room` cache.
   - Build a `Room` in `rooms` map; **do not** start the battle yet.
   - Push the room id onto a `pending_rooms` FIFO.
   - Emit `matched{room_id, opponent, seat}` to both users over their WS.
2. **v1 single-emulator gate:** if `active_room` is `None` and `pending_rooms` is non-empty, pop
   the next pending room, set `active_room = Some(room_id)`, and hand it to the **room engine**
   (transition `Matched -> SlotMachine`). All other pending rooms STAY in `Matched` and their
   players see "waiting for an open arena…" until the active room reaches `Done`.

> **Why a queue + pending list rather than starting on pairing:** because only one emulator exists,
> pairing (cheap, DB-only) is decoupled from arena allocation (the scarce resource). When v2 adds
> worker processes, the "active_room gate" becomes "assign room to a free worker"; everything else
> is unchanged.

---

## 3. Mapping a Room onto the existing engine

The **room engine** is one async task per *active* room (v1: at most one runs at a time). It owns
the translation Room-FSM <-> `AppInner`. Seat -> engine channel mapping:

| Room concept            | Engine call                                                              |
|-------------------------|--------------------------------------------------------------------------|
| Start the matchup       | `setup_tx.send(SetupReq{ player:p1.species, enemy:p2.species, level, player_name:p1.username, enemy_name:p2.username, reply })` and `await` the reply |
| **P1** commits move `s` | `action_tx.send(AgentAction::Move{ slot: s })`  (drives the **YOU** side) |
| **P2** commits move `s` | `enemy_force.store(s, Relaxed)` (forces CCDD = the **enemy** side this turn); after the turn resolves, store `0xFF` again so it doesn't repeat |
| Read battle             | clone `*inner.battle.lock()` -> `BattleState`                            |
| Battle over?            | `state.in_battle == 0`                                                    |
| Winner                  | side whose `hp > 0` at battle end (the loser's hp hit 0)                  |

### P2 = enemy side: the one subtlety
`enemy_force` forces the enemy move **every frame the engine is waiting on CCDD**. After P2's move
is consumed (turn resolves, `turns_in_battle` increments), the engine must reset
`enemy_force = 0xFF` so the *next* turn waits for a fresh P2 choice instead of silently repeating
the last move. Concretely, in the room-engine turn loop:

```
on P2 commit(slot):  enemy_force.store(slot)         // arm for this turn
on turn resolved:    enemy_force.store(0xFF)         // disarm; require a new P2 choice next turn
```

`pipeline.rs` already only writes CCDD when `in_battle != 0 && CCDD != 0` (i.e. the engine is
actually asking the enemy to choose), so arming early is safe — it takes effect exactly when the
engine reads it.

### Determining the winner (authoritative end detection)
The room engine polls the snapshot each tick (e.g. 100 ms). Battle-end logic:
```rust
let s = snapshot();                  // BattleState
if s.in_battle == 0 && room.phase == Battle {
    // battle just ended. Winner = the side that still has HP.
    let winner = if s.player.hp > 0 && s.enemy.hp == 0 { Seat::P1 }
                 else if s.enemy.hp > 0 && s.player.hp == 0 { Seat::P2 }
                 else {
                     // both 0 (selfdestruct / tie) or ambiguous -> use last-known nonzero HP,
                     // else fall back to "the side that did NOT faint last". Tie -> P1 by rule.
                     last_alive_seat(room)
                 };
    finish(room, winner);
}
```
Because the engine snapshot updates every frame, also keep `room.last_alive_seat` updated each tick
while `in_battle != 0` so the `0/0` selfdestruct edge case has a deterministic answer.

> **Guard:** ignore `in_battle == 0` during `Setup` (the intro state briefly has `D014==0`); only
> evaluate end-of-battle once `phase == Battle` and we have seen `in_battle != 0` at least once
> (a `battle_began` latch set when the first `MainMenu` is observed).

---

## 4. Both players watch the SAME stream

No change to WebRTC. v1 has ONE emulator => ONE pair of broadcast channels => **the single shared
screen**. Each player's `room.html`:
1. After being placed in a room (via WS `room_state`), runs the **existing** `POST /offer` flow
   (recvonly video+audio + an `input` data channel — though input is now ignored for battles).
2. Subscribes to `inner.video_tx` / `audio_tx` like any viewer today.

Both peers therefore render the identical room screen. The `keyframe_req` already fires on each new
`Connected` peer, so the second joiner gets a clean keyframe.

> **v2 scaling note:** when each room gets its own emulator worker process, `POST /offer` becomes
> `POST /room/{id}/offer`, routed to that worker's broadcast channels. The client change is one URL.
> Keep the offer body identical so `room.html` is forward-compatible.

> **Spectators / fairness:** in v1 both players see the same pixels, including the FIGHT-menu
> cursor movements driven by P1's `action_tx` macro. That's acceptable (it's literally one shared
> Game Boy). Move *selection* is via the WS/HTTP intent + on-screen buttons, NOT by watching the
> menu, so there's no input-timing advantage.

---

## 5. Turn timer (server-authoritative 15s/side; CPU picks on timeout)

### How the server knows a move is needed, and whose
A move is needed exactly when the engine is **idle at the FIGHT menu**:
`state.in_battle != 0 && state.menu == MenuPhase::MainMenu`
(`MainMenu` already means: in battle, player mon on field, no input macro in flight — see
`battle::next_menu_phase`).

Gen-1 resolves **both** sides' moves within one engine turn (`turns_in_battle` / CCD5 increments
once per round). So the room models a **round** as: collect P1's move, then P2's move, then let the
engine execute the round. Sequencing in the room engine:

```
Round loop (while in_battle != 0):
  wait until menu == MainMenu                         // FIGHT menu ready => start of a round
  room.turn = P1; arm 15s deadline; emit your_turn{seat:1} + timer{15}
  await P1.committed_move (intent or timeout)
      timeout -> pick random legal move for P1 (see below)
  action_tx.send(Move{slot: p1_move})                 // YOU side; engine plays the macro

  room.turn = P2; arm 15s deadline; emit your_turn{seat:2} + timer{15}
  await P2.committed_move (intent or timeout)
      timeout -> random legal move for P2
  enemy_force.store(p2_move)                           // ENEMY side; forces CCDD this round

  // engine now executes the round (animations); menu leaves MainMenu (Animating)
  wait until turns_in_battle increments OR in_battle == 0
  enemy_force.store(0xFF)                               // disarm for next round
  clear both committed_move; emit battle_state snapshot
```

> **Why P1 then P2 (sequential), not simultaneous:** P1's move is submitted as button taps that
> open the FIGHT menu and pick the slot; the engine then waits on CCDD for the enemy. Submitting P1
> first lets the macro reach the point where CCDD is read, at which moment P2's forced value is
> consumed. Both 15s windows can overlap in the UI (both players see the timer and can pre-select),
> but the server commits them in this order. **Simpler v1 alternative:** run both 15s timers in
> parallel (each player independently commits any time during the round); the engine just needs
> both values before it executes — submit P1 to `action_tx` and P2 to `enemy_force` as each
> arrives, in the P1-then-P2 order. The deadline is per-player, 15s from round start.

### Picking a random legal move on timeout
The CPU fallback uses the snapshot's move list for that seat:
```rust
fn random_legal_move(mon: &BattlePokemon, rng: &mut impl Rng) -> u8 {
    let legal: Vec<u8> = (0..4)
        .filter(|&i| mon.moves[i] != 0 && (mon.pp[i] & 0x3f) != 0)  // has a move + PP left
        .map(|i| i as u8)
        .collect();
    *legal.choose(rng).unwrap_or(&0)   // slot 0 always exists; Struggle handles all-zero-PP
}
```
On timeout the server submits this exactly as if the player chose it, broadcasts `move_auto{seat,
slot}`, and proceeds. The 15s deadline lives in `room.turn_deadline: Instant`; the room engine
checks `Instant::now() >= deadline` each 100 ms tick (also `tokio::select!`-able against the WS
commit so a real choice short-circuits the wait).

### Timer broadcast
Server emits `timer{seat, seconds_left}` once per second so the client renders a countdown without
trusting client clocks. The authoritative deadline is the server `Instant`; the client display is
cosmetic.

---

## 6. Realtime client<->server signaling: WebSocket (`src/ws.rs`)

One **WebSocket per logged-in client**, established right after login, carrying JSON for lobby/room/
slot/turn state. WebRTC stays separate (it carries media only). Endpoint:

```
GET /ws            (upgrades; auth via the `sid` session cookie -> user_id)
```

### WsHub (registry)
```rust
pub struct WsHub { conns: Mutex<HashMap<UserId, Vec<mpsc::UnboundedSender<ServerMsg>>>> }
// send_to(user_id, msg) fans out to all that user's tabs; remove dead senders on send error.
// On every (re)connect, the hub immediately pushes the user's current lobby/room state (resume).
```

A user may have multiple tabs; the hub fans out to all, so F5 in any tab shows the same state.

### Server -> Client events (`ServerMsg`, `#[serde(tag="type", rename_all="snake_case")]`)
```jsonc
{ "type":"hello",        "user": {"id":7,"username":"ash","wins":3,"losses":1} }
{ "type":"lobby",        "queued": false, "queue_size": 0 }          // you're Home, not queued
{ "type":"queued",       "position": 2 }                            // you pressed Find Match
{ "type":"matched",      "room_id": 42, "seat": 1, "opponent":"misty" }
{ "type":"room_state",   "room_id":42, "phase":"slot_machine", "seat":1,
                          "you":  {"username":"ash",  "species": null, "hp":null, "max_hp":null},
                          "opp":  {"username":"misty","species": null, "hp":null, "max_hp":null},
                          "level":50 }
{ "type":"slot_spin",    "seat":1, "tick": 12, "shown_species": 84 } // reel frame (cosmetic stream)
{ "type":"slot_result",  "you_species": 84, "opp_species": 6, "you_name":"GENGAR?", "opp_name":"CHARIZARD?" }
{ "type":"your_turn",    "seat": 1, "deadline_ms": 15000,
                          "moves": [{"slot":0,"id":85,"name":"Thunderbolt","pp":15}, ...] }
{ "type":"timer",        "seat": 1, "seconds_left": 9 }
{ "type":"move_auto",    "seat": 2, "slot": 1 }                      // CPU picked for the (other) player
{ "type":"battle_state", "in_battle":2, "turn":7,
                          "you":{"species":84,"hp":120,"max_hp":160,"status":0},
                          "opp":{"species":6, "hp":40, "max_hp":150,"status":0},
                          "menu":"main_menu" }
{ "type":"winner",       "seat": 1, "you_won": true }                // -> client returns Home after banner
{ "type":"room_closed",  "reason":"battle_ended" }                  // go Home; queue is empty
{ "type":"error",        "code":"not_your_turn", "message":"..." }
```

### Client -> Server intents (`ClientMsg`, same tagging)
```jsonc
{ "type":"find_match" }                 // valid only when Home; enqueue
{ "type":"cancel_queue" }               // leave the queue (only while queued, not yet roomed)
{ "type":"commit_move", "slot": 2 }     // valid only when phase==Battle && room.turn==my seat
{ "type":"resume" }                     // "where am I?" — server replies with lobby/room_state
{ "type":"ping" }                       // keepalive (server replies pong); also detects liveness
```

### Validation (server-authoritative)
Every intent is checked against `(user -> room -> phase -> seat)`. Illegal intents (e.g.
`commit_move` when it isn't your turn, or `find_match` while already in a room) get an `error`
event and are dropped. The client UI also disables buttons, but the server is the source of truth.

### Why WebSocket (not SSE / polling)
- Bidirectional: clients push `commit_move` with low latency; server pushes `your_turn`/`timer`.
- One connection multiplexes lobby + room + turn + result.
- Liveness: WS close = `connected=false` for F5 detection (see §7). axum's `ws` is already
  available; no new transport dep.
- The current `/battle/state` 600 ms HTTP poll is replaced by `battle_state` pushes (but the HTTP
  endpoint stays for the admin/single-player console — see §8).

---

## 7. F5 / refresh persistence (cannot leave until battle ends)

### Server tracks user -> room in BOTH the DB (`user_room`, `rooms`) and the in-memory cache.
On (re)load the client does:
1. `GET /api/me` (cookie auth) -> `{user, room_id?}` for the initial HTML routing decision, OR
2. simply open `/ws` and send `{"type":"resume"}` — the hub replies with the current `lobby` or
   `room_state` (+ `slot_result` / `your_turn` / `winner` as appropriate for the live phase).

### Resume endpoint (HTTP, for the very first paint before WS connects)
```
GET /api/me  ->  200 { "user": {...}, "room": { "id":42, "phase":"battle", "seat":1 } | null }
```
The static pages use this to route: no session -> `login.html`; session + no room ->
`lobby.html`; session + room -> `room.html?id=42`. `room.html` then opens `/ws`, sends `resume`,
re-`POST /offer` for the shared video, and rebuilds the UI from `room_state`.

### "Cannot leave until the battle ends"
- There is no client-facing "leave room" intent while `phase ∈ {SlotMachine, Setup, Battle}`.
  `find_match`/`cancel_queue` are rejected with `error{code:"in_room"}`.
- A WS disconnect (closed tab / network blip) sets `seat.connected = false` but does **NOT** remove
  the user from the room or end the battle. The room engine keeps running; the timer keeps ticking;
  if the disconnected player's turn times out, the CPU plays for them (that's the existing 15s
  rule). On reconnect, `resume` drops them straight back into the live battle.
- Only at `phase == Result` (after `winner`) does the client get a **Return Home** action, which
  the server honors by clearing `user_room` for that user. The room itself is torn down by the
  engine (see §10), not by either client leaving.
- **Server restart while a battle is live:** the emulator state is in-process and lost on restart
  (no per-room savestate in v1). On boot, the server marks any `rooms` row with `ended_at IS NULL`
  as abandoned (`phase=Done`, `winner_seat=NULL`), clears `user_room`, and resumes users to the
  Lobby. (v2: persist a per-room savestate via the existing `save_tx`/`load_tx` to truly survive
  restarts.)

---

## 8. Auth (`src/auth.rs`) + how it plugs into axum without breaking single-player

### Endpoints
```
POST /api/register  {username, password}  -> set-cookie sid; 200 {user} | 409 username taken
POST /api/login     {username, password}  -> set-cookie sid; 200 {user} | 401
POST /api/logout                          -> clear cookie; delete session row
GET  /api/me                              -> {user, room?} (see §7) | 401
```
- Passwords hashed with **argon2id** (`argon2` crate); store the PHC string in `users.pw_hash`.
- Session: 32-byte random hex token in an **HttpOnly, SameSite=Lax** cookie `sid`; row in
  `sessions` with a 30-day expiry. A small `auth` extractor (axum `FromRequestParts`) reads the
  cookie, looks up the session, and yields `AuthUser{ id, username }`. `/ws`, `/api/me`,
  `/api/logout`, and all room intents require it.

### Router composition (`signaling.rs`)
`AppState` grows a `game: Arc<GameState>` (which itself holds `inner: Arc<AppInner>` and `db`).
```rust
#[derive(Clone)]
pub struct AppState {
    pub api: Arc<API>,
    pub inner: Arc<AppInner>,     // kept for /offer + the admin console
    pub game: Arc<GameState>,     // NEW: DB, queue, rooms, ws hub
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // --- media (unchanged) ---
        .route("/offer", post(offer_handler))
        // --- auth ---
        .route("/api/register", post(auth::register))
        .route("/api/login",    post(auth::login))
        .route("/api/logout",   post(auth::logout))
        .route("/api/me",       get (auth::me))
        // --- realtime ---
        .route("/ws", get(ws::ws_upgrade))
        // --- admin / single-player console (gate behind a feature flag or admin check) ---
        .nest("/battle", battle_admin_router())   // keep /battle/* working as today
        .fallback_service(ServeDir::new("static").append_index_html_on_directories(true))
        .with_state(state)
}
```

### Keeping single-player intact
The existing `/battle/*` console manipulates the same one emulator. To avoid a player's match
colliding with someone poking `/battle/setup`:
- Gate `/battle/*` behind an **admin** check (e.g. `ADMIN_TOKEN` env / a `users.is_admin` flag), or
  a build feature `--features console`. The room engine takes a lock (`emu_busy`) while a match is
  live; `/battle/*` handlers return `409 Conflict` if `emu_busy` is set. This lets the original
  console keep working when no match is running, and prevents interference during matches.
- `index.html` (the CRT console) stays reachable at e.g. `/console.html`; the **new default page**
  served at `/` becomes `lobby.html` (or `login.html` when unauthenticated). No existing handler is
  removed — only the index file that `/` resolves to changes.

---

## 9. Slot machine + the 151 sprites

### Server side (authoritative result, cosmetic reel)
The slot machine result is decided **on the server** the instant the room enters `SlotMachine`:
```rust
let p1_species = SPECIES.choose(&mut rng).unwrap().species;
let p2_species = SPECIES.choose(&mut rng).unwrap().species;   // independent; may dup, that's fine
room.p1.species = p1_species; room.p2.species = p2_species;
// persist to rooms.p1_species / p2_species so F5 mid-spin still shows the locked result
```
Then the server runs a short reel animation purely for show: ~2s of `slot_spin{seat, shown_species}`
events (random species each tick, slowing down) ending with `slot_result{you_species, opp_species}`.
A client that joins/refreshes mid-spin just gets the final `slot_result` (idempotent), so there's no
desync. After ~2.5s the server transitions `SlotMachine -> Setup` and fires `setup_tx`.

> The reel can also be **client-only** (server just sends `slot_result`, client animates locally),
> which is simpler and removes the per-tick WS traffic. Recommended for v1: server sends only
> `slot_result`; the client animates the reel for ~2s then displays the locked sprites. The server
> still waits ~2s before `Setup` so the animation has time to play. This is the chosen v1 design.

### The 151 sprite images
- **Source:** the project already vendors **pret/pokered** (`pokered_color/`) and references it in
  `species_data.rs` ("auto-generated from pret/pokered"). Front sprites live in pokered at
  `gfx/pokemon/front/<name>.png` (2bpp `.2bpp` in-repo; the PNG sources are in the pret tree). Use a
  build/asset script (`scripts/extract_sprites.sh`) to copy/convert them to
  `static/sprites/<dex>.png`, 1..151 by **National Dex** number, upscaled (e.g. 4x nearest-neighbor
  to ~224px) for the slot reel. Alternative source: PokéAPI sprites
  (`sprites/pokemon/versions/generation-i/red-blue/<dex>.png`) if pret PNGs aren't directly present.
- **Index mapping gotcha:** the slot machine and battle engine use the **internal index**
  (`s.species`, e.g. Bulbasaur = `0x99` = 153), NOT the dex number. Sprites are named by dex number.
  So the server (or a generated JS table) must map internal index -> dex number. Easiest: emit a
  small table from `species_data.rs` (add a `dex: u16` field, or a parallel `SPECIES_DEX` array) and
  expose `GET /api/species` -> `[{index, dex, name}]`. The client uses `dex` to pick the sprite file
  and `name` for the label; the server uses `index` for `setup_tx`.
  - **Action item:** add `dex` to `Gen1Species` (or a side table) since the current table only has
    the internal index + name. The 151 rows are ordered by dex already in `species_data.rs`
    (Bulbasaur, Ivysaur, ... row N is dex N+1), so `dex = table_position + 1` — derive it cheaply.

### Slot-machine -> Setup wiring
When the reel finishes, the room engine calls:
```rust
setup_tx.send(SetupReq {
    player: room.p1.species, enemy: room.p2.species, level: room.level,
    player_name: p1_username.to_uppercase(), enemy_name: p2_username.to_uppercase(),
    reply,
}); reply.await??;            // engine loads intro state, injects, drives send-out
room.phase = Setup;           // BattleIntro plays on the shared stream
// wait until menu == MainMenu (send-out done) -> room.phase = Battle; begin round loop
```
The names show as the in-game nicknames (capped to 10 chars by `encode_name`).

---

## 10. The room engine task (putting §1,3,5,9,10 together)

One async task drives the active room. Pseudocode (lives in `game.rs`):
```rust
async fn run_room(game: Arc<GameState>, room_id: RoomId) {
    let inner = game.inner.clone();
    game.emu_busy.store(true, Relaxed);

    // 1) SLOT MACHINE: result already rolled by matchmaker handoff; just animate + wait ~2.5s.
    set_phase(&game, room_id, RoomPhase::SlotMachine).await;            // emits room_state
    broadcast_slot_result(&game, room_id).await;                        // emits slot_result
    sleep(Duration::from_millis(2500)).await;

    // 2) SETUP: inject the matchup, drive send-out.
    set_phase(&game, room_id, RoomPhase::Setup).await;
    let (p, e, lvl, pn, en) = room_setup_args(&game, room_id).await;
    let (tx, rx) = oneshot::channel();
    inner.setup_tx.send(SetupReq{ player:p, enemy:e, level:lvl, player_name:pn, enemy_name:en, reply:tx });
    if rx.await.is_err() || /* setup err */ { abort_room(&game, room_id, "setup_failed").await; return; }

    // wait for send-out to finish: menu becomes MainMenu and in_battle != 0
    wait_until(|| { let s = snap(&inner); s.in_battle != 0 && s.menu == MenuPhase::MainMenu }).await;

    // 3) BATTLE: round loop with 15s/side.
    set_phase(&game, room_id, RoomPhase::Battle).await;
    loop {
        let s = snap(&inner);
        if s.in_battle == 0 { break; }                                 // battle ended
        if s.menu != MenuPhase::MainMenu { tick_sleep().await; continue; }

        let last_turns = s.turns_in_battle;
        // collect P1 then P2 (parallel timers, sequential submit). 15s each; CPU on timeout.
        let p1_slot = await_move_or_cpu(&game, room_id, Seat::P1).await;
        inner.action_tx.send(AgentAction::Move{ slot: p1_slot });
        let p2_slot = await_move_or_cpu(&game, room_id, Seat::P2).await;
        inner.enemy_force.store(p2_slot, Relaxed);

        // let the round execute; detect resolution by CCD5 increment or battle end.
        wait_until(|| { let s = snap(&inner);
            s.in_battle == 0 || s.turns_in_battle != last_turns }).await;
        inner.enemy_force.store(0xFF, Relaxed);                        // disarm
        broadcast_battle_state(&game, room_id).await;
    }

    // 4) RESULT: winner = side with HP>0 (deterministic tie rule in §3).
    let winner = decide_winner(&game, room_id).await;
    record_result(&game, room_id, winner).await;                       // DB wins/losses; rooms.winner_seat
    set_phase(&game, room_id, RoomPhase::Result).await;                // emits winner{}
    sleep(Duration::from_secs(5)).await;                               // banner

    // 5) TEARDOWN: free the emulator + send both players Home.
    set_phase(&game, room_id, RoomPhase::Done).await;                  // emits room_closed
    clear_user_room(&game, room_id).await;
    game.emu_busy.store(false, Relaxed);
    *game.active_room.lock().await = None;
    // matchmaker picks up the next pending room on its next tick.
}
```
`await_move_or_cpu` is a `tokio::select!` over (a) the seat's `commit_move` arriving via a per-room
`mpsc` (fed by the WS handler when it validates a `commit_move`) and (b) a 15s `sleep`. On the
timeout branch it computes `random_legal_move(seat_mon, rng)` and emits `move_auto`.

> **Emulator reset between matches:** because there is exactly one emulator, every match begins with
> a fresh `setup_tx` that **load_state(legendary_intro.state)** + injects, so the previous match's
> residue is wiped at `Setup`. No explicit reset needed beyond the existing setup path.

---

## 11. Client pages (static)

- **`login.html`** — register/login form -> `POST /api/login|register`; on success -> `lobby.html`.
- **`lobby.html`** — opens `/ws`; shows username + W/L; a big **Find Match** button (sends
  `find_match`), queue status (`queued`/`queue_size`), and "waiting for arena" while another match
  runs. On `matched` -> navigate to `room.html?id=...`.
- **`room.html`** — the heart of the game:
  - On load: `GET /api/me` to confirm membership, open `/ws`, send `resume`, run the existing
    `POST /offer` flow into a `<video>` (reuse the CRT shell from `index.html`).
  - Renders by `phase` from `room_state`:
    - `slot_machine`: animate the two reels (151 sprites from `/sprites/<dex>.png`), land on
      `slot_result`.
    - `setup`: "FIGHT!" intro; video shows send-out.
    - `battle`: HP bars for you/opp (from `battle_state`), your 4 move buttons (enabled only on
      `your_turn` for your seat), a 15s countdown (from `timer`), opponent "thinking…" indicator.
      `commit_move{slot}` on click; buttons disable after commit.
    - `result`: WIN/LOSE banner + **Return Home** button (navigates to `lobby.html`; server already
      cleared `user_room`).
  - Hard rule: no UI affordance to leave during `slot_machine|setup|battle`; refreshing re-enters
    via `resume`.
- Reuse `index.html` (rename to `console.html`) as the **admin/dev** single-player console.

---

## 12. Build order (incremental, each step compiles & runs)

1. **DB + auth** (`db.rs`, `auth.rs`, migrations) + `login.html`; `/api/me` routing. Existing
   single-player console still works (now at `/console.html`).
2. **WS hub** (`ws.rs`) + `lobby.html`; `hello`/`lobby`/`queued`/`resume`. Find Match enqueues but
   no room yet (log a "matched" stub).
3. **Matchmaker + Room rows** — pairing, `user_room`, `matched`/`room_state`. `room.html` opens the
   shared `/offer` stream (both players see the same screen). Still no battle logic.
4. **Room engine: Setup + Battle** — `setup_tx`, round loop, `commit_move`, `battle_state`,
   `winner`. CPU-on-timeout (15s). This is the playable core.
5. **Slot machine** — server roll + `slot_result`; client reel + 151 sprites; `/api/species` with
   dex mapping; `scripts/extract_sprites.sh`.
6. **Polish** — F5 resume across all phases, server-restart abandonment, W/L stats, admin gate on
   `/battle/*`, multi-tab fan-out, queue-size > 2 "waiting for arena".

---

## 13. Risks / gotchas (call-outs for the implementer)

- **One emulator = one battle.** The whole design funnels all matches through `emu_busy` +
  `active_room`. Do NOT call `setup_tx`/`action_tx`/`enemy_force` from two places at once — only the
  room engine touches them during a match; gate `/battle/*` behind `emu_busy`/admin.
- **`enemy_force` must be disarmed (`0xFF`) after each round**, or P2's move repeats every round.
- **Winner detection on `in_battle==0`** must be gated to `phase==Battle` after a `battle_began`
  latch, because the intro state momentarily reads `in_battle != 0` but `D014==0` and transitions
  are noisy; rely on `menu==MainMenu` to know the battle is truly interactive.
- **Internal-index vs dex-number** for sprites (§9) — the engine speaks internal index; sprites are
  dex-numbered. Provide the mapping table or the slot machine shows the wrong picture.
- **`Move{slot}` taps reset the cursor each turn** (per `action_to_taps` comment), so submitting the
  same slot two turns in a row is safe; no cursor-drift handling needed.
- **PP exhaustion / Struggle:** `random_legal_move` filters on `pp & 0x3f != 0`; if all are 0 the
  engine uses Struggle automatically — submitting slot 0 still works (the macro just opens FIGHT and
  presses A; Struggle is auto-selected by the ROM).
- **Both 0 HP (Selfdestruct/Explosion tie):** keep `last_alive_seat` updated each Battle tick for a
  deterministic winner; documented tie rule = the side that fainted *later* wins, else P1.
- **Reconnect mid-turn:** the timer is server-authoritative; a refresh during your 15s does not
  reset it — `resume` returns the remaining `deadline_ms`. If you don't reconnect in time, CPU plays.
- **axum 0.8 WS:** use `WebSocketUpgrade`; authenticate from the `sid` cookie in the upgrade handler
  (cookies are available on the upgrade GET). Reject unauthenticated upgrades with 401.
- **SQLite concurrency:** use a single `SqlitePool` with WAL mode; the matchmaker/room-engine writes
  are low-volume (room create/finish, stats), so contention is negligible.
```
```
---

## 14. Summary of the answer to each requirement

| # | Requirement | Where solved |
|---|-------------|--------------|
| 1 | register + login | §8 `auth.rs`, `users`/`sessions`, argon2 + cookie |
| 2 | Find Match button | §6 `find_match` intent, `lobby.html` |
| 3 | room when 2 matched | §2 matchmaker + `rooms`/`user_room` rows; §1 Room FSM |
| 4 | both watch SAME stream | §4 both `POST /offer`, one emulator broadcast |
| 5 | slot machine -> random species + 151 sprites | §9 server roll + `slot_result` + `/sprites/<dex>.png` |
| 5b| 15s/move, CPU random on timeout | §5 `await_move_or_cpu` + `random_legal_move` |
| 6 | winner -> Home | §10 Result phase + `winner` event + Return Home |
| 7 | F5 keeps you in room until end | §7 `user_room` + `/api/me` + WS `resume`; no leave intent |
| 8 | new modules + non-breaking integration | §0 file list, §8 router/AppState, §12 build order |
