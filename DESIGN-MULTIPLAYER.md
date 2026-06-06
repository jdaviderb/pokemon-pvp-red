# DESIGN — 2-Player Online Pokémon Arena (v1, buildable)

Single, reconciled, copy-pasteable plan that merges the three probe designs
(`research/mp-db-auth.md`, `research/mp-sprites.md`, `research/mp-architecture.md`) into one
build. It reuses the existing single-emulator engine (`pipeline.rs` + `battle.rs`) **verbatim**,
adds auth + DB + a room/game layer + a per-client WebSocket, and does **not** break `/offer`,
`/battle/*`, or the single-player CRT console.

Flow: register/login → Lobby "Find Match" → matched into a **room** → slot machine rolls a random
species per player → 15s/move turn-based battle (CPU random fallback on timeout) → winner banner →
Home. F5/refresh resumes you into your room until the battle ends.

**Reconciliation decisions (where the two probes disagreed):**

1. **DB tool = SeaORM 1.1 + sea-orm-migration (sqlx backend), NOT raw sqlx.** The db-auth probe
   *compile-verified* the SeaORM stack against this exact `Cargo.lock` under rustc 1.92 with zero
   OpenSSL and zero version perturbation. The architecture probe's "raw sqlx + .sql migrations" is
   the fallback only. Migrations live in `src/migrations/` as Rust (DB-agnostic DDL), not in a
   `migrations/*.sql` dir.
2. **Schema = db-auth's `users`/`sessions` + architecture's `rooms`/`matches`/`user_room`,** all
   expressed as one SeaORM migration with the schema builder so they emit correct SQLite *and*
   Postgres DDL. (`matches` is the finished-game history table; `rooms` is the live row.)
3. **WebSocket = axum 0.8 built-in `axum::extract::ws`** (no `tokio-tungstenite`). axum 0.8 is
   already in the tree; `ws` is on by default. This is the architecture probe's "preferred" path.
4. **Cookies = `axum-extra` PrivateCookieJar** (db-auth probe), not `tower-cookies`. The session
   cookie is encrypted+signed.
