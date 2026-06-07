use sea_orm_migration::prelude::*;

mod m20260606_000001_init;
mod m20260606_000002_google;
mod m20260606_000003_flags;
mod m20260606_000004_match_history;
mod m20260607_000005_name_chosen;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260606_000001_init::Migration),
            Box::new(m20260606_000002_google::Migration),
            Box::new(m20260606_000003_flags::Migration),
            Box::new(m20260606_000004_match_history::Migration),
            Box::new(m20260607_000005_name_chosen::Migration),
        ]
    }
}
