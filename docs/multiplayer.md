# 2-Player Online Arena

Online multiplayer built on top of the single-emulator battle arena: register/login → **Find
Match** → matched into a **room** → a slot machine rolls a random Pokémon for each player → a
turn-based battle (15 s/move, CPU plays random on timeout) → winner → home. Both players watch the
**same** live Game Boy over WebRTC. F5 keeps you in your room until the battle ends.

> Verified end-to-end in headless Chrome: two users matchmake, get random fighters, battle
> turn-by-turn, and one sees **YOU WIN!** / the other **YOU LOSE**; W/L recorded; F5 mid-battle
> resumes the room; navigating to the lobby bounces back to the room.

Implementation plan + reconciled design: [`../DESIGN-MULTIPLAYER.md`](../DESIGN-MULTIPLAYER.md).

---

## 1. Run it

Uses the **`Pokemon Red.gb`** ROM (the battle savestates are ROM-specific) — it's the **default**,
and gambatte renders it in **color** via GBC auto-colorization, so no extra args are needed:

```sh
cd ~/pokemon-pvp-red
cargo run --release
# verbose:  RUST_LOG=nes_web=info cargo run --release
```

On boot it **auto-creates `data.db`** (SQLite) and runs migrations. Open **http://localhost:3000**.

**To actually play you need two sessions** (each needs its own session cookie):

1. Window 1 (normal): **Register** — username ≥3 chars, **password ≥6 chars** → lands in the Lobby.
2. Window 2 (**incognito**, or a different browser): register a second user.
3. Press **⚔ FIND MATCH** in **both** → they pair → slot machine → battle. Click a move within
   15 s (or the CPU picks one). Winner returns to the Lobby.

Stop the server with **Ctrl-C**. Wipe `data.db` to reset all users.

---

## 2. Architecture

```
 Browser (login/lobby/room .html)
   │  POST /auth/{register,login,logout}      ── argon2id + encrypted session cookie
   │  GET  /api/me   → {user, room?}          ── first-paint routing
   │  WS   /ws       ── lobby/queue/room/battle events (JSON)
   │  POST /offer    ── WebRTC: subscribe to the room's shared video/audio
   ▼
 axum :3000 ── AppState{ api, inner(emulator), db, cookie_key, game(GameState) }
                                   │
   GameState: queue · rooms · user_room · pending · active_room · emu_busy · WsHub
     matchmaker (250 ms): pair 2 queued users → room; run ONE room at a time on the emulator
     run_room: SlotMachine → Setup(setup_tx) → Battle(turn loop) → Result → Done
                                   │ reuses the single-emulator engine VERBATIM:
     P1 move → action_tx (YOU side) · P2 move → enemy_force/CCDD · slot roll → setup_tx
```

- **DB = SeaORM** (`sqlx` backend, `runtime-tokio-rustls`, no OpenSSL). DB-agnostic: SQLite by
  default (`sqlite://./data.db?mode=rwc`, auto-created), **Postgres** by setting `DATABASE_URL` —
  no code change. Tables: `users`, `sessions`, `rooms`, `matches`, `user_room`.
- **Auth** (`src/auth.rs`): argon2id hashing, server-side sessions stored in an encrypted+signed
  `axum-extra` private cookie. `AuthUser` extractor gates `/api/me` and `/ws`. Set `COOKIE_SECRET`
  (≥64 bytes) in prod so sessions survive restarts; otherwise a fresh key is generated each boot.
- **One emulator = one concurrent battle (v1).** The libretro core uses process-global buffers, so
  only one emulator instance exists. Matches funnel through `emu_busy` + `active_room`; extra
  matched pairs wait in a `pending` FIFO. While a match runs, the dev console's `/battle/*` mutators
  return `409`. Scale path: one emulator worker-process per room (`DESIGN-MULTIPLAYER.md` §6).

---

## 3. Room flow & seat → engine mapping

```
Lobby ──find_match──► Queue ──pair 2──► Matched ──emu free──► SlotMachine ──~2.8s──►
Setup ──send-out──► Battle ──in_battle==0──► Result ──5s──► Done ──► Lobby
```

