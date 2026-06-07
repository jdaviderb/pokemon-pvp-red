use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "matches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub room_id: i32,
    pub p1_user: i32,
    pub p2_user: i32,
    pub p1_species: i32,
    pub p2_species: i32,
    pub winner_seat: Option<i32>,
    pub ended_at: ChronoDateTimeUtc,
    /// Public room UUID + name snapshots (history survives the room + guest-account cleanup).
    pub public_id: Option<String>,
    pub p1_name: Option<String>,
    pub p2_name: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
