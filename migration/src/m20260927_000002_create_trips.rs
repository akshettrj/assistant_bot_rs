use sea_orm_migration::prelude::*;

/// The tables of the trips module: trips and their members, fixed exchange
/// rates, the active trip of each chat, the ledger (entries, who paid, who
/// owes, and the history of changes) and the drafts awaiting confirmation.
///
/// Amounts and rates are exact decimal strings: SQLite has no exact decimal
/// type.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Trips::Table)
                    .if_not_exists()
                    .col(id(Trips::Id))
                    .col(ColumnDef::new(Trips::HomeChatId).big_integer().not_null())
                    .col(ColumnDef::new(Trips::Name).string().not_null())
                    .col(ColumnDef::new(Trips::BaseCurrency).string().not_null())
                    .col(ColumnDef::new(Trips::Status).string().not_null())
                    .col(ColumnDef::new(Trips::CreatedBy).big_integer().not_null())
                    .col(timestamp(Trips::CreatedAt))
                    .col(
                        ColumnDef::new(Trips::EndedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TripMembers::Table)
                    .if_not_exists()
                    .col(id(TripMembers::Id))
                    .col(ColumnDef::new(TripMembers::TripId).integer().not_null())
                    .col(ColumnDef::new(TripMembers::Name).string().not_null())
                    .col(ColumnDef::new(TripMembers::UserId).big_integer().null())
                    .col(timestamp(TripMembers::CreatedAt))
                    .foreign_key(&mut cascade(
                        TripMembers::Table,
                        TripMembers::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;
        unique_index(
            manager,
            TripMembers::Table,
            "trip_members_name",
            [TripMembers::TripId, TripMembers::Name],
        )
        .await?;
        // Members without a Telegram account have no user id: NULLs are
        // distinct in unique indexes, on SQLite and Postgres alike.
        unique_index(
            manager,
            TripMembers::Table,
            "trip_members_user",
            [TripMembers::TripId, TripMembers::UserId],
        )
        .await?;

        manager
            .create_table(
                Table::create()
                    .table(TripRates::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(TripRates::TripId).integer().not_null())
                    .col(ColumnDef::new(TripRates::Currency).string().not_null())
                    .col(ColumnDef::new(TripRates::Rate).string().not_null())
                    .primary_key(
                        Index::create()
                            .col(TripRates::TripId)
                            .col(TripRates::Currency),
                    )
                    .foreign_key(&mut cascade(
                        TripRates::Table,
                        TripRates::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ActiveTrips::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActiveTrips::ChatId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ActiveTrips::TripId).integer().not_null())
                    .foreign_key(&mut cascade(
                        ActiveTrips::Table,
                        ActiveTrips::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Entries::Table)
                    .if_not_exists()
                    .col(id(Entries::Id))
                    .col(ColumnDef::new(Entries::TripId).integer().not_null())
                    .col(ColumnDef::new(Entries::Kind).string().not_null())
                    .col(ColumnDef::new(Entries::Description).string().not_null())
                    .col(ColumnDef::new(Entries::Category).string().not_null())
                    .col(ColumnDef::new(Entries::Currency).string().not_null())
                    .col(ColumnDef::new(Entries::Total).string().not_null())
                    .col(ColumnDef::new(Entries::Rate).string().not_null())
                    .col(ColumnDef::new(Entries::RateSource).string().not_null())
                    .col(ColumnDef::new(Entries::BaseTotal).string().not_null())
                    .col(ColumnDef::new(Entries::SpentOn).date().not_null())
                    .col(ColumnDef::new(Entries::SplitMethod).string().not_null())
                    .col(ColumnDef::new(Entries::Origin).string().not_null())
                    .col(ColumnDef::new(Entries::CreatedBy).big_integer().not_null())
                    .col(timestamp(Entries::CreatedAt))
                    .col(timestamp(Entries::UpdatedAt))
                    .col(
                        ColumnDef::new(Entries::DeletedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Entries::DeletedBy).big_integer().null())
                    .foreign_key(&mut cascade(
                        Entries::Table,
                        Entries::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("entries_trip")
                    .table(Entries::Table)
                    .col(Entries::TripId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EntryPayers::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(EntryPayers::EntryId).integer().not_null())
                    .col(ColumnDef::new(EntryPayers::MemberId).integer().not_null())
                    // In the entry's currency, then in the trip's.
                    .col(ColumnDef::new(EntryPayers::Amount).string().not_null())
                    .col(ColumnDef::new(EntryPayers::BaseAmount).string().not_null())
                    .primary_key(
                        Index::create()
                            .col(EntryPayers::EntryId)
                            .col(EntryPayers::MemberId),
                    )
                    .foreign_key(&mut cascade(
                        EntryPayers::Table,
                        EntryPayers::EntryId,
                        Entries::Table,
                        Entries::Id,
                    ))
                    .foreign_key(&mut restrict(
                        EntryPayers::Table,
                        EntryPayers::MemberId,
                        TripMembers::Table,
                        TripMembers::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EntryShares::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(EntryShares::EntryId).integer().not_null())
                    .col(ColumnDef::new(EntryShares::MemberId).integer().not_null())
                    // The split's input: a weight (equal, shares) or an exact
                    // amount in the entry's currency.
                    .col(ColumnDef::new(EntryShares::Weight).string().null())
                    .col(ColumnDef::new(EntryShares::Exact).string().null())
                    .col(ColumnDef::new(EntryShares::BaseAmount).string().not_null())
                    .primary_key(
                        Index::create()
                            .col(EntryShares::EntryId)
                            .col(EntryShares::MemberId),
                    )
                    .foreign_key(&mut cascade(
                        EntryShares::Table,
                        EntryShares::EntryId,
                        Entries::Table,
                        Entries::Id,
                    ))
                    .foreign_key(&mut restrict(
                        EntryShares::Table,
                        EntryShares::MemberId,
                        TripMembers::Table,
                        TripMembers::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EntryHistory::Table)
                    .if_not_exists()
                    .col(id(EntryHistory::Id))
                    .col(ColumnDef::new(EntryHistory::EntryId).integer().not_null())
                    .col(ColumnDef::new(EntryHistory::Action).string().not_null())
                    .col(ColumnDef::new(EntryHistory::By).big_integer().not_null())
                    .col(timestamp(EntryHistory::At))
                    .col(ColumnDef::new(EntryHistory::BeforeJson).text().null())
                    .foreign_key(&mut cascade(
                        EntryHistory::Table,
                        EntryHistory::EntryId,
                        Entries::Table,
                        Entries::Id,
                    ))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Drafts::Table)
                    .if_not_exists()
                    .col(id(Drafts::Id))
                    .col(ColumnDef::new(Drafts::TripId).integer().not_null())
                    .col(ColumnDef::new(Drafts::ChatId).big_integer().not_null())
                    .col(ColumnDef::new(Drafts::MessageId).integer().null())
                    .col(ColumnDef::new(Drafts::CreatedBy).big_integer().not_null())
                    .col(ColumnDef::new(Drafts::Json).text().not_null())
                    .col(timestamp(Drafts::CreatedAt))
                    .col(timestamp(Drafts::ExpiresAt))
                    .foreign_key(&mut cascade(
                        Drafts::Table,
                        Drafts::TripId,
                        Trips::Table,
                        Trips::Id,
                    ))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            Drafts::Table.into_iden(),
            EntryHistory::Table.into_iden(),
            EntryShares::Table.into_iden(),
            EntryPayers::Table.into_iden(),
            Entries::Table.into_iden(),
            ActiveTrips::Table.into_iden(),
            TripRates::Table.into_iden(),
            TripMembers::Table.into_iden(),
            Trips::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(table).to_owned())
                .await?;
        }
        Ok(())
    }
}

fn id(column: impl IntoIden) -> ColumnDef {
    ColumnDef::new(column)
        .integer()
        .not_null()
        .auto_increment()
        .primary_key()
        .to_owned()
}

fn timestamp(column: impl IntoIden) -> ColumnDef {
    ColumnDef::new(column)
        .timestamp_with_time_zone()
        .not_null()
        .to_owned()
}

/// A foreign key that keeps the row it points to from being deleted: a member
/// who paid or owes stays on the trip.
fn restrict(
    from_table: impl IntoIden,
    from_column: impl IntoIden,
    to_table: impl IntoIden,
    to_column: impl IntoIden,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .from(from_table, from_column)
        .to(to_table, to_column)
        .on_delete(ForeignKeyAction::Restrict)
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

async fn unique_index<const N: usize>(
    manager: &SchemaManager<'_>,
    table: impl IntoIden,
    name: &str,
    columns: [impl IntoIden; N],
) -> Result<(), DbErr> {
    let mut index = Index::create();
    index.name(name).table(table).unique();
    for column in columns {
        index.col(column);
    }
    manager.create_index(index.to_owned()).await
}

#[derive(DeriveIden)]
enum Trips {
    Table,
    Id,
    HomeChatId,
    Name,
    BaseCurrency,
    Status,
    CreatedBy,
    CreatedAt,
    EndedAt,
}

#[derive(DeriveIden)]
enum TripMembers {
    Table,
    Id,
    TripId,
    Name,
    UserId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum TripRates {
    Table,
    TripId,
    Currency,
    Rate,
}

#[derive(DeriveIden)]
enum ActiveTrips {
    Table,
    ChatId,
    TripId,
}

#[derive(DeriveIden)]
enum Entries {
    Table,
    Id,
    TripId,
    Kind,
    Description,
    Category,
    Currency,
    Total,
    Rate,
    RateSource,
    BaseTotal,
    SpentOn,
    SplitMethod,
    Origin,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
    DeletedBy,
}

#[derive(DeriveIden)]
enum EntryPayers {
    Table,
    EntryId,
    MemberId,
    Amount,
    BaseAmount,
}

#[derive(DeriveIden)]
enum EntryShares {
    Table,
    EntryId,
    MemberId,
    Weight,
    Exact,
    BaseAmount,
}

#[derive(DeriveIden)]
enum EntryHistory {
    Table,
    Id,
    EntryId,
    Action,
    By,
    At,
    BeforeJson,
}

#[derive(DeriveIden)]
enum Drafts {
    Table,
    Id,
    TripId,
    ChatId,
    MessageId,
    CreatedBy,
    Json,
    CreatedAt,
    ExpiresAt,
}
