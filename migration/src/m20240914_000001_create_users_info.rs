use sea_orm_migration::prelude::*;

const USERNAME_INDEX: &str = "idx_users_info_username";

/// Stores the latest known profile of every Telegram user the assistant has
/// seen, e.g. to resolve `@username`s (which the Bot API cannot do).
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(UsersInfo::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UsersInfo::Id)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(UsersInfo::FirstName).string().not_null())
                    .col(ColumnDef::new(UsersInfo::LastName).string().null())
                    .col(ColumnDef::new(UsersInfo::Username).string().null())
                    .col(
                        ColumnDef::new(UsersInfo::IsBot)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(UsersInfo::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UsersInfo::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(USERNAME_INDEX)
                    .table(UsersInfo::Table)
                    .col(UsersInfo::Username)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UsersInfo::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UsersInfo {
    Table,
    Id,
    FirstName,
    LastName,
    Username,
    IsBot,
    CreatedAt,
    UpdatedAt,
}
