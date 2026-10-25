use sea_orm_migration::{prelude::*, sea_orm::DatabaseBackend};

/// `settings.updated_by` becomes text: botconf 0.2 records who made a change
/// as a string (not every platform's user ids are numbers).
///
/// On SQLite, botconf still reads the old `bigint` column (it casts it to
/// text), but such a column keeps every numeric-looking actor, i.e. every
/// Telegram user id, as an INTEGER. Postgres rejects text in a `bigint`
/// column outright. SQLite cannot change a column's type, so the table is
/// rebuilt there.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => rebuild(manager, ColumnType::Text, "TEXT").await,
            _ => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "ALTER TABLE settings ALTER COLUMN updated_by TYPE text USING \
                         updated_by::text",
                    )
                    .await?;
                Ok(())
            }
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => rebuild(manager, ColumnType::BigInteger, "INTEGER").await,
            _ => {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "ALTER TABLE settings ALTER COLUMN updated_by TYPE bigint USING \
                         updated_by::bigint",
                    )
                    .await?;
                Ok(())
            }
        }
    }
}

/// Recreates `settings` with `updated_by` of type `actor`, casting the
/// stored actors to `cast` (an SQLite type name).
async fn rebuild(manager: &SchemaManager<'_>, actor: ColumnType, cast: &str) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(SettingsNew::Table)
                .col(
                    ColumnDef::new(Settings::Key)
                        .string()
                        .not_null()
                        .primary_key(),
                )
                .col(ColumnDef::new(Settings::Value).text().not_null())
                .col(ColumnDef::new_with_type(Settings::UpdatedBy, actor).null())
                .col(
                    ColumnDef::new(Settings::UpdatedAt)
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .to_owned(),
        )
        .await?;
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "INSERT INTO settings_new (key, value, updated_by, updated_at) SELECT key, value, \
             CAST(updated_by AS {cast}), updated_at FROM settings"
        ))
        .await?;
    manager
        .drop_table(Table::drop().table(Settings::Table).to_owned())
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(SettingsNew::Table, Settings::Table)
                .to_owned(),
        )
        .await
}

#[derive(DeriveIden)]
enum Settings {
    Table,
    Key,
    Value,
    UpdatedBy,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SettingsNew {
    Table,
}
