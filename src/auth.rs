//! Registration / login / logout, argon2id password hashing, server-side sessions in a
//! private (encrypted+signed) cookie, and an `AuthUser` extractor that gates handlers.

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRef, FromRequestParts, State};
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

pub async fn register(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Json(c): Json<Credentials>,
) -> Result<(PrivateCookieJar, StatusCode), (StatusCode, String)> {
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
    pub id: i32,
    pub phase: String,
    pub seat: u8,
}

pub async fn me(State(st): State<AppState>, AuthUser(u): AuthUser) -> Json<serde_json::Value> {
    let room = crate::rooms::current_room_for(&st, u.id).await;
    Json(serde_json::json!({
        "user": {"id": u.id, "username": u.username, "wins": u.wins, "losses": u.losses},
        "room": room,
        "dev": st.dev,
    }))
}
