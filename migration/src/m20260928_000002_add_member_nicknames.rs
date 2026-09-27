use sea_orm_migration::prelude::*;

/// Other names members go by ("Rinny" for Erin), as a JSON list.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(TripMembers::Table)
                    .add_column(
                        ColumnDef::new(TripMembers::Nicknames)
                            .text()
                            .not_null()
                            .default("[]"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(TripMembers::Table)
                    .drop_column(TripMembers::Nicknames)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum TripMembers {
    Table,
    Nicknames,
}
