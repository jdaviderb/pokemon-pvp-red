use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // users
        m.create_table(
            Table::create()
                .table(Users::Table)
                .if_not_exists()
                .col(ColumnDef::new(Users::Id).integer().not_null().auto_increment().primary_key())
                .col(ColumnDef::new(Users::Username).string().not_null().unique_key())
                .col(ColumnDef::new(Users::PassHash).string().not_null())
                .col(ColumnDef::new(Users::Wins).integer().not_null().default(0))
                .col(ColumnDef::new(Users::Losses).integer().not_null().default(0))
                .col(ColumnDef::new(Users::CreatedAt).timestamp_with_time_zone().not_null())
                .to_owned(),
        )
        .await?;

        // sessions(token PK, user_id FK, expires)
        m.create_table(
            Table::create()
                .table(Sessions::Table)
                .if_not_exists()
                .col(ColumnDef::new(Sessions::Token).string().not_null().primary_key())
                .col(ColumnDef::new(Sessions::UserId).integer().not_null())
                .col(ColumnDef::new(Sessions::Expires).timestamp_with_time_zone().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .from(Sessions::Table, Sessions::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;

        // rooms — the LIVE room row (survives restart only for abandonment cleanup)
        m.create_table(
            Table::create()
                .table(Rooms::Table)
                .if_not_exists()
                .col(ColumnDef::new(Rooms::Id).integer().not_null().auto_increment().primary_key())
                .col(ColumnDef::new(Rooms::Phase).string().not_null())
                .col(ColumnDef::new(Rooms::P1User).integer().not_null())
                .col(ColumnDef::new(Rooms::P2User).integer().not_null())
                .col(ColumnDef::new(Rooms::P1Species).integer().null())
                .col(ColumnDef::new(Rooms::P2Species).integer().null())
                .col(ColumnDef::new(Rooms::Level).integer().not_null().default(50))
                .col(ColumnDef::new(Rooms::WinnerSeat).integer().null())
                .col(ColumnDef::new(Rooms::CreatedAt).timestamp_with_time_zone().not_null())
                .col(ColumnDef::new(Rooms::EndedAt).timestamp_with_time_zone().null())
                .to_owned(),
        )
        .await?;

        // matches — finished-game history
        m.create_table(
            Table::create()
                .table(Matches::Table)
                .if_not_exists()
                .col(ColumnDef::new(Matches::Id).integer().not_null().auto_increment().primary_key())
                .col(ColumnDef::new(Matches::RoomId).integer().not_null())
                .col(ColumnDef::new(Matches::P1User).integer().not_null())
                .col(ColumnDef::new(Matches::P2User).integer().not_null())
                .col(ColumnDef::new(Matches::P1Species).integer().not_null())
                .col(ColumnDef::new(Matches::P2Species).integer().not_null())
                .col(ColumnDef::new(Matches::WinnerSeat).integer().null())
                .col(ColumnDef::new(Matches::EndedAt).timestamp_with_time_zone().not_null())
                .to_owned(),
        )
        .await?;

        // user_room — 1 active room per user (F5 resume)
        m.create_table(
            Table::create()
                .table(UserRoom::Table)
                .if_not_exists()
                .col(ColumnDef::new(UserRoom::UserId).integer().not_null().primary_key())
                .col(ColumnDef::new(UserRoom::RoomId).integer().not_null())
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for t in [
            UserRoom::Table.into_iden(),
            Matches::Table.into_iden(),
            Rooms::Table.into_iden(),
            Sessions::Table.into_iden(),
            Users::Table.into_iden(),
        ] {
            m.drop_table(Table::drop().table(t).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Username,
    PassHash,
    Wins,
    Losses,
    CreatedAt,
}
#[derive(DeriveIden)]
enum Sessions {
    Table,
    Token,
    UserId,
    Expires,
}
#[derive(DeriveIden)]
enum Rooms {
    Table,
    Id,
    Phase,
    P1User,
    P2User,
    P1Species,
    P2Species,
    Level,
    WinnerSeat,
    CreatedAt,
    EndedAt,
}
#[derive(DeriveIden)]
enum Matches {
    Table,
    Id,
    RoomId,
    P1User,
    P2User,
    P1Species,
    P2Species,
    WinnerSeat,
    EndedAt,
}
#[derive(DeriveIden)]
enum UserRoom {
    Table,
    UserId,
    RoomId,
}
