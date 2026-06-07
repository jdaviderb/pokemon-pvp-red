//! Provider-agnostic OAuth2 (authorization-code) social login. Google is wired now; add Twitter,
//! Apple, GitHub, ... by registering another `OAuthProvider` in `registry_from_env` — the flow,
//! routes, and DB schema (`oauth_accounts(provider, subject)`) stay the same.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use chrono::Utc;
use rand::rngs::OsRng;
use rand::RngCore;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use serde::Deserialize;

use crate::entities::{oauth_accounts, users};
use crate::signaling::AppState;

const STATE_COOKIE: &str = "oauth_state";

/// One OAuth2 provider. `extract` pulls (subject, display_name) out of the provider's userinfo JSON,
/// so each provider's quirks live in exactly one place.
#[derive(Clone)]
pub struct OAuthProvider {
    pub name: &'static str,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub userinfo_url: &'static str,
    pub scope: &'static str,
    pub extract: fn(&serde_json::Value) -> Option<(String, String)>,
}

pub type Registry = HashMap<String, OAuthProvider>;

/// Build the provider registry from env. Add a block per provider as their env vars appear.
pub fn registry_from_env() -> Registry {
    let mut r = Registry::new();

    // Google — needs GOOGLE_CLIENT_ID / GOOGLE_CLIENT_SECRET / GOOGLE_REDIRECT_URI.
    if let (Ok(id), Ok(secret), Ok(redirect)) = (
        std::env::var("GOOGLE_CLIENT_ID"),
        std::env::var("GOOGLE_CLIENT_SECRET"),
        std::env::var("GOOGLE_REDIRECT_URI"),
    ) {
        if !id.is_empty() && !secret.is_empty() && !redirect.is_empty() {
            r.insert(
                "google".into(),
                OAuthProvider {
                    name: "google",
                    client_id: id,
                    client_secret: secret,
                    redirect_uri: redirect,
                    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
                    token_url: "https://oauth2.googleapis.com/token",
                    userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo",
                    scope: "openid email profile",
                    extract: |j| {
                        let sub = j.get("sub")?.as_str()?.to_string();
                        let name = j
                            .get("name")
                            .and_then(|v| v.as_str())
                            .or_else(|| j.get("email").and_then(|v| v.as_str()))
                            .unwrap_or("Trainer")
                            .to_string();
                        Some((sub, name))
                    },
                },
            );
            tracing::info!("oauth: google provider configured");
        }
    }
    r
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// GET /auth/oauth/{provider} -> redirect to the provider's consent screen. A random `state` is
/// stored in a short-lived signed cookie and checked on the callback (CSRF protection).
pub async fn start(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Path(provider): Path<String>,
) -> Result<(PrivateCookieJar, Redirect), (StatusCode, String)> {
    let p = st
        .oauth
        .get(&provider)
        .ok_or((StatusCode::NOT_FOUND, "unknown provider".into()))?;
    let state = random_hex(24);
    let url = reqwest::Url::parse_with_params(
        p.authorize_url,
        &[
            ("client_id", p.client_id.as_str()),
            ("redirect_uri", p.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", p.scope),
            ("state", state.as_str()),
            ("access_type", "online"),
            ("prompt", "select_account"),
        ],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Bind the state to the provider so a state minted for one can't be replayed on another.
    let cookie = Cookie::build((STATE_COOKIE, format!("{provider}:{state}")))
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(st.cookie_secure)
        .path("/")
        .max_age(time::Duration::minutes(10))
        .build();
    Ok((jar.add(cookie), Redirect::to(url.as_str())))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// GET /auth/oauth/{provider}/callback -> verify state, exchange code, fetch userinfo, find-or-create
/// the linked user, start a session, and land on "/".
pub async fn callback(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Result<(PrivateCookieJar, Redirect), (StatusCode, String)> {
    let p = st
        .oauth
        .get(&provider)
        .ok_or((StatusCode::NOT_FOUND, "unknown provider".into()))?;

    // Always clear the one-shot state cookie.
    let want = jar.get(STATE_COOKIE).map(|c| c.value().to_string());
    let jar = jar.remove(Cookie::from(STATE_COOKIE));

    // User denied / provider returned an error / no code -> back to login, no fuss.
    if q.error.is_some() || q.code.is_none() {
        return Ok((jar, Redirect::to("/login")));
    }
    let code = q.code.unwrap();
    let state = q.state.unwrap_or_default();

    // CSRF: the returned state must equal the one we stored, bound to this provider.
    if state.is_empty() || want.as_deref() != Some(format!("{provider}:{state}").as_str()) {
        return Err((StatusCode::BAD_REQUEST, "bad oauth state".into()));
    }

    // Bounded client so a slow/degraded provider can't pile up long-lived requests. Detailed errors
    // are logged server-side; the client only ever sees a generic message (no leak).
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| {
            tracing::error!("oauth {provider}: client build: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "login failed".to_string())
        })?;
    let bad_gateway = |what: &str, e: &dyn std::fmt::Display| {
        tracing::error!("oauth {provider} {what}: {e}");
        (StatusCode::BAD_GATEWAY, "oauth provider error".to_string())
    };
    // 1. Exchange the authorization code for an access token (confidential client w/ secret).
    let tok: serde_json::Value = client
        .post(p.token_url)
        .form(&[
            ("code", code.as_str()),
            ("client_id", p.client_id.as_str()),
            ("client_secret", p.client_secret.as_str()),
            ("redirect_uri", p.redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| bad_gateway("token request", &e))?
        .json()
        .await
        .map_err(|e| bad_gateway("token json", &e))?;
    let access = tok
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_GATEWAY, "oauth provider error".into()))?;

    // 2. Fetch userinfo and pull the stable subject + a display name.
    let info: serde_json::Value = client
        .get(p.userinfo_url)
        .bearer_auth(access)
        .send()
        .await
        .map_err(|e| bad_gateway("userinfo request", &e))?
        .json()
        .await
        .map_err(|e| bad_gateway("userinfo json", &e))?;
    let (subject, display) = (p.extract)(&info)
        .ok_or((StatusCode::BAD_GATEWAY, "oauth provider error".into()))?;

    // 3. Find-or-create the user linked to (provider, subject) — never by email (no takeover).
    let user_id = upsert_oauth_user(&st, p.name, &subject, &display)
        .await
        .map_err(|e| {
            tracing::error!("oauth {provider} upsert: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "login failed".to_string())
        })?;
    let jar = crate::auth::start_session(&st, jar, user_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((jar, Redirect::to("/")))
}

/// Return the user_id for (provider, subject); create the user + link on first login.
async fn upsert_oauth_user(
    st: &AppState,
    provider: &str,
    subject: &str,
    display: &str,
) -> anyhow::Result<i32> {
    if let Some(acc) = find_link(st, provider, subject).await? {
        return Ok(acc);
    }
    let username = unique_username(st, &sanitize_username(display)).await?;
    // Atomic: the user row and its oauth link commit together, so a failed link never leaves an
    // orphan user (and the (provider,subject) UNIQUE index resolves the concurrent-login race).
    let txn = st.db.begin().await?;
    let user = users::ActiveModel {
        username: Set(username),
        pass_hash: Set(String::new()), // empty PHC => password login impossible for this account
        wins: Set(0),
        losses: Set(0),
        created_at: Set(Utc::now()),
        name_chosen: Set(false), // must pick a trainer name via the name-entry on first login
        ..Default::default()
    }
    .insert(&txn)
    .await?;
    let link = oauth_accounts::ActiveModel {
        user_id: Set(user.id),
        provider: Set(provider.to_string()),
        subject: Set(subject.to_string()),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&txn)
    .await;
    match link {
        Ok(_) => {
            txn.commit().await?;
            Ok(user.id)
        }
        Err(_) => {
            // Roll back the just-created user, then: if a concurrent login linked this identity
            // first, reuse theirs; otherwise propagate the real error (no orphan left behind).
            let _ = txn.rollback().await;
            find_link(st, provider, subject)
                .await?
                .ok_or_else(|| anyhow::anyhow!("oauth link insert failed"))
        }
    }
}

async fn find_link(st: &AppState, provider: &str, subject: &str) -> anyhow::Result<Option<i32>> {
    Ok(oauth_accounts::Entity::find()
        .filter(oauth_accounts::Column::Provider.eq(provider))
        .filter(oauth_accounts::Column::Subject.eq(subject))
        .one(&st.db)
        .await?
        .map(|a| a.user_id))
}

fn sanitize_username(s: &str) -> String {
    let mut out: String =
        s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(16).collect();
    if out.len() < 3 {
        out = format!("trainer{out}");
    }
    out
}

/// First free `username`, `username2`, ... then a random suffix as a last resort.
async fn unique_username(st: &AppState, base: &str) -> anyhow::Result<String> {
    for n in 0..50u32 {
        let cand = if n == 0 { base.to_string() } else { format!("{base}{n}") };
        let taken = users::Entity::find()
            .filter(users::Column::Username.eq(&cand))
            .one(&st.db)
            .await?
            .is_some();
        if !taken {
            return Ok(cand);
        }
    }
    Ok(format!("{base}{}", random_hex(3)))
}
