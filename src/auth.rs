//! Registration / login / logout, argon2id password hashing, server-side sessions in a
//! private (encrypted+signed) cookie, and an `AuthUser` extractor that gates handlers.

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::{request::Parts, StatusCode};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar, SameSite};
use chrono::{Duration, Utc};
use rand::rngs::OsRng;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::entities::{sessions, users};
use crate::signaling::AppState;

const SESSION_COOKIE: &str = "nes_session";
const SESSION_TTL_HOURS: i64 = 12;

pub fn hash_password(plain: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string())
}

pub fn verify_password(plain: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|p| Argon2::default().verify_password(plain.as_bytes(), &p).is_ok())
        .unwrap_or(false)
}

fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(serde::Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Username/password login is on when the `login_username` feature flag is set (or in DEV).
pub(crate) async fn username_login_on(st: &AppState) -> bool {
    st.dev || crate::flags::enabled(&st.db, crate::flags::LOGIN_USERNAME).await
}

pub async fn register(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(c): Json<Credentials>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    if !username_login_on(&st).await {
        return Err((StatusCode::FORBIDDEN, "username login disabled".into()));
    }
    if c.username.len() < 3 || c.password.len() < 6 {
        return Err((StatusCode::BAD_REQUEST, "username>=3, password>=6".into()));
    }
    let pass_hash =
        hash_password(&c.password).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let user = users::ActiveModel {
        username: Set(c.username),
        pass_hash: Set(pass_hash),
        wins: Set(0),
        losses: Set(0),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&st.db)
    .await
    .map_err(|_| (StatusCode::CONFLICT, "username taken".into()))?;
    Ok((
        start_session(&st, jar, user.id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        StatusCode::CREATED,
    ))
}

pub async fn login(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(c): Json<Credentials>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    if !username_login_on(&st).await {
        return Err((StatusCode::FORBIDDEN, "username login disabled".into()));
    }
    let user = users::Entity::find()
        .filter(users::Column::Username.eq(&c.username))
        .one(&st.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::UNAUTHORIZED, "bad credentials".to_string()))?;
    if !verify_password(&c.password, &user.pass_hash) {
        return Err((StatusCode::UNAUTHORIZED, "bad credentials".into()));
    }
    Ok((
        start_session(&st, jar, user.id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        StatusCode::OK,
    ))
}

pub(crate) async fn start_session(
    st: &AppState,
    jar: PrivateCookieJar,
    user_id: i32,
) -> anyhow::Result<PrivateCookieJar> {
    let token = new_token();
    sessions::ActiveModel {
        token: Set(token.clone()),
        user_id: Set(user_id),
        expires: Set(Utc::now() + Duration::hours(SESSION_TTL_HOURS)),
    }
    .insert(&st.db)
    .await?;
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(st.cookie_secure)
        .path("/")
        .max_age(time::Duration::hours(SESSION_TTL_HOURS))
        .build();
    Ok(jar.add(cookie))
}

#[derive(serde::Deserialize, Default)]
pub struct GuestReq {
    /// Optional chosen nickname; sanitized + made unique. Falls back to a random Guest-XXXX.
    #[serde(default)]
    pub nickname: String,
}

/// POST /auth/guest {nickname?} -> create a disposable guest account and start a session. Gated by
/// the `guest_mode` feature flag. Progress is irrelevant; the row can be purged later.
pub async fn guest(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(req): Json<GuestReq>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
    if !crate::flags::enabled(&st.db, crate::flags::GUEST_MODE).await {
        return Err((StatusCode::FORBIDDEN, "guest mode disabled".into()));
    }
    let username = match sanitize_nick(&req.nickname) {
        Some(base) => unique_or_suffix(&st, &base).await,
        None => unique_guest_name(&st).await,
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let user = users::ActiveModel {
        username: Set(username),
        pass_hash: Set(String::new()),
        wins: Set(0),
        losses: Set(0),
        created_at: Set(Utc::now()),
        is_guest: Set(true),
        ..Default::default()
    }
    .insert(&st.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let jar = start_session(&st, jar, user.id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((jar, StatusCode::OK))
}

#[derive(serde::Deserialize)]
pub struct NameQ {
    #[serde(default)]
    pub name: String,
}

/// GET /api/name-available?name=X -> {available, name} (sanitized). Used by the guest name-entry to
/// validate the trainer name is unique before creating the account.
pub async fn name_available(State(st): State<AppState>, Query(q): Query<NameQ>) -> Json<serde_json::Value> {
    match sanitize_nick(&q.name) {
        None => Json(serde_json::json!({ "available": false, "reason": "too short" })),
        Some(name) => {
            let free = username_free(&st, &name).await.unwrap_or(false);
            Json(serde_json::json!({ "available": free, "name": name }))
        }
    }
}

/// Keep alphanumerics/-/_ (max 20); None if fewer than 3 usable chars remain (-> random name).
fn sanitize_nick(s: &str) -> Option<String> {
    let out: String =
        s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(20).collect();
    (out.len() >= 3).then_some(out)
}

async fn username_free(st: &AppState, name: &str) -> anyhow::Result<bool> {
    Ok(users::Entity::find()
        .filter(users::Column::Username.eq(name))
        .one(&st.db)
        .await?
        .is_none())
}

/// `base` if free, else `base-XXXX`; random Guest name as a last resort.
async fn unique_or_suffix(st: &AppState, base: &str) -> anyhow::Result<String> {
    use rand::RngCore;
    if username_free(st, base).await? {
        return Ok(base.to_string());
    }
    for _ in 0..50 {
        let mut b = [0u8; 2];
        OsRng.fill_bytes(&mut b);
        let cand = format!("{base}-{:02X}{:02X}", b[0], b[1]);
        if username_free(st, &cand).await? {
            return Ok(cand);
        }
    }
    unique_guest_name(st).await
}

/// First free "Guest-XXXX" (random hex), retried; random suffix as a last resort.
async fn unique_guest_name(st: &AppState) -> anyhow::Result<String> {
    use rand::RngCore;
    for _ in 0..50 {
        let mut b = [0u8; 2];
        OsRng.fill_bytes(&mut b);
        let cand = format!("Guest-{:02X}{:02X}", b[0], b[1]);
        if username_free(st, &cand).await? {
            return Ok(cand);
        }
    }
    let mut b = [0u8; 4];
    OsRng.fill_bytes(&mut b);
    Ok(format!("Guest-{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>()))
}

pub async fn logout(State(st): State<AppState>, jar: PrivateCookieJar) -> PrivateCookieJar {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        let _ = sessions::Entity::delete_by_id(c.value().to_string()).exec(&st.db).await;
    }
    jar.remove(Cookie::from(SESSION_COOKIE))
}

/// Present in a handler signature => the request is authenticated; 401 otherwise.
pub struct AuthUser(pub users::Model);

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    Key: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let st = AppState::from_ref(state);
        let jar = PrivateCookieJar::<Key>::from_request_parts(parts, state)
            .await
            .map_err(|_| (StatusCode::UNAUTHORIZED, "no cookie"))?;
        let token = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_owned())
            .ok_or((StatusCode::UNAUTHORIZED, "no session"))?;
        let sess = sessions::Entity::find_by_id(token)
            .one(&st.db)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "session not found"))?;
        if sess.expires < Utc::now() {
            return Err((StatusCode::UNAUTHORIZED, "expired"));
        }
        let user = users::Entity::find_by_id(sess.user_id)
            .one(&st.db)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "db"))?
            .ok_or((StatusCode::UNAUTHORIZED, "user gone"))?;
        Ok(AuthUser(user))
    }
}

