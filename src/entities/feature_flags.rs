use sea_orm::entity::prelude::*;

/// Runtime feature flags (key -> enabled). Read live from the DB so a flag can be toggled by
/// editing the row (no recompile). Defaults are seeded at boot in `flags.rs`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "feature_flags")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub enabled: bool,
    pub updated_at: ChronoDateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
