use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // Link each finished match to its public room UUID + snapshot the player names, so the
        // result is queryable by share link and survives the (ephemeral) room / guest accounts.
        for col in [Matches::PublicId, Matches::P1Name, Matches::P2Name] {
            m.alter_table(
                Table::alter()
                    .table(Matches::Table)
                    .add_column_if_not_exists(ColumnDef::new(col).string().null())
                    .to_owned(),
            )
            .await?;
        }
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_matches_public_id")
                .table(Matches::Table)
                .col(Matches::PublicId)
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_index(Index::drop().name("idx_matches_public_id").table(Matches::Table).to_owned())
            .await?;
        for col in [Matches::PublicId, Matches::P1Name, Matches::P2Name] {
            m.alter_table(Table::alter().table(Matches::Table).drop_column(col).to_owned()).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Matches {
    Table,
    PublicId,
    P1Name,
    P2Name,
}
