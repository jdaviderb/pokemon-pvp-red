use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        // Has the user picked their trainer name yet? Default TRUE so existing accounts are untouched;
        // new OAuth (Google) users are created with FALSE and must choose one via the name-entry.
        m.alter_table(
            Table::alter()
                .table(Users::Table)
                .add_column_if_not_exists(
                    ColumnDef::new(Users::NameChosen).boolean().not_null().default(true),
                )
                .to_owned(),
        )
        .await?;
        Ok(())
    }

    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.alter_table(Table::alter().table(Users::Table).drop_column(Users::NameChosen).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    NameChosen,
}