| Room step | Engine call |
|---|---|
| slot roll | `SPECIES.choose(rng)` per seat (internal index) |
| start matchup | `setup_tx.send(SetupReq{ player, enemy, level, player_name=p1.user.upper(), enemy_name=p2.user.upper() })` (loads `states/legendary_intro.state`, injects both parties) |
| **P1** picks move slot | `action_tx.send(Move{slot})` (the YOU side) |
| **P2** picks move slot | `enemy_force.store(slot)` (forces `wEnemySelectedMove`/CCDD), reset to `0xFF` after the round |
| read battle | clone `inner.battle` (the per-frame `BattleState` snapshot) |
| winner | side with `hp>0` when `in_battle==0`; 0/0 → last-alive; tie → P1 |

Each round, `run_round` re-issues the player's move while the FIGHT menu is still up (a first A as
the menu renders is occasionally dropped) and advances result/status text with A so status moves
(Sing, Growl…) don't deadlock the turn counter. Setup retries up to 3× and aborts cleanly if the
battle never reaches the FIGHT menu — the client is sent home rather than stranded.

---

## 4. HTTP + WebSocket API

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/auth/register` · `/auth/login` · `/auth/logout` | `{username,password}` → sets/clears the session cookie |
| `GET` | `/api/me` | `{user:{id,username,wins,losses}, room:{id,phase,seat}\|null}` |
| `GET` | `/api/species` | `[{dex,index,name},…]` — slot-machine sprite (dex) ↔ engine (internal index) map |
| `GET` | `/ws` | authenticated WebSocket (game events) |
| `POST` | `/offer` | WebRTC SDP — subscribe to the room's shared stream (unchanged) |

**WS server→client** (`{"type":…}`): `hello` · `lobby` · `queued{position}` ·
`matched{room_id,seat,opponent}` · `room_state{phase,seat,you,opp,level}` ·
`slot_result{you_species,opp_species,you_dex,opp_dex}` · `your_turn{seat,deadline_ms,moves[]}` ·
`battle_state{in_battle,turn,you,opp,menu}` · `move_auto{seat,slot}` · `winner{seat,you_won}` ·
`room_closed{reason}`.

**WS client→server**: `find_match` · `cancel_queue` · `commit_move{slot}` · `resume` · `ping`.
Every intent is validated server-side against `(user → room → phase → seat)`.

---

## 5. 15-second timer, F5 resume, "can't leave"

- **Timer**: server-authoritative. On `your_turn` the client counts down `deadline_ms` locally; the
  server independently times out at 15 s and submits a **random legal move** (`move_auto`).
- **F5 / refresh**: `GET /api/me` routes first paint (no session → `login.html`; in a room →
  `room.html`; else `lobby.html`). `room.html` opens `/ws`, sends `resume`, and the server re-pushes
  the live phase. The 15 s deadline is **not** reset by a refresh.
- **Can't leave**: while in a room, `lobby.html` bounces you back to the room. A WS disconnect just
  marks you absent — the room keeps running and the CPU plays your turns on timeout. Only at
  **Result** do you get **Return Home**.
- **Server restart mid-battle**: in-process emulator state is lost; `recover_abandoned` (on boot)
  marks live rooms done (winner NULL) and clears `user_room`, so affected users resume to the Lobby.

---

## 6. Files

```
src/db.rs           connect_and_migrate (sqlite auto-create + migrate; DATABASE_URL → postgres)
src/migrations/     users/sessions/rooms/matches/user_room (DB-agnostic DDL)
src/entities/       SeaORM models
src/auth.rs         argon2id, register/login/logout, AuthUser extractor, /api/me
src/rooms.rs        GameState, matchmaker, run_room (slot→setup→turn loop), timer, winner, resume
src/ws.rs           /ws upgrade, WsHub, JSON event protocol
static/login.html   register / login
static/lobby.html   Find Match + queue status
static/room.html    shared <video> + slot machine + dual move panels + 15s timer + result
static/console.html the single-player dev console (renamed from index.html)
static/sprites/     1..151.png — Gen-1 front sprites by National Dex number
```

---

## 7. Troubleshooting

- **"nothing happens" on register/login** → password must be **≥6** characters (username ≥3).
- **Both players see the title screen / "Match couldn't start"** → setup didn't take; you're sent
  back to the Lobby automatically — just **Find Match** again. (`RUST_LOG=nes_web=info` logs
  `setup attempt N failed` / `battle never reached the FIGHT menu`.)
- **Matched but waiting** → another match owns the emulator (v1 = one battle at a time); you start
  when it frees.
- **No sound** → the room video starts muted (autoplay); click **🔊 sound**.
- **Wrong ROM** → run with `Pokemon Red.gb`; the `.gbc` color hack won't load the savestate.
