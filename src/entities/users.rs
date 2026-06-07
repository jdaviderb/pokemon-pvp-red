use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub username: String,
    pub pass_hash: String,
    pub wins: i32,
    pub losses: i32,
    pub created_at: ChronoDateTimeUtc,
    /// Disposable guest account (random nickname, progress irrelevant); false for real accounts.
    #[sea_orm(default_value = false)]
    pub is_guest: bool,
    /// Whether the user has picked their trainer name. New OAuth users are created with `false` and
    /// must choose one via the name-entry screen before playing.
    #[sea_orm(default_value = true)]
    pub name_chosen: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
