//! Leaderboard: wins per trainer over Today / Weekly / Monthly windows.
//!
//! Production shape: a background job recomputes the board every `RANKING_REFRESH_SECS` (default 300)
//! and caches it BOTH in memory and in a JSON file (`cache/ranking.json`). `/api/ranking` is then a
//! cheap in-memory read (no DB hit per request), and on restart the file is loaded so the board is
//! served instantly from the last snapshot until the job refreshes it. The job runs only on the
//! coordinator/solo process (workers share the DB but don't serve users).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::{json, Value};

/// In-memory snapshot of the board, shared with the /api/ranking handler.
pub type RankingCache = Arc<Mutex<Value>>;

const FILE: &str = "cache/ranking.json";
const TOP_N: i64 = 50;

pub fn new_cache() -> RankingCache {
    Arc::new(Mutex::new(json!({ "today": [], "weekly": [], "monthly": [], "updated": null })))
}

/// Top trainers by wins in the last `hours`, computed from the finished-match history.
async fn window_wins(db: &DatabaseConnection, hours: i64) -> Vec<Value> {
    let since = (Utc::now() - ChronoDuration::hours(hours)).to_rfc3339();
    let backend = db.get_database_backend();
    // Postgres won't implicitly compare a `timestamptz` column to a bound TEXT param, so cast it
    // ($1::timestamptz). sqlite stores datetimes as text and compares fine with a plain `?`.
    let ph = if matches!(backend, DatabaseBackend::Postgres) { "$1::timestamptz" } else { "?" };
    let sql = format!(
        "SELECT name, COUNT(*) AS wins FROM (\
            SELECT (CASE WHEN winner_seat = 1 THEN p1_name ELSE p2_name END) AS name \
            FROM matches WHERE winner_seat IS NOT NULL AND ended_at >= {ph}\
         ) t WHERE name IS NOT NULL GROUP BY name ORDER BY wins DESC, name ASC LIMIT {TOP_N}"
    );
    let rows = match db
        .query_all(Statement::from_sql_and_values(backend, &sql, [since.into()]))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("ranking window_wins query failed: {e}");
            Vec::new()
        }
    };
    rows.iter()
        .filter_map(|r| {
            let name: String = r.try_get("", "name").ok()?;
            let wins: i64 = r.try_get("", "wins").ok()?;
            Some(json!({ "name": name, "wins": wins }))
        })
        .collect()
}

/// Recompute all three windows.
pub async fn compute(db: &DatabaseConnection) -> Value {
    json!({
        "today": window_wins(db, 24).await,
        "weekly": window_wins(db, 24 * 7).await,
        "monthly": window_wins(db, 24 * 30).await,
        "updated": Utc::now().to_rfc3339(),
    })
}

/// Load the last cached file into memory on boot (instant board, survives restart).
pub fn load_file(cache: &RankingCache) {
    if let Ok(s) = std::fs::read_to_string(FILE) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            *cache.lock().unwrap() = v;
        }
    }
}

/// Background job: refresh the cache + file every `interval` (computes once immediately on boot).
pub fn spawn_job(db: DatabaseConnection, cache: RankingCache, interval: Duration) {
    tokio::spawn(async move {
        loop {
            let v = compute(&db).await;
            *cache.lock().unwrap() = v.clone();
            let _ = std::fs::create_dir_all("cache");
            if let Ok(s) = serde_json::to_string(&v) {
                let _ = std::fs::write(FILE, s);
            }
            tokio::time::sleep(interval).await;
        }
    });
}
