# DB + Auth tooling for the 2-player online arena

**Verified** on this machine, 2026-06-06, against the project's pinned toolchain.

- `rustc 1.92.0 (ded5c06cf 2025-12-08)`, `cargo 1.92.0`
- Existing tree already locks: `webrtc 0.17.1`, `axum 0.8.9`, `tokio 1.52.3`,
  `tower-http 0.6.11`, **`rustls 0.23.40`**, **`ring 0.17.14`**, `uuid 1.23.2`,
  `getrandom 0.2.17` + `0.3.4` + `0.4.2`, `rand 0.9.4`, `time 0.3.47`, `zeroize 1.8.2`.
  **No OpenSSL, no native-tls, no aws-lc anywhere in the tree.**

Verification method: copied `Cargo.toml` + `Cargo.lock` + `rust-toolchain.toml` into
`/tmp/nes-db-resolve`, added the candidate stack, ran `cargo generate-lockfile` (full
MSRV-resolver solve) and `cargo build -p sea-orm -p sea-orm-migration -p argon2
-p axum-extra -p sqlx -p libsqlite3-sys`. **Result: full solve of 407 packages, all
existing versions preserved byte-for-byte, and the entire DB/auth stack compiled cleanly
in ~16s under rustc 1.92.** API signatures below were read from the resolved crate sources.

---

## Recommendation: SeaORM + sea-orm-migration (DB-agnostic, sqlx backend)

**Use SeaORM 1.1 with the `runtime-tokio-rustls` runtime and BOTH `sqlx-sqlite` +
`sqlx-postgres` backends compiled in.** Justification:

1. **DB-agnostic by design.** Entities (`DeriveEntityModel`) and migrations
   (`MigrationTrait` + `sea_query` schema builder) are written once and emit correct DDL
   for SQLite or Postgres. Switching is a one-line `DATABASE_URL` change — no entity or
   migration edits. This is exactly the "easy to swap SQLite -> Postgres later" requirement.
2. **Runs migrations on startup** via `Migrator::up(&db, None)` — no external CLI needed in
   prod (the `cli` feature is optional and we leave it off).
3. **Shares the existing TLS stack.** With `runtime-tokio-rustls`, SeaORM/sqlx use
   **rustls 0.23.40 + ring 0.17.14 — the exact versions webrtc 0.17 already locked.**
   Verified: the solved lockfile contains **zero** `openssl`, `openssl-sys`, `native-tls`,
   or `aws-lc-*` crates. No new system C/C++ TLS dependency, no OpenSSL build risk.
