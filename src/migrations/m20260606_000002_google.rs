use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // Provider-agnostic social logins: one row per (provider, subject), linked to a user. A user
        // can link several providers (google now; twitter/apple/etc. later) with no schema change.
        m.create_table(
            Table::create()
                .table(OauthAccounts::Table)
                .if_not_exists()
                .col(ColumnDef::new(OauthAccounts::Id).integer().not_null().auto_increment().primary_key())
                .col(ColumnDef::new(OauthAccounts::UserId).integer().not_null())
                .col(ColumnDef::new(OauthAccounts::Provider).string().not_null())
                .col(ColumnDef::new(OauthAccounts::Subject).string().not_null())
                .col(ColumnDef::new(OauthAccounts::CreatedAt).timestamp_with_time_zone().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .from(OauthAccounts::Table, OauthAccounts::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
        // One account per (provider, subject) — the identity key we log in by.
        m.create_index(
            Index::create()
                .if_not_exists()
                .name("idx_oauth_provider_subject")
                .table(OauthAccounts::Table)
                .col(OauthAccounts::Provider)
                .col(OauthAccounts::Subject)
                .unique()
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.drop_table(Table::drop().table(OauthAccounts::Table).to_owned()).await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum OauthAccounts {
    Table,
    Id,
    UserId,
    Provider,
    Subject,
    CreatedAt,
}
