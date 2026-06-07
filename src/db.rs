//! DB connection + migrations. DB-agnostic via SeaORM: SQLite by default (auto-created on first
//! run), Postgres by setting DATABASE_URL — no code change.

use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Connect to DATABASE_URL (default = local sqlite, auto-created). `migrate=true` (coordinator /
/// solo) runs all pending migrations and enables sqlite WAL; worker processes pass `false` so N
/// workers don't all re-run migrations + hammer the writer on boot.
///
/// Pools are kept SMALL per process: with many worker processes each opening its own pool, a large
/// per-pool max would blow up total connections (and lock a single-writer sqlite file). Use Postgres
/// (DATABASE_URL) behind a pooler for real multi-process scale.
pub async fn connect(migrate: bool) -> Result<DatabaseConnection, DbErr> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://./data.db?mode=rwc".to_string());
    tracing::info!("DB: {} (migrate={migrate})", url.split('@').last().unwrap_or(&url)); // never log creds
    let is_sqlite = url.starts_with("sqlite");

    let mut opt = ConnectOptions::new(url);
    opt.max_connections(if migrate { 16 } else { 4 })
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(30));
    let db = Database::connect(opt).await?;

    if migrate {
        if is_sqlite {
            // WAL is a persistent file setting (writers don't block readers) — set once here; every
            // worker process then benefits when it opens the shared file.
            let _ = db.execute_unprepared("PRAGMA journal_mode=WAL;").await;
        }
        crate::migrations::Migrator::up(&db, None).await?; // None = apply ALL pending
    }
    Ok(db)
}

/// Back-compat helper for the solo path: connect + migrate.
pub async fn connect_and_migrate() -> Result<DatabaseConnection, DbErr> {
    connect(true).await
}
