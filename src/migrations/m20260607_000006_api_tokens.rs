use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // Bearer tokens that let an AI agent (the MCP server) authenticate as a user without a
        // browser cookie. One per user is enough for now; the token is the primary key.
        m.create_table(
            Table::create()
                .table(ApiTokens::Table)
                .if_not_exists()
                .col(ColumnDef::new(ApiTokens::Token).string().not_null().primary_key())
                .col(ColumnDef::new(ApiTokens::UserId).integer().not_null())
                .col(ColumnDef::new(ApiTokens::Label).string().not_null().default(""))
                .col(ColumnDef::new(ApiTokens::CreatedAt).timestamp_with_time_zone().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .from(ApiTokens::Table, ApiTokens::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_api_tokens_user")
                .table(ApiTokens::Table)
                .col(ApiTokens::UserId)
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(ApiTokens::Table).to_owned()).await
    }
}

#[derive(DeriveIden)]
enum ApiTokens {
    Table,
    Token,
    UserId,
    Label,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
