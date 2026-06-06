//! DB connection + migrations. DB-agnostic via SeaORM: SQLite by default (auto-created on first
//! run), Postgres by setting DATABASE_URL — no code change.

use sea_orm::{Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Connect to DATABASE_URL (default = local sqlite, auto-created with `?mode=rwc`), then run all
/// pending migrations. Idempotent — safe on every boot.
pub async fn connect_and_migrate() -> Result<DatabaseConnection, DbErr> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://./data.db?mode=rwc".to_string());
    tracing::info!("DB: {}", url.split('@').last().unwrap_or(&url)); // never log creds
    let db = Database::connect(&url).await?;
    crate::migrations::Migrator::up(&db, None).await?; // None = apply ALL pending
    Ok(db)
}