4. **SQLite needs no system lib.** `libsqlite3-sys 0.30.1` is pulled with the `bundled`
   feature (SeaORM's `sqlx-sqlite` enables it), so SQLite is compiled from vendored C — no
   `brew install sqlite` needed. (Adds a one-time C compile, see "Compile cost" below.)

**Why not raw sqlx with the `Any` driver (the stated alternative)?**
`sqlx::Any` *is* runtime-swappable, but it erases column types and forces you to write SQL
strings that work on both dialects yourself (e.g. `TEXT`/`VARCHAR`, autoincrement syntax,
`?` vs `$1` placeholders). For a schema this small it's tempting, but SeaORM's typed
entities + the `sea_query` schema builder give you compile-checked queries and
dialect-correct DDL for free, which is worth the modest extra compile. **If you wanted the
absolute minimum dep footprint**, plain `sqlx 0.8` with `runtime-tokio-rustls,sqlite,postgres,migrate`
and `.sql` files in a `migrations/` dir + `sqlx::migrate!()` also resolves and compiles
fine under 1.92 — but you lose dialect-agnostic DDL. Recommendation stands: **SeaORM**.

---

## (deps) EXACT, version-pinned Cargo.toml additions

Paste under `[dependencies]`. These are the versions the resolver actually picked and that
compiled under rustc 1.92 alongside webrtc/axum/tokio:

```toml
# --- DB (DB-agnostic; sqlite now, postgres later via DATABASE_URL only) ---
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
argon2 = "0.5"                       # 0.5.3; password hashing (Argon2id default)
# NOTE: argon2 0.5 -> password-hash 0.5 -> SaltString::generate(impl CryptoRngCore) needs
# the rand_core 0.6 trait. rand 0.9's OsRng impls rand_core 0.9 and will NOT satisfy it.
# Pin rand 0.8 specifically for salt generation. (rand 0.9.4 also stays in the tree for
# webrtc; the two coexist — verified in the lockfile.)
rand = "0.8"                         # 0.8.6; provides OsRng : CryptoRngCore (rand_core 0.6)

# --- cookies / session extractor for axum 0.8 ---
axum-extra = { version = "0.10", features = ["cookie", "cookie-private"] }  # 0.10.3
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }
```

> Do NOT bump `axum-extra` to 0.12 — it targets axum 0.9. **0.10 is the line that matches
> axum 0.8**, which webrtc/your server use. The resolver confirmed 0.10.3 with axum 0.8.9.

Resolved versions (from the verified lockfile):
`sea-orm 1.1.20`, `sea-orm-migration 1.1.20`, `sqlx 0.8.6`, `sqlx-sqlite 0.8.6`,
`sqlx-postgres 0.8.6`, `libsqlite3-sys 0.30.1` (bundled), `argon2 0.5.3`,
`password-hash 0.5.0`, `rand 0.8.6` (+ `rand_core 0.6.4`), `axum-extra 0.10.3`,
`chrono 0.4.x`. **All existing webrtc/axum/tokio/rustls/ring versions unchanged.**

---

## (a) Startup: auto-create sqlite file if missing + run migrations

For `sqlite:./data.db` we DON'T hand the URL straight to `Database::connect` — by default
sqlx refuses to create a missing file. Build a `SqliteConnectOptions` with
`.create_if_missing(true)` (verified: `sqlx_sqlite::SqliteConnectOptions::create_if_missing`
exists at sqlx-sqlite-0.8.6) and feed it through SeaORM's `SqlxSqliteConnector`, OR — simpler
and DB-agnostic — just append `?mode=rwc` to the sqlite URL so the create flag rides in the
DSN and `Database::connect(url)` works for both backends with one code path. Use the DSN
approach so the *same* `connect_db` works for postgres too.

```rust
// src/db.rs
use sea_orm::{Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Connect to whatever DATABASE_URL points at, defaulting to a local sqlite file that is
/// created on first run. Then run all pending migrations. Idempotent: safe every boot.
pub async fn connect_and_migrate() -> Result<DatabaseConnection, DbErr> {
    // Default: a sqlite file next to the binary. `mode=rwc` => read-write-create:
    // sqlx creates ./data.db (and the journal) if it does not exist.
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://./data.db?mode=rwc".to_string());
    tracing::info!("DB: {}", url.split('@').last().unwrap_or(&url)); // don't log creds

    let db = Database::connect(&url).await?;          // verified signature: connect<C: Into<ConnectOptions>>
    crate::migrations::Migrator::up(&db, None).await?; // None = apply ALL pending
    Ok(db)
}
```

Wire into `main.rs` (before building `AppState`):

```rust
let db = db::connect_and_migrate().await?;   // anyhow::Result<()> -> `?` works (DbErr: std::error::Error)
let cookie_key = axum_extra::extract::cookie::Key::generate(); // see note in (d)
let state = AppState { api, inner, db, cookie_key };
```

> If you prefer NOT to mutate the DSN, the explicit form is:
> ```rust
> use sqlx::sqlite::SqliteConnectOptions;
> use sea_orm::SqlxSqliteConnector;
> let opt = SqliteConnectOptions::new().filename("data.db").create_if_missing(true);
> let db = SqlxSqliteConnector::from_sqlx_sqlite_pool(
>     sqlx::SqlitePool::connect_with(opt).await?);
> ```
> The `?mode=rwc` DSN form is preferred because it keeps one branch-free code path for both
> sqlite and postgres.

## (b) Switch to Postgres — env only, zero code change

```bash
# sqlite (default, nothing to set)
# DATABASE_URL unset  ->  sqlite://./data.db?mode=rwc

# postgres: just export this and restart. Migrations auto-run on boot.
export DATABASE_URL='postgres://user:pass@localhost:5432/nes_web'
# rustls TLS to a managed PG (sslmode in the DSN, handled by sqlx-postgres + rustls):
export DATABASE_URL='postgres://user:pass@db.example.com:5432/nes_web?sslmode=require'
```

Postgres must already exist (PG has no "create file"); for sqlite the file is auto-created.
Both backends are compiled in, so the binary supports either at runtime by URL scheme.

---

## (c) Migrations + entities

### Migrations (`src/migrations/`)

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

`src/migrations/m20260606_000001_init.rs` (DB-agnostic DDL via the schema builder; emits
correct SQLite vs Postgres types automatically):
```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // users(id, username UNIQUE, pass_hash, created_at)
        m.create_table(
            Table::create()
                .table(Users::Table)
                .if_not_exists()
                .col(ColumnDef::new(Users::Id).integer().not_null().auto_increment().primary_key())
                .col(ColumnDef::new(Users::Username).string().not_null().unique_key())
                .col(ColumnDef::new(Users::PassHash).string().not_null())
                .col(ColumnDef::new(Users::CreatedAt).timestamp_with_time_zone().not_null())
                .to_owned(),
        ).await?;

        // sessions(token PK, user_id FK, expires)
        m.create_table(
            Table::create()
                .table(Sessions::Table)
                .if_not_exists()
                .col(ColumnDef::new(Sessions::Token).string().not_null().primary_key())
                .col(ColumnDef::new(Sessions::UserId).integer().not_null())
                .col(ColumnDef::new(Sessions::Expires).timestamp_with_time_zone().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .from(Sessions::Table, Sessions::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        ).await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(Sessions::Table).to_owned()).await?;
        m.drop_table(Table::drop().table(Users::Table).to_owned()).await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users { Table, Id, Username, PassHash, CreatedAt }
#[derive(DeriveIden)]
enum Sessions { Table, Token, UserId, Expires }
```

> **Sessions vs JWT decision:** use the **sessions table + private cookie** shown here, not
> JWT. Reasons specific to this app: (1) requirement (7) "F5/refresh keeps you in your room
> until the battle ends" — a server-side session lets you store/lookup the player's
> `room_id` and re-attach on reconnect, and lets you forcibly invalidate (logout, ban,
> battle-end) which JWT can't; (2) the room/matchmaking layer already needs server state, so
> a sessions table is no extra moving part; (3) no JWT signing-key rotation headaches.
> The `token` is a random opaque 256-bit string stored in a **private (encrypted+signed)**
> cookie via `axum-extra`. (A JWT-in-cookie variant would swap the sessions table for a
> `jsonwebtoken` crate, but loses server-side invalidation — not recommended here.)

### Entities (`src/entities/`)

`src/entities/users.rs`:
```rust
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub username: String,
    pub pass_hash: String,
    pub created_at: ChronoDateTimeUtc, // chrono DateTime<Utc>; needs feature "with-chrono"
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::sessions::Entity")]
    Sessions,
}
impl Related<super::sessions::Entity> for Entity {
    fn to() -> RelationDef { Relation::Sessions.def() }
}
impl ActiveModelBehavior for ActiveModel {}
```

`src/entities/sessions.rs`:
```rust
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub token: String,
    pub user_id: i32,
    pub expires: ChronoDateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(belongs_to = "super::users::Entity",
              from = "Column::UserId", to = "super::users::Column::Id")]
    User,
}
impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef { Relation::User.def() }
}
impl ActiveModelBehavior for ActiveModel {}
```

`src/entities/mod.rs`:
```rust
pub mod users;
pub mod sessions;
pub use users::Entity as Users;
pub use sessions::Entity as Sessions;
```

---

## (d) Password hashing (argon2) + register/login + auth extractor

### AppState additions

`AppState` is a plain `#[derive(Clone)] struct` today, so `FromRef` derivation is clean
(this is the supported shape — `PrivateCookieJar` needs `K: FromRef<S> + Into<Key>`, and
the docs explicitly warn it does NOT work with `Arc<AppState>`):

```rust
use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sea_orm::DatabaseConnection;

#[derive(Clone)]
pub struct AppState {
    pub api: std::sync::Arc<::webrtc::api::API>,
    pub inner: std::sync::Arc<crate::pipeline::AppInner>,
    pub db: DatabaseConnection,   // cheap to clone (Arc'd pool inside)
    pub cookie_key: Key,          // for the PRIVATE (encrypted) session cookie
}

// Lets PrivateCookieJar pull the key out of state.
impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self { state.cookie_key.clone() }
}
```

> `cookie_key` MUST be stable across restarts in prod (else all sessions invalidate on
> deploy). Dev: `Key::generate()`. Prod: load 64 bytes from env, e.g.
> `Key::from(&base64_decoded_secret)`. Keep this in `COOKIE_SECRET` env.

### Password hashing helpers (`src/auth.rs`)

Verified API: `argon2::Argon2::default()` implements `PasswordHasher::hash_password(pwd,
&salt)`; `password_hash::SaltString::generate(rng)` takes `impl CryptoRngCore` satisfied by
`rand 0.8`'s `OsRng`; verification via `PasswordVerifier::verify_password`.

```rust
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::SaltString;
use rand::rngs::OsRng;            // rand 0.8 -> OsRng: CryptoRngCore (rand_core 0.6)

pub fn hash_password(plain: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash: {e}"))?;
    Ok(hash.to_string()) // PHC string "$argon2id$v=19$m=...$...": store directly in pass_hash
}

pub fn verify_password(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn new_session_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect() // 64-hex-char opaque token
}
```

### Register / Login handlers

```rust
use axum::{extract::State, http::StatusCode, Json};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use crate::entities::{users, sessions};

#[derive(serde::Deserialize)]
pub struct Credentials { pub username: String, pub password: String }

const SESSION_COOKIE: &str = "nes_session";
const SESSION_TTL_HOURS: i64 = 12;

pub async fn register(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(cred): Json<Credentials>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    if cred.username.len() < 3 || cred.password.len() < 6 {
        return Err((StatusCode::BAD_REQUEST, "username>=3, password>=6".into()));
    }
    let pass_hash = hash_password(&cred.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let user = users::ActiveModel {
        username: Set(cred.username.clone()),
        pass_hash: Set(pass_hash),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&st.db)
    .await
    .map_err(|_| (StatusCode::CONFLICT, "username taken".into()))?; // UNIQUE violation

    let jar = start_session(&st, jar, user.id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((jar, StatusCode::CREATED))
}

pub async fn login(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(cred): Json<Credentials>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    let user = users::Entity::find()
        .filter(users::Column::Username.eq(&cred.username))
        .one(&st.db).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::UNAUTHORIZED, "bad credentials".into()))?;

    if !verify_password(&cred.password, &user.pass_hash) {
        return Err((StatusCode::UNAUTHORIZED, "bad credentials".into()));
    }
    let jar = start_session(&st, jar, user.id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((jar, StatusCode::OK))
}

async fn start_session(st: &AppState, jar: PrivateCookieJar, user_id: i32)
    -> anyhow::Result<PrivateCookieJar>
{
    let token = new_session_token();
    sessions::ActiveModel {
        token: Set(token.clone()),
        user_id: Set(user_id),
        expires: Set(Utc::now() + Duration::hours(SESSION_TTL_HOURS)),
        ..Default::default()
    }.insert(&st.db).await?;

    let cookie = Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)   // Lax is fine for same-origin server
        .secure(false)              // set true behind HTTPS in prod
        .path("/")
        .max_age(time::Duration::hours(SESSION_TTL_HOURS))
        .build();
    Ok(jar.add(cookie)) // PrivateCookieJar encrypts+signs the value with cookie_key
}

pub async fn logout(State(st): State<AppState>, jar: PrivateCookieJar) -> PrivateCookieJar {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        let _ = sessions::Entity::delete_by_id(c.value().to_string()).exec(&st.db).await;
    }
    jar.remove(Cookie::from(SESSION_COOKIE))
}
```

### Auth extractor / middleware (`AuthUser`)

A custom `FromRequestParts` extractor: pull the private cookie, look up the session, reject
if missing/expired, return the user. Put `AuthUser` in any handler that needs login (e.g.
`/match/find`, `/battle/action`). This is the idiomatic axum 0.8 way — no separate
middleware layer needed, and it composes per-route.

```rust
use axum::extract::{FromRef, FromRequestParts};
use axum::http::{request::Parts, StatusCode};
use axum_extra::extract::cookie::PrivateCookieJar;
use chrono::Utc;
use sea_orm::EntityTrait;
use crate::entities::{users, sessions};

/// Present in a handler signature => the request is authenticated. 401 otherwise.
pub struct AuthUser(pub users::Model);

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let st = AppState::from_ref(state);
        // PrivateCookieJar decrypts using Key (FromRef<AppState>).
        let jar = PrivateCookieJar::<axum_extra::extract::cookie::Key>::from_request_parts(parts, state)
            .await
            .map_err(|_| (StatusCode::UNAUTHORIZED, "no cookie"))?;
        let token = jar.get("nes_session")
            .map(|c| c.value().to_owned())
            .ok_or((StatusCode::UNAUTHORIZED, "no session"))?;

        let sess = sessions::Entity::find_by_id(token)
            .one(&st.db).await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "session not found"))?;
        if sess.expires < Utc::now() {
            return Err((StatusCode::UNAUTHORIZED, "session expired"));
        }
        let user = users::Entity::find_by_id(sess.user_id)
            .one(&st.db).await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "user gone"))?;
        Ok(AuthUser(user))
    }
}
```

Usage in a route handler (ties into requirement (7): look up the player's current room):
```rust
async fn find_match(State(st): State<AppState>, AuthUser(user): AuthUser) -> impl IntoResponse {
    // user.id is the authenticated player; enqueue into matchmaker, return room when paired
}
```

Router wiring (extend the existing `router()` in `signaling.rs`):
```rust
.route("/auth/register", post(auth::register))
.route("/auth/login",    post(auth::login))
.route("/auth/logout",   post(auth::logout))
```

> `time` (used by `Cookie::max_age`) is already in the tree at `0.3.47` (a webrtc dep), so
> no new crate — but add an explicit `time = "0.3"` to `[dependencies]` if you call
> `time::Duration` directly, to make the dependency intentional rather than implicit.

---

## Compile-cost & version-conflict risks (with webrtc / OpenSSL)

- **OpenSSL: NONE.** Verified the solved lockfile has zero `openssl`/`openssl-sys`/
  `native-tls`/`aws-lc-*`. `runtime-tokio-rustls` is what keeps it that way — **do not**
  use SeaORM's `runtime-tokio-native-tls` feature (that would pull OpenSSL/native-tls and
  could fight with the system and double the TLS surface). rustls 0.23 + ring 0.17 are
  shared with webrtc, so there is exactly one TLS/crypto stack in the binary.
- **No version perturbation.** Adding the stack left `webrtc 0.17.1`, `axum 0.8.9`,
  `tokio 1.52.3`, `tower-http 0.6.11`, `rustls 0.23.40`, `ring 0.17.14`, `uuid 1.23.2`,
  `getrandom 0.2.17` byte-for-byte unchanged. The existing project still builds identically.
- **MSRV is satisfied.** Full `cargo build` of the new crates succeeded under rustc 1.92.
  (rustc 1.92 is well above the MSRVs of sea-orm 1.1 / sqlx 0.8 / argon2 0.5.)
- **`rand` two-version coexistence is intentional, not a conflict.** `rand 0.9.4` stays for
  webrtc; `rand 0.8.6` is added for argon2's salt (`OsRng: CryptoRngCore` from rand_core
  0.6). Cargo links both fine (different major versions are separate crates). **Pitfall:** if
  you write `rand::rngs::OsRng` and the wrong `rand` is in scope, `SaltString::generate`
  won't typecheck — make sure `auth.rs` uses the 0.8 `OsRng`. Pinning `rand = "0.8"` in the
  manifest makes the unqualified `rand::` path resolve to 0.8.
