use sea_orm_migration::prelude::*;

/// Exporting trips to Splitwise: the group each trip goes to, who each person
/// is on Splitwise, and the Splitwise expense each entry became.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SplitwiseLinks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SplitwiseLinks::TripId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseLinks::GroupId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseLinks::GroupName)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SplitwiseLinks::Currency).string().not_null())
                    .col(
                        ColumnDef::new(SplitwiseLinks::LinkedBy)
                            .big_integer()
                            .not_null(),
                    )
                    .col(timestamp(SplitwiseLinks::LinkedAt))
                    .foreign_key(&mut cascade(
                        SplitwiseLinks::Table,
                        SplitwiseLinks::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        // Telegram users are the same Splitwise user on every trip.
        manager
            .create_table(
                Table::create()
                    .table(SplitwiseUsers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SplitwiseUsers::UserId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseUsers::SplitwiseId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SplitwiseUsers::Name).string().not_null())
                    .to_owned(),
            )
            .await?;

        // People without Telegram, one trip at a time.
        manager
            .create_table(
                Table::create()
                    .table(SplitwiseMembers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SplitwiseMembers::MemberId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseMembers::SplitwiseId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SplitwiseMembers::Name).string().not_null())
                    .foreign_key(&mut cascade(
                        SplitwiseMembers::Table,
                        SplitwiseMembers::MemberId,
                        TripMembers::Table,
                        TripMembers::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SplitwiseExports::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SplitwiseExports::EntryId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseExports::TripId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseExports::GroupId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SplitwiseExports::ExpenseId)
                            .big_integer()
                            .not_null(),
                    )
                    // What was sent, as JSON: an entry whose expense would
                    // differ has changed since.
                    .col(ColumnDef::new(SplitwiseExports::Sent).text().not_null())
                    // Splitwise's `updated_at` after the bot's change: a later
                    // one means someone changed it there.
                    .col(
                        ColumnDef::new(SplitwiseExports::UpdatedAt)
                            .string()
                            .not_null(),
                    )
                    .col(timestamp(SplitwiseExports::ExportedAt))
                    .foreign_key(&mut cascade(
                        SplitwiseExports::Table,
                        SplitwiseExports::EntryId,
                        Entries::Table,
                        Entries::Id,
                    ))
                    .foreign_key(&mut cascade(
                        SplitwiseExports::Table,
                        SplitwiseExports::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("splitwise_exports_trip")
                    .table(SplitwiseExports::Table)
                    .col(SplitwiseExports::TripId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            SplitwiseExports::Table.into_iden(),
            SplitwiseMembers::Table.into_iden(),
            SplitwiseUsers::Table.into_iden(),
            SplitwiseLinks::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(table).to_owned())
                .await?;
        }
        Ok(())
    }
}

fn timestamp(column: impl IntoIden) -> ColumnDef {
    ColumnDef::new(column)
        .timestamp_with_time_zone()
        .not_null()
        .to_owned()
}

/// A foreign key whose rows go with the row they point to.
fn cascade(
    from_table: impl IntoIden,
    from_column: impl IntoIden,
    to_table: impl IntoIden,
    to_column: impl IntoIden,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .from(from_table, from_column)
        .to(to_table, to_column)
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

#[derive(DeriveIden)]
enum Trips {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TripMembers {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Entries {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum SplitwiseLinks {
    Table,
    TripId,
    GroupId,
    GroupName,
    Currency,
    LinkedBy,
    LinkedAt,
}

#[derive(DeriveIden)]
enum SplitwiseUsers {
    Table,
    UserId,
    SplitwiseId,
    Name,
}

#[derive(DeriveIden)]
enum SplitwiseMembers {
    Table,
    MemberId,
    SplitwiseId,
    Name,
}

#[derive(DeriveIden)]
enum SplitwiseExports {
    Table,
    EntryId,
    TripId,
    GroupId,
    ExpenseId,
    Sent,
    UpdatedAt,
    ExportedAt,
}