/// GET /api/me -> {user, room?} for first-paint routing (login? lobby? room?).
#[derive(serde::Serialize)]
pub struct MeRoom {
    pub id: String,
    pub phase: String,
    pub seat: u8,
}

pub async fn me(State(st): State<AppState>, AuthUser(u): AuthUser) -> Json<serde_json::Value> {
    let room = crate::rooms::current_room_for(&st, u.id).await;
    Json(serde_json::json!({
        "user": {"id": u.id, "username": u.username, "wins": u.wins, "losses": u.losses, "is_guest": u.is_guest, "name_chosen": u.name_chosen},
        "room": room,
        "dev": st.dev,
    }))
}

#[derive(serde::Deserialize)]
pub struct SetNameReq {
    #[serde(default)]
    pub name: String,
}

/// POST /auth/set-name {name} (authenticated): set the user's unique trainer name + mark it chosen.
/// Used by the name-entry screen for accounts created without a name (Google first login).
pub async fn set_name(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(req): Json<SetNameReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let name = sanitize_nick(&req.name)
        .ok_or((StatusCode::BAD_REQUEST, "name needs 3+ characters".to_string()))?;
    let current = u.username.clone();
    if name != current
        && !username_free(&st, &name)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Err((StatusCode::CONFLICT, "name taken".into()));
    }
    let mut am: users::ActiveModel = u.into();
    am.username = Set(name);
    am.name_chosen = Set(true);
    am.update(&st.db).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}