- **Heavy compile: the SQLite bundled C build.** `libsqlite3-sys 0.30.1` (bundled) compiles
  the SQLite amalgamation in C — a one-time ~10-20s hit on a clean build and a `cc` toolchain
  requirement (already present on this mac). It does NOT recompile on incremental Rust
  builds. If you want to avoid the C compile entirely, you *can* drop `sqlx-sqlite`'s bundled
  build by depending on system sqlite, but bundled is the zero-setup default and is fine here.
- **Proc-macro / first-build cost.** SeaORM + sqlx pull `sea-query`, `sea-schema`,
  `proc-macro2`/`syn`, `sqlx-macros`. First clean compile of just these crates was ~16s on
  this machine; thereafter incremental. This is additive to (not multiplied with) the
  existing webrtc/vpx/opus build.
- **`getrandom` is already triple-versioned** (0.2/0.3/0.4) in the existing tree; the auth
  stack reuses 0.2 (via rand_core 0.6 -> getrandom 0.2). No new getrandom major added.

## TL;DR

SeaORM 1.1 + sea-orm-migration 1.1 with `runtime-tokio-rustls` + both `sqlx-sqlite` and
`sqlx-postgres`, argon2 0.5, rand 0.8 (for the salt), axum-extra 0.10 private cookies, chrono
0.4. Server-side sessions table (not JWT) for room re-attach + forced invalidation.
`sqlite://./data.db?mode=rwc` auto-creates the file; `Migrator::up(&db, None)` runs migrations
on boot; flip `DATABASE_URL` to a `postgres://...` DSN to switch with zero code changes.
**Verified to resolve and compile under rustc 1.92 with no OpenSSL and no change to the
existing webrtc/axum/tokio/rustls/ring versions.**
