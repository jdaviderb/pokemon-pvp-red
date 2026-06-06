use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "rooms")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub phase: String,
    pub p1_user: i32,
    pub p2_user: i32,
    pub p1_species: Option<i32>,
    pub p2_species: Option<i32>,
    pub level: i32,
    pub winner_seat: Option<i32>,
    pub created_at: ChronoDateTimeUtc,
    pub ended_at: Option<ChronoDateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
