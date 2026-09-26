use sea_orm_migration::prelude::*;

/// Stores the latest known title of the groups and channels the assistant has
/// seen, so that settings can show names rather than chat ids.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ChatsInfo::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ChatsInfo::Id)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ChatsInfo::Title).string().null())
                    .col(ColumnDef::new(ChatsInfo::Username).string().null())
                    .col(
                        ColumnDef::new(ChatsInfo::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ChatsInfo::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ChatsInfo::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ChatsInfo {
    Table,
    Id,
    Title,
    Username,
    CreatedAt,
    UpdatedAt,
}
