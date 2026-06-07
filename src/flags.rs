//! Runtime feature flags, stored one-row-per-flag in `feature_flags` and read live from the DB so a
//! flag can be flipped by editing its row (no recompile, no restart). Defaults are seeded at boot.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};

use crate::entities::feature_flags;
use crate::rooms::GameState;

/// Show the username/password login form + allow /auth/register and /auth/login. Default OFF
/// (Google is the primary login).
pub const LOGIN_USERNAME: &str = "login_username";
/// Allow "play as guest" — a disposable account with a random nickname. Default OFF.
pub const GUEST_MODE: &str = "guest_mode";
/// Show the "MODE 1 VS 1 / LEVEL CAP" corner box on the title screen. Default OFF.
pub const TITLE_MODE_BOX: &str = "title_mode_box";

/// All known flags and their default-on-first-boot value.
const DEFAULTS: &[(&str, bool)] =
    &[(LOGIN_USERNAME, false), (GUEST_MODE, true), (TITLE_MODE_BOX, false)];

/// Insert any missing known flag with its default so it exists in the table to be toggled.
pub async fn seed_defaults(db: &DatabaseConnection) -> anyhow::Result<()> {
    for (key, def) in DEFAULTS {
        if feature_flags::Entity::find_by_id(*key).one(db).await?.is_none() {
            feature_flags::ActiveModel {
                key: Set((*key).to_string()),
                enabled: Set(*def),
                updated_at: Set(Utc::now()),
            }
            .insert(db)
            .await?;
        }
    }
    Ok(())
}

/// Current value of a flag (false if missing or on any DB error).
pub async fn enabled(db: &DatabaseConnection, key: &str) -> bool {
    feature_flags::Entity::find_by_id(key)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|f| f.enabled)
        .unwrap_or(false)
}

/// Read every known flag from the DB into a map (for the in-memory cache).
async fn read_all(db: &DatabaseConnection) -> HashMap<String, bool> {
    let mut m = HashMap::new();
    for (key, _) in DEFAULTS {
        m.insert((*key).to_string(), enabled(db, key).await);
    }
    m
}

/// Cached flag value (from `GameState.flags_cache`) — no DB round-trip. Used on hot paths like
/// /api/config (every page load). False until the cache is seeded.
pub fn cached(game: &GameState, key: &str) -> bool {
    game.flags_cache.lock().unwrap().get(key).copied().unwrap_or(false)
}

/// Populate the flag cache once, synchronously (call at boot before serving).
pub async fn seed_cache(game: &GameState) {
    *game.flags_cache.lock().unwrap() = read_all(&game.db).await;
}

/// Keep the flag cache fresh so a flipped flag takes effect within ~30s without a restart.
pub fn spawn_cache_refresher(game: Arc<GameState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let m = read_all(&game.db).await;
            *game.flags_cache.lock().unwrap() = m;
        }
    });
}
