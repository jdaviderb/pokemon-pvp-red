use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // Generic runtime feature flags (key -> enabled). Seeded with defaults at boot in flags.rs.
        m.create_table(
            Table::create()
                .table(FeatureFlags::Table)
                .if_not_exists()
                .col(ColumnDef::new(FeatureFlags::Key).string().not_null().primary_key())
                .col(ColumnDef::new(FeatureFlags::Enabled).boolean().not_null().default(false))
                .col(ColumnDef::new(FeatureFlags::UpdatedAt).timestamp_with_time_zone().not_null())
                .to_owned(),
        )
        .await?;

        // Mark disposable guest accounts so they can be filtered / purged later.
        m.alter_table(
            Table::alter()
                .table(Users::Table)
                .add_column_if_not_exists(
                    ColumnDef::new(Users::IsGuest).boolean().not_null().default(false),
                )
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(Table::alter().table(Users::Table).drop_column(Users::IsGuest).to_owned())
            .await?;
        m.drop_table(Table::drop().table(FeatureFlags::Table).to_owned()).await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum FeatureFlags {
    Table,
    Key,
    Enabled,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum Users {
    Table,
    IsGuest,
}
