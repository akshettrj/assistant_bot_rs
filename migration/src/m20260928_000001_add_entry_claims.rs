use sea_orm_migration::prelude::*;

/// Keeps each entry's claims (what was said about it: items, extras, "the
/// rest"...) so that editing it starts from them, not just from the amounts.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Entries::Table)
                    .add_column(ColumnDef::new(Entries::ClaimsJson).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Entries::Table)
                    .drop_column(Entries::ClaimsJson)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Entries {
    Table,
    ClaimsJson,
}
