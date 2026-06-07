use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Indexes the leaderboard + collection queries need. The ranking job scans by `ended_at` (every
/// window), and /api/collection scans by the winner's user id — both full-scan `matches` without
/// these, which stalls at scale (and on sqlite a long scan blocks the single writer).
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_matches_ended_at")
                .table(Matches::Table)
                .col(Matches::EndedAt)
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_matches_p1_winner")
                .table(Matches::Table)
                .col(Matches::P1User)
                .col(Matches::WinnerSeat)
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_matches_p2_winner")
                .table(Matches::Table)
                .col(Matches::P2User)
                .col(Matches::WinnerSeat)
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        for n in ["idx_matches_ended_at", "idx_matches_p1_winner", "idx_matches_p2_winner"] {
            m.drop_index(Index::drop().if_exists().name(n).table(Matches::Table).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Matches {
    Table,
    EndedAt,
    P1User,
    P2User,
    WinnerSeat,
}