5. **Slot reel = client-animated**, server sends only the locked `slot_result` (architecture
   probe's chosen v1). No per-tick `slot_spin` WS traffic.
6. **Sprite mapping = `SPECIES[dex-1].species`** exposed via `GET /api/species`. Sprites are keyed
   by **National Dex** number (`static/sprites/<dex>.png`); the engine speaks **internal index**
   (`.species`). Never send the dex number to setup.

---

## 0. What we reuse vs. what is new

### Reused unchanged
- **`pipeline::AppInner`** — one emulator on one OS thread (libretro uses process-global static
  buffers ⇒ exactly one `AppInner`/process). Already exposes everything the room engine needs:
  - `setup_tx: SetupReq{player, enemy, level, player_name, enemy_name, reply}` — loads
    `states/legendary_intro.state`, injects both party slots, drives send-out, replies `Ok/Err`.
  - `action_tx: AgentAction` — the **YOU** side move (`Move{slot}`) ⇒ **player 1**.
  - `enemy_force: Arc<AtomicU8>` — forces `wEnemySelectedMove` (CCDD); `0xFF` = game AI ⇒ this is
    how **player 2** drives the opponent. `pipeline.rs` only writes CCDD when
    `in_battle != 0 && CCDD != 0`, so arming early is safe.
  - `battle: Arc<Mutex<Option<BattleState>>>` — latest WRAM snapshot, refreshed every frame.
  - `video_tx`/`audio_tx` broadcast channels + `keyframe_req`.
- **`battle::BattleState`** — `in_battle` (D057), `menu` (`MainMenu` = FIGHT menu ready),
  `turns_in_battle` (CCD5), per-side `BattlePokemon{ hp, max_hp, moves[4], pp[4], ... }`.
- **`battle::SPECIES`** — 151 Gen-1 rows in **Pokedex order**; `SPECIES[d-1].species` is the
  internal index for dex `d`. `battle::species_menu()` already returns `[(index, name)]`.
- **`webrtc::build_peer_and_answer`** — one peer per browser `POST /offer`. Both players in a room
  call `/offer` and subscribe to the same broadcast ⇒ both watch the **same** screen. KEEP.

### Existing seat→engine mapping (do not change the channel meanings)
| Existing endpoint | Channel | New role |
|---|---|---|
| `POST /battle/setup` | `setup_tx` | slot-machine result → matchup injection |
| `POST /battle/action` | `action_tx` (`Move{slot}`) | **player 1** move |
| `POST /battle/enemy` | `enemy_force` (CCDD) | **player 2** move |

### New files
```
src/db.rs                         connect_and_migrate(); pool lives in DatabaseConnection
src/entities/mod.rs               SeaORM entity re-exports
src/entities/users.rs             users entity
src/entities/sessions.rs          sessions entity
src/entities/rooms.rs             rooms entity (live room row)
src/entities/matches.rs           matches entity (finished-game history)
src/entities/user_room.rs         user_room entity (1 active room per user; F5 resume)
src/migrations/mod.rs             Migrator
src/migrations/m20260606_000001_init.rs   all 5 tables, DB-agnostic DDL
src/auth.rs                       argon2 hash/verify, register/login/logout/me, AuthUser extractor
src/rooms.rs                      Room FSM types, GameState, Matchmaker, RoomEngine, turn timer
src/ws.rs                         /ws upgrade, WsHub registry, ServerMsg/ClientMsg, resume
src/signaling.rs    (EDIT)        merge routers; AppState gains db, cookie_key, game
src/main.rs         (EDIT)        connect_and_migrate, build GameState, spawn matchmaker
static/login.html                 register/login
static/lobby.html                 Home: Find Match + queue status
static/room.html                  shared <video> + slot machine + dual move panels + 15s timer
static/console.html               (rename of index.html) admin/dev single-player console
static/sprites/1.png .. 151.png   DONE — 151 PNGs by National Dex number (verified, §2)
```

---

## 1. DB + Auth tool (pinned deps, startup flow, migrations, entities, argon2, extractor)

### 1.1 Cargo.toml additions (pinned, verified to resolve+compile under rustc 1.92)

Paste under `[dependencies]`. These are the versions the MSRV resolver actually picked alongside
`webrtc 0.17.1 / axum 0.8.9 / tokio 1.52.3 / rustls 0.23.40 / ring 0.17.14`, with **no OpenSSL**
and **no change to any existing locked version**:

```toml
# --- DB (DB-agnostic: sqlite now, postgres later via DATABASE_URL only) ---
# Resolves to sea-orm 1.1.20 / sea-orm-migration 1.1.20 / sqlx 0.8.6.
# runtime-tokio-rustls => shares rustls 0.23 + ring 0.17 with webrtc (NO OpenSSL).
sea-orm = { version = "1.1", default-features = false, features = [
    "sqlx-sqlite",
    "sqlx-postgres",
    "runtime-tokio-rustls",
    "macros",
    "with-chrono",
] }
sea-orm-migration = { version = "1.1", default-features = false, features = [
    "sqlx-sqlite",
    "sqlx-postgres",
    "runtime-tokio-rustls",
] }

# --- AUTH ---
argon2 = "0.5"        # 0.5.3 — Argon2id password hashing
# argon2 0.5 -> password-hash 0.5 -> SaltString::generate needs rand_core 0.6 (rand 0.8 OsRng).
# rand 0.9.4 stays in the tree for webrtc; rand 0.8.6 is added for the salt. They coexist.
rand = "0.8"          # 0.8.6 — OsRng: CryptoRngCore, slot-machine RNG, CPU random move, tokens
axum-extra = { version = "0.10", features = ["cookie", "cookie-private"] }  # 0.10.3 (matches axum 0.8)
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }
async-trait = "0.1"   # MigrationTrait / FromRequestParts impls in migrations + entities
```

> **Do NOT** use SeaORM's `runtime-tokio-native-tls` (pulls OpenSSL) or bump `axum-extra` to 0.12
> (targets axum 0.9). `libsqlite3-sys 0.30.1` compiles bundled (vendored C, no `brew install`),
> a one-time ~10–20s build hit, no recompile on incremental Rust builds.

### 1.2 Startup: create-if-missing + migrate (`src/db.rs`)

`?mode=rwc` makes sqlx create `data.db` on first run; one branch-free path works for sqlite and
postgres (postgres ignores the flag — the PG database must pre-exist).

```rust
// src/db.rs
use sea_orm::{Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Connect to DATABASE_URL (default = local sqlite, auto-created), then run all pending
/// migrations. Idempotent — safe on every boot.
pub async fn connect_and_migrate() -> Result<DatabaseConnection, DbErr> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://./data.db?mode=rwc".to_string());
    tracing::info!("DB: {}", url.split('@').last().unwrap_or(&url)); // never log creds
    let db = Database::connect(&url).await?;
    crate::migrations::Migrator::up(&db, None).await?; // None = apply ALL pending
    Ok(db)
}
```

Switch to Postgres with **zero code change**:
```bash
export DATABASE_URL='postgres://user:pass@host:5432/nes_web?sslmode=require'
```

### 1.3 Migrations (`src/migrations/`)

`src/migrations/mod.rs`:
```rust
use sea_orm_migration::prelude::*;
mod m20260606_000001_init;
pub struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260606_000001_init::Migration)]
    }
}
```

`src/migrations/m20260606_000001_init.rs` (one migration, all 5 tables; DB-agnostic DDL):
```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // users
        m.create_table(Table::create().table(Users::Table).if_not_exists()
            .col(ColumnDef::new(Users::Id).integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(Users::Username).string().not_null().unique_key())
            .col(ColumnDef::new(Users::PassHash).string().not_null())
            .col(ColumnDef::new(Users::Wins).integer().not_null().default(0))
            .col(ColumnDef::new(Users::Losses).integer().not_null().default(0))
            .col(ColumnDef::new(Users::CreatedAt).timestamp_with_time_zone().not_null())
            .to_owned()).await?;

        // sessions(token PK, user_id FK, expires)
        m.create_table(Table::create().table(Sessions::Table).if_not_exists()
            .col(ColumnDef::new(Sessions::Token).string().not_null().primary_key())
            .col(ColumnDef::new(Sessions::UserId).integer().not_null())
            .col(ColumnDef::new(Sessions::Expires).timestamp_with_time_zone().not_null())
            .foreign_key(ForeignKey::create()
                .from(Sessions::Table, Sessions::UserId).to(Users::Table, Users::Id)
                .on_delete(ForeignKeyAction::Cascade))
            .to_owned()).await?;

        // rooms — the LIVE room row (survives restart only for abandonment cleanup)
        m.create_table(Table::create().table(Rooms::Table).if_not_exists()
            .col(ColumnDef::new(Rooms::Id).integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(Rooms::Phase).string().not_null())           // mirrors RoomPhase
            .col(ColumnDef::new(Rooms::P1User).integer().not_null())
            .col(ColumnDef::new(Rooms::P2User).integer().not_null())
            .col(ColumnDef::new(Rooms::P1Species).integer().null())          // internal index
            .col(ColumnDef::new(Rooms::P2Species).integer().null())
            .col(ColumnDef::new(Rooms::Level).integer().not_null().default(50))
            .col(ColumnDef::new(Rooms::WinnerSeat).integer().null())         // 1 | 2 | NULL
            .col(ColumnDef::new(Rooms::CreatedAt).timestamp_with_time_zone().not_null())
            .col(ColumnDef::new(Rooms::EndedAt).timestamp_with_time_zone().null())
            .to_owned()).await?;

        // matches — finished-game history (read-only audit; rooms get torn down)
        m.create_table(Table::create().table(Matches::Table).if_not_exists()
            .col(ColumnDef::new(Matches::Id).integer().not_null().auto_increment().primary_key())
            .col(ColumnDef::new(Matches::RoomId).integer().not_null())
            .col(ColumnDef::new(Matches::P1User).integer().not_null())
            .col(ColumnDef::new(Matches::P2User).integer().not_null())
            .col(ColumnDef::new(Matches::P1Species).integer().not_null())
            .col(ColumnDef::new(Matches::P2Species).integer().not_null())
            .col(ColumnDef::new(Matches::WinnerSeat).integer().null())       // NULL = abandoned/tie
            .col(ColumnDef::new(Matches::EndedAt).timestamp_with_time_zone().not_null())
            .to_owned()).await?;

        // user_room — 1 active room per user (F5 resume)
        m.create_table(Table::create().table(UserRoom::Table).if_not_exists()
            .col(ColumnDef::new(UserRoom::UserId).integer().not_null().primary_key())
            .col(ColumnDef::new(UserRoom::RoomId).integer().not_null())
            .to_owned()).await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for t in [UserRoom::Table.into_iden(), Matches::Table.into_iden(),
                  Rooms::Table.into_iden(), Sessions::Table.into_iden(), Users::Table.into_iden()] {
            m.drop_table(Table::drop().table(t).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)] enum Users { Table, Id, Username, PassHash, Wins, Losses, CreatedAt }
#[derive(DeriveIden)] enum Sessions { Table, Token, UserId, Expires }
#[derive(DeriveIden)] enum Rooms {
    Table, Id, Phase, P1User, P2User, P1Species, P2Species, Level, WinnerSeat, CreatedAt, EndedAt }
#[derive(DeriveIden)] enum Matches {
    Table, Id, RoomId, P1User, P2User, P1Species, P2Species, WinnerSeat, EndedAt }
#[derive(DeriveIden)] enum UserRoom { Table, UserId, RoomId }
```

> **Sessions, not JWT.** Server-side sessions let us store/look-up the player's `room_id` for F5
> resume and forcibly invalidate (logout, battle-end). The room layer already needs server state.

### 1.4 Entities (`src/entities/`)

`mod.rs`:
```rust
pub mod users; pub mod sessions; pub mod rooms; pub mod matches; pub mod user_room;
```

`users.rs`:
```rust
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)] pub id: i32,
    #[sea_orm(unique)] pub username: String,
    pub pass_hash: String,
    pub wins: i32,
    pub losses: i32,
    pub created_at: ChronoDateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
```

`sessions.rs`:
```rust
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)] pub token: String,
    pub user_id: i32,
    pub expires: ChronoDateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
```

`rooms.rs` / `matches.rs` / `user_room.rs`: same `DeriveEntityModel` shape, columns mirroring the
migration; `Option<i32>` for the nullable `*_species`/`winner_seat`, `Option<ChronoDateTimeUtc>`
for `ended_at`. (Empty `Relation` enums are fine — we query by id and don't need FK joins.)

### 1.5 Auth (`src/auth.rs`) — argon2 + handlers + extractor

```rust
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::SaltString;
use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::{request::Parts, StatusCode};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar, SameSite};
use chrono::{Duration, Utc};
use rand::rngs::OsRng;                 // rand 0.8 OsRng: CryptoRngCore (rand_core 0.6)
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use crate::entities::{users, sessions};
use crate::signaling::AppState;

const SESSION_COOKIE: &str = "nes_session";
const SESSION_TTL_HOURS: i64 = 12;

pub fn hash_password(plain: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default().hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?.to_string())
}
pub fn verify_password(plain: &str, phc: &str) -> bool {
    PasswordHash::new(phc).map(|p| Argon2::default()
        .verify_password(plain.as_bytes(), &p).is_ok()).unwrap_or(false)
}
fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32]; OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(serde::Deserialize)]
pub struct Credentials { pub username: String, pub password: String }

pub async fn register(State(st): State<AppState>, jar: PrivateCookieJar, Json(c): Json<Credentials>)
    -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    if c.username.len() < 3 || c.password.len() < 6 {
        return Err((StatusCode::BAD_REQUEST, "username>=3, password>=6".into()));
    }
    let pass_hash = hash_password(&c.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let user = users::ActiveModel {
        username: Set(c.username), pass_hash: Set(pass_hash),
        wins: Set(0), losses: Set(0), created_at: Set(Utc::now()), ..Default::default()
    }.insert(&st.db).await.map_err(|_| (StatusCode::CONFLICT, "username taken".into()))?;
    Ok((start_session(&st, jar, user.id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?, StatusCode::CREATED))
}

pub async fn login(State(st): State<AppState>, jar: PrivateCookieJar, Json(c): Json<Credentials>)
    -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    let user = users::Entity::find().filter(users::Column::Username.eq(&c.username))
        .one(&st.db).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::UNAUTHORIZED, "bad credentials".into()))?;
    if !verify_password(&c.password, &user.pass_hash) {
        return Err((StatusCode::UNAUTHORIZED, "bad credentials".into()));
    }
    Ok((start_session(&st, jar, user.id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?, StatusCode::OK))
}

async fn start_session(st: &AppState, jar: PrivateCookieJar, user_id: i32)
    -> anyhow::Result<PrivateCookieJar> {
    let token = new_token();
    sessions::ActiveModel {
        token: Set(token.clone()), user_id: Set(user_id),
        expires: Set(Utc::now() + Duration::hours(SESSION_TTL_HOURS)), ..Default::default()
    }.insert(&st.db).await?;
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .http_only(true).same_site(SameSite::Lax).secure(false).path("/")
        .max_age(time::Duration::hours(SESSION_TTL_HOURS)).build();
    Ok(jar.add(cookie))
}

pub async fn logout(State(st): State<AppState>, jar: PrivateCookieJar) -> PrivateCookieJar {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        let _ = sessions::Entity::delete_by_id(c.value().to_string()).exec(&st.db).await;
    }
    jar.remove(Cookie::from(SESSION_COOKIE))
}

/// Present in a handler signature ⇒ request is authenticated. 401 otherwise.
pub struct AuthUser(pub users::Model);
impl<S> FromRequestParts<S> for AuthUser
where AppState: FromRef<S>, S: Send + Sync {
    type Rejection = (StatusCode, &'static str);
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let st = AppState::from_ref(state);
        let jar = PrivateCookieJar::<Key>::from_request_parts(parts, state).await
            .map_err(|_| (StatusCode::UNAUTHORIZED, "no cookie"))?;
        let token = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned())
            .ok_or((StatusCode::UNAUTHORIZED, "no session"))?;
        let sess = sessions::Entity::find_by_id(token).one(&st.db).await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "session not found"))?;
        if sess.expires < Utc::now() { return Err((StatusCode::UNAUTHORIZED, "expired")); }
        let user = users::Entity::find_by_id(sess.user_id).one(&st.db).await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "user gone"))?;
        Ok(AuthUser(user))
    }
}

/// GET /api/me -> {user, room?} for first-paint routing (login? lobby? room?).
#[derive(serde::Serialize)]
pub struct MeRoom { pub id: i32, pub phase: String, pub seat: u8 }
pub async fn me(State(st): State<AppState>, AuthUser(u): AuthUser) -> Json<serde_json::Value> {
    let room = crate::rooms::current_room_for(&st, u.id).await; // Option<MeRoom>
    Json(serde_json::json!({
        "user": {"id": u.id, "username": u.username, "wins": u.wins, "losses": u.losses},
        "room": room,
    }))
}
```

---

## 2. Sprites — CONFIRMED

- **`static/sprites/` contains exactly 151 PNGs**, named `1.png .. 151.png` by **National Dex
  number** (verified: `ls static/sprites | wc -l == 151`; files `1.png`,`6.png`,`25.png`,
  `150.png`,`151.png` all present). 96×96 transparent canvas, pixel-art (render with
  `image-rendering: pixelated`).
- **Mapping the UI + server use:** sprites are dex-keyed; the engine's `setup_tx` wants the
  **internal Gen-1 index**. The `SPECIES` table (`src/species_data.rs`) is **in Pokedex order**
  (row 0 = Bulbasaur = dex 1, … row 150 = Mew = dex 151), and each row's `.species` is the internal
  byte. So:
  ```
  internal_index = SPECIES[dex - 1].species        // e.g. dex 1 -> 0x99, dex 25 -> 0x54, dex 150 -> 0x83
  ```
  Verified against the sprites probe's `DEX_TO_INTERNAL` table (identical values).
- **Server exposes the mapping** so the slot machine never hard-codes it. Add to `signaling.rs`:
  ```rust
  // GET /api/species -> [{dex, index, name}]  (dex = position+1; index = .species)
  async fn species_list_handler() -> Json<Vec<serde_json::Value>> {
      Json(crate::battle::SPECIES.iter().enumerate().map(|(i, s)| serde_json::json!({
          "dex": i + 1, "index": s.species, "name": s.name })).collect())
  }
  ```
  Client flow: slot machine picks dex `d ∈ 1..=151` → shows `/sprites/<d>.png` → server (already
  authoritative) sends `slot_result.you_species` = the **internal index** it rolled; the client
  reverse-maps index→dex via `/api/species` to pick the sprite. The server rolls on `SPECIES`
  directly (`SPECIES.choose(rng).species`), so it always has the internal index for `setup_tx`.

---

## 3. Room state machine, matchmaking, WS protocol, F5, 15s timer, seat mapping, winner→Home

### 3.1 Room FSM (`src/rooms.rs`)
```
   Lobby ──find_match──► Queue ──pair 2──► Matched ──emu free──► SlotMachine ──~2.5s──►
   Setup ──send-out done──► Battle ──in_battle==0──► Result ──5s banner──► Done ──► Home
```
```rust
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomPhase { Matched, SlotMachine, Setup, Battle, Result, Done }

#[derive(Clone, Copy, PartialEq, Eq)] pub enum Seat { P1, P2 }

pub type RoomId = i32; pub type UserId = i32;

pub struct PlayerSeat {
    pub user_id: UserId,
    pub username: String,
    pub species: u8,                  // internal index from the slot machine
    pub committed_move: Option<u8>,   // slot 0..3 chosen this round (None = waiting)
    pub connected: bool,              // has a live WS now (F5 toggles this)
    pub move_tx: Option<tokio::sync::mpsc::UnboundedSender<u8>>, // engine wakeup on commit
}

pub struct Room {
    pub id: RoomId,
    pub phase: RoomPhase,
    pub p1: PlayerSeat,               // YOU side  -> action_tx
    pub p2: PlayerSeat,               // ENEMY side-> enemy_force
    pub level: u8,                    // default 50
    pub turn_deadline: Option<std::time::Instant>,
    pub last_alive_seat: Seat,        // for selfdestruct/0-0 tie rule
    pub winner: Option<Seat>,
}
```

### 3.2 GameState + matchmaker (single active room v1)
```rust
use std::sync::atomic::AtomicBool;
use std::collections::{HashMap, VecDeque};
use tokio::sync::Mutex;

pub struct GameState {
    pub inner: std::sync::Arc<crate::pipeline::AppInner>, // the one emulator
    pub db: sea_orm::DatabaseConnection,
    pub queue: Mutex<VecDeque<UserId>>,                   // FIFO of Find-Match presses
    pub rooms: Mutex<HashMap<RoomId, Room>>,
    pub user_room: Mutex<HashMap<UserId, RoomId>>,        // hot cache of user_room table
    pub pending: Mutex<VecDeque<RoomId>>,                 // matched rooms awaiting the emulator
    pub active_room: Mutex<Option<RoomId>>,               // v1: ONE room uses the emulator
    pub emu_busy: AtomicBool,                             // gates /battle/* during a match
    pub ws: crate::ws::WsHub,
}
```
**Matchmaker task** (one `tokio::spawn`, ~250 ms tick):
1. Lock `queue`; while `len >= 2` pop two distinct users `(a, b)`:
   - insert `rooms` row (phase `Matched`, p1=a, p2=b, level=50, species NULL); insert two
     `user_room` rows; update caches; build the in-memory `Room`; push id onto `pending`.
   - WS `matched{room_id, seat, opponent}` to both.
2. **Single-emulator gate:** if `active_room.is_none()` and `pending` non-empty, pop the next
   pending room, set `active_room`, spawn `run_room(game, room_id)` (→ `SlotMachine`). All other
   pending rooms stay `Matched` ("waiting for an open arena…").

### 3.3 WebSocket event protocol (`src/ws.rs`)

One WS per logged-in client, auth'd from the `nes_session` cookie on the upgrade GET. WebRTC stays
separate (media only).

**Server→Client `ServerMsg`** (`#[serde(tag="type", rename_all="snake_case")]`):
```jsonc
{ "type":"hello",       "user":{"id":7,"username":"ash","wins":3,"losses":1} }
{ "type":"lobby",       "queued":false, "queue_size":0 }
{ "type":"queued",      "position":2 }
{ "type":"matched",     "room_id":42, "seat":1, "opponent":"misty" }
{ "type":"room_state",  "room_id":42, "phase":"battle", "seat":1, "level":50,
                        "you":{"username":"ash","species":84},"opp":{"username":"misty","species":6} }
{ "type":"slot_result", "you_species":84, "opp_species":6, "you_dex":94, "opp_dex":6 }
{ "type":"your_turn",   "seat":1, "deadline_ms":15000,
                        "moves":[{"slot":0,"id":85,"name":"THUNDERBOLT","pp":15}, ...] }
{ "type":"timer",       "seat":1, "seconds_left":9 }
{ "type":"move_auto",   "seat":2, "slot":1 }            // CPU picked for a seat on timeout
{ "type":"battle_state","in_battle":2, "turn":7,
                        "you":{"species":84,"hp":120,"max_hp":160,"status":0},
                        "opp":{"species":6,"hp":40,"max_hp":150,"status":0}, "menu":"main_menu" }
{ "type":"winner",      "seat":1, "you_won":true }      // client returns Home after banner
{ "type":"room_closed", "reason":"battle_ended" }
{ "type":"error",       "code":"not_your_turn", "message":"..." }
```
**Client→Server `ClientMsg`** (same tagging):
```jsonc
{ "type":"find_match" }                 // valid only when Home
{ "type":"cancel_queue" }               // only while queued, not yet roomed
{ "type":"commit_move", "slot":2 }      // only when phase==Battle && it's your seat's turn
{ "type":"resume" }                     // "where am I?" -> lobby | room_state (+ live phase msgs)
{ "type":"ping" }                       // keepalive
```
Every intent is validated server-side against `(user → room → phase → seat)`; illegal intents get
`error` and are dropped. The UI disables buttons too, but the server is the source of truth.

**WsHub:**
```rust
pub struct WsHub {
    conns: tokio::sync::Mutex<HashMap<UserId, Vec<tokio::sync::mpsc::UnboundedSender<ServerMsg>>>>,
}
// send_to(user_id, msg) fans out to all that user's tabs; prune dead senders on send error.
// On (re)connect, immediately push the user's current lobby/room state (resume).
```

### 3.4 Seat → engine mapping (the heart)
| Room concept | Engine call |
|---|---|
| Start matchup | `setup_tx.send(SetupReq{ player:p1.species, enemy:p2.species, level, player_name:p1.username.to_uppercase(), enemy_name:p2.username.to_uppercase(), reply })` then await reply |
| **P1** commits slot `s` | `action_tx.send(AgentAction::Move{slot:s})` (YOU side) → `/battle/action` |
| **P2** commits slot `s` | `enemy_force.store(s, Relaxed)` (forces CCDD) → `/battle/enemy`; reset `0xFF` after the round |
| Read battle | clone `*inner.battle.lock().unwrap()` |
| Battle over? | `state.in_battle == 0` (only once `phase==Battle` after `MainMenu` seen) |
| Winner | side whose `hp > 0`; on 0/0 use `last_alive_seat`, tie ⇒ P1 |
| Slot-machine → Setup | `setup_tx` as above (reuses `states/legendary_intro.state`) |
| Winner → Home | `Result` phase → WS `winner{}` → 5s → `Done` → clear `user_room` → client navigates to `lobby.html` |

### 3.5 15s server-authoritative turn timer + CPU-random fallback
A move is needed when `state.in_battle != 0 && state.menu == MenuPhase::MainMenu`. Gen-1 resolves
both sides in one round (`turns_in_battle`/CCD5 +1 per round). Round loop in `run_room`:
```rust
async fn run_room(game: Arc<GameState>, room_id: RoomId) {
    let inner = game.inner.clone();
    game.emu_busy.store(true, Ordering::Relaxed);

    set_phase(&game, room_id, RoomPhase::SlotMachine).await;     // room_state
    broadcast_slot_result(&game, room_id).await;                 // slot_result (server already rolled)
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

    set_phase(&game, room_id, RoomPhase::Setup).await;
    let (p, e, lvl, pn, en) = setup_args(&game, room_id).await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = inner.setup_tx.send(crate::pipeline::SetupReq {
        player: p, enemy: e, level: lvl, player_name: pn, enemy_name: en, reply: tx });
    if !matches!(rx.await, Ok(Ok(()))) { abort_room(&game, room_id, "setup_failed").await; return; }

    // send-out done = MainMenu + in_battle != 0
    wait_until(&inner, |s| s.in_battle != 0 && s.menu == MenuPhase::MainMenu).await;

    set_phase(&game, room_id, RoomPhase::Battle).await;
    loop {
        let s = snap(&inner);
        if s.in_battle == 0 { break; }
        if s.menu != MenuPhase::MainMenu { tick().await; continue; }
        let last_turns = s.turns_in_battle;
        update_last_alive(&game, room_id, &s).await;

        // P1 then P2; 15s each (UI timers can overlap); CPU random on timeout.
        let p1 = await_move_or_cpu(&game, room_id, Seat::P1, &s.player).await;
        let _ = inner.action_tx.send(AgentAction::Move { slot: p1 });
        let p2 = await_move_or_cpu(&game, room_id, Seat::P2, &s.enemy).await;
        inner.enemy_force.store(p2, Ordering::Relaxed);          // arm enemy for this round

        wait_until(&inner, move |s| s.in_battle == 0 || s.turns_in_battle != last_turns).await;
        inner.enemy_force.store(0xFF, Ordering::Relaxed);        // DISARM — else P2 repeats
        clear_committed(&game, room_id).await;
        broadcast_battle_state(&game, room_id).await;
    }

    let winner = decide_winner(&game, room_id).await;            // hp>0; 0/0 -> last_alive; tie -> P1
    record_result(&game, room_id, winner).await;                // users.wins/losses; rooms+matches rows
    set_phase(&game, room_id, RoomPhase::Result).await;          // winner{}
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    set_phase(&game, room_id, RoomPhase::Done).await;            // room_closed{}
    clear_user_room(&game, room_id).await;
    game.emu_busy.store(false, Ordering::Relaxed);
    *game.active_room.lock().await = None;                       // matchmaker picks next pending
}

/// 15s deadline OR a real commit_move (whichever first). CPU random legal move on timeout.
async fn await_move_or_cpu(game: &Arc<GameState>, room_id: RoomId, seat: Seat,
                           mon: &crate::battle::BattlePokemon) -> u8 {
    let mut rx = arm_turn(game, room_id, seat).await;            // your_turn{} + per-round move_tx + start timer task
    match tokio::time::timeout(std::time::Duration::from_secs(15), rx.recv()).await {
        Ok(Some(slot)) => slot,
        _ => { let slot = random_legal_move(mon, &mut rand::thread_rng());
               broadcast_move_auto(game, room_id, seat, slot).await; slot }
    }
}

fn random_legal_move(mon: &crate::battle::BattlePokemon, rng: &mut impl rand::Rng) -> u8 {
    use rand::seq::SliceRandom;
    let legal: Vec<u8> = (0..4)
        .filter(|&i| mon.moves[i] != 0 && (mon.pp[i] & 0x3f) != 0)
        .map(|i| i as u8).collect();
    *legal.choose(rng).unwrap_or(&0)   // slot 0 always exists; Struggle covers all-zero-PP
}
```
- `arm_turn` sets `room.turn_deadline = now + 15s`, emits `your_turn{seat, deadline_ms:15000,
  moves}` from the snapshot, creates the per-round `move_tx`/`rx` (stored on the seat so the WS
  `commit_move` handler can forward the slot), and spawns a 1 Hz `timer{seat, seconds_left}` task.
- The WS `commit_move` handler validates `(your room, phase==Battle, your seat armed)` and sends
  `slot` over the seat's `move_tx`, short-circuiting the wait.
- **`enemy_force` MUST be reset to `0xFF` after each round** or P2's move repeats forever.

### 3.6 Winner detection (authoritative)
Only evaluate once `phase==Battle` and `MainMenu` has been seen at least once (guards the noisy
intro where `in_battle != 0` but `D014==0`). On `in_battle==0`:
```rust
let s = snap(&inner);
let winner = if s.player.hp > 0 && s.enemy.hp == 0 { Seat::P1 }
             else if s.enemy.hp > 0 && s.player.hp == 0 { Seat::P2 }
             else { room.last_alive_seat };   // 0/0 selfdestruct; tie default P1
```
Keep `last_alive_seat` updated each Battle tick.

### 3.7 F5 / resume (cannot leave until battle ends)
- `user → room` is tracked in BOTH the DB (`user_room`, `rooms`) and the in-memory cache.
- First paint: `GET /api/me` → `{user, room?}`; static pages route: no session → `login.html`,
  session+no room → `lobby.html`, session+room → `room.html?id=N`.
- `room.html` opens `/ws`, sends `{"type":"resume"}` → hub replies with current `room_state` +
  the live-phase message (`slot_result` / `your_turn` with remaining `deadline_ms` / `winner`),
  and re-`POST /offer` for the shared video.
- **No leave intent** while `phase ∈ {SlotMachine, Setup, Battle}`; `find_match`/`cancel_queue`
  rejected with `error{code:"in_room"}`. A WS disconnect sets `connected=false` only — the room and
  the 15s timer keep running; if a disconnected player's turn times out, CPU plays for them.
- **Only at `Result`** does the client get **Return Home** (server already cleared `user_room`).
- **Server restart mid-battle:** emulator state is in-process and lost. On boot, mark every `rooms`
  row with `ended_at IS NULL` abandoned (`phase=Done`, `winner_seat=NULL`), write a `matches` row
  with `winner_seat=NULL`, clear `user_room`, resume affected users to Lobby.

---

## 4. New modules + exact AppState/axum wiring (non-breaking)

### 4.1 AppState (`signaling.rs`)
```rust
use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sea_orm::DatabaseConnection;

#[derive(Clone)]
pub struct AppState {
    pub api: std::sync::Arc<::webrtc::api::API>,      // unchanged (/offer)
    pub inner: std::sync::Arc<crate::pipeline::AppInner>, // unchanged (/offer, /battle/*)
    pub db: DatabaseConnection,                        // NEW (cheap clone; Arc'd pool)
    pub cookie_key: Key,                               // NEW (private session cookie)
    pub game: std::sync::Arc<crate::rooms::GameState>, // NEW (queue, rooms, ws hub)
}
impl FromRef<AppState> for Key { fn from_ref(s: &AppState) -> Self { s.cookie_key.clone() } }
```

### 4.2 Router (extends existing — every current route preserved)
```rust
pub fn router(state: AppState) -> Router {
    let static_service = ServeDir::new("static").append_index_html_on_directories(true);
    Router::new()
        // --- media (UNCHANGED) ---
        .route("/offer", post(offer_handler))
        // --- single-player battle console (UNCHANGED; gated by emu_busy at handler level) ---
        .route("/battle/state",  get(battle_state_handler))
        .route("/battle/action", post(battle_action_handler))
        .route("/battle/save",   post(battle_save_handler))
        .route("/battle/load",   post(battle_load_handler))
        .route("/battle/setup",  post(battle_setup_handler))
        .route("/battle/species",get(battle_species_handler))
        .route("/battle/enemy",  post(battle_enemy_handler))
        // --- NEW: auth ---
        .route("/auth/register", post(crate::auth::register))
        .route("/auth/login",    post(crate::auth::login))
        .route("/auth/logout",   post(crate::auth::logout))
        .route("/api/me",        get(crate::auth::me))
        .route("/api/species",   get(species_list_handler))
        // --- NEW: realtime ---
        .route("/ws", get(crate::ws::ws_upgrade))
        .fallback_service(static_service)
        .with_state(state)
}
```
**Keeping single-player intact:** the `/battle/*` handlers gain a guard returning `409 Conflict`
when `state.game.emu_busy` is set, so the dev console can't collide with a live match (and a match
can't be disturbed by `/battle/setup`). The console moves to `static/console.html`; `/` now serves
`lobby.html` (or `login.html` when unauthenticated) — only the index *file* changes, no handler is
removed.

### 4.3 main.rs wiring
```rust
mod auth; mod db; mod entities; mod migrations; mod rooms; mod ws;   // + existing mods

let inner = pipeline::start(core_path, rom_path);
let api = crate::webrtc::build_api()?;
let database = db::connect_and_migrate().await?;
rooms::recover_abandoned(&database).await?;          // restart cleanup (§3.7)
let cookie_key = match std::env::var("COOKIE_SECRET") {
    Ok(b64) => axum_extra::extract::cookie::Key::from(&base64_decode(&b64)),
    Err(_)  => axum_extra::extract::cookie::Key::generate(),   // dev only
};
let game = std::sync::Arc::new(rooms::GameState::new(inner.clone(), database.clone(), /*ws hub*/));
rooms::spawn_matchmaker(game.clone());               // 250ms tick; single-emulator gate
let state = AppState { api, inner, db: database, cookie_key, game };
let app = router(state);
```

### 4.4 New static pages
- **`login.html`** — register/login form → `POST /auth/login|register`; success → `lobby.html`.
- **`lobby.html`** — opens `/ws`; shows username + W/L; big **Find Match** button (`find_match`);
  queue status (`queued`/`queue_size`); "waiting for an open arena" while another match runs;
  on `matched` → `room.html?id=…`.
- **`room.html`** — the game screen; reuse the CRT shell from `console.html`:
  - on load: `GET /api/me` to confirm membership; open `/ws`; send `resume`; run the existing
    `POST /offer` flow into `<video>`.
  - render by `phase`:
    - `slot_machine`: animate two reels using `/sprites/<dex>.png` (151 images), land on the
      `slot_result` (reverse-map index→dex via `/api/species`).
    - `setup`: "FIGHT!" intro (video shows send-out).
    - `battle`: HP bars (you/opp from `battle_state`); **dual move panels** — your 4 buttons
      enabled only on `your_turn` for your seat, opponent panel shows "thinking…"; 15s **turn
      timer** from `timer`; `commit_move{slot}` on click; buttons disable after commit.
    - `result`: WIN/LOSE banner + **Return Home** → `lobby.html`.
  - **No leave affordance** during `slot_machine|setup|battle`; F5 re-enters via `resume`.
- **`console.html`** — the renamed `index.html`, the admin/dev single-player console (unchanged).

---

## 5. ORDERED build order (each step independently compilable/testable)

See `build_order` in the structured output. Each step ends with `cargo build` green and a manual
smoke test; the existing single-player console keeps working throughout (it just moves to
`/console.html` after step 1).

---

## 6. Risks / gotchas

- **One emulator = one battle (multi-room concurrency limit).** All matches funnel through
  `emu_busy` + `active_room`; extra matched rooms wait in `pending`. Never touch
  `setup_tx`/`action_tx`/`enemy_force` from two places — only `run_room` does during a match, and
  `/battle/*` returns 409 while `emu_busy`. v2 = one emulator worker process per room (the
  `active_room` gate becomes "assign to a free worker"; `/offer` becomes `/room/{id}/offer`,
  protocol unchanged).
- **WebRTC for 2 viewers on one room.** v1 has one emulator ⇒ one broadcast pair ⇒ both players see
  the *same* pixels (including P1's FIGHT-cursor macro). Acceptable — it's literally one Game Boy,
  and move *selection* is via WS intents + on-screen buttons (no input-timing advantage).
  `keyframe_req` fires on each new peer so the second joiner gets a clean keyframe. The broadcast
  channels are bounded (`video=16`, `audio=64`) — a slow viewer Lags but recovers (already handled
  in `webrtc.rs`), it does not stall the other.
- **Timer race conditions.** Deadline is a server `Instant`; `timer{}` ticks are cosmetic. Use a
  single `tokio::select!`/`timeout` over (real commit via the seat `move_tx`) vs (15s sleep) so a
  late commit can't double-submit after the CPU fired. Reset `enemy_force=0xFF` only *after* the
  round resolves (CCD5 increment), never mid-round. The per-round `move_tx` is recreated each round
  so a stale `commit_move` from the previous round is dropped (channel closed → `error`).
- **F5 mid-battle.** Refresh does NOT reset the 15s deadline (`resume` returns remaining
  `deadline_ms`); WS close only flips `connected=false`; the room keeps running and CPU plays on
  timeout. Server *restart* mid-battle loses emulator state ⇒ `recover_abandoned` marks the room
  Done/NULL-winner and resumes users to Lobby (v2: per-room savestate via `save_tx`/`load_tx`).
- **`enemy_force` disarm** — forgetting `0xFF` after each round makes P2 repeat its move every turn.
- **Internal-index vs dex-number** — engine speaks internal index, sprites are dex-numbered; always
  go through `/api/species` / `SPECIES[d-1].species`, never send dex to `setup_tx`.
- **Selfdestruct/0-0 tie** — keep `last_alive_seat` per Battle tick; documented tie default = P1.
- **SQLite concurrency** — single pool; writes (room create/finish, stats) are low-volume; enable
  WAL via DSN if contention ever appears.
- **rand two-version pitfall** — `auth.rs` must use rand 0.8's `OsRng` for `SaltString::generate`
  (rand 0.9's `OsRng` won't satisfy `CryptoRngCore`). Pinning `rand = "0.8"` makes unqualified
  `rand::` resolve to 0.8; rand 0.9.4 stays only for webrtc.
```
