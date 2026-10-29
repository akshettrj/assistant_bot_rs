use sea_orm_migration::prelude::*;

/// The handles of settings-panel buttons whose action does not fit
/// Telegram's 64 bytes of callback data, so that they keep working after a
/// restart (botconf 0.4's hybrid button tokens).
///
/// The same table as `botconf_panel::create_tokens_table("panel_tokens")`:
/// `key` (the handle), `action` (the button's encoded action) and
/// `expires_at` (milliseconds since the Unix epoch). It is written out here
/// so that the migrations do not depend on the panel crate.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PanelTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PanelTokens::Key)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PanelTokens::Action).text().not_null())
                    .col(
                        ColumnDef::new(PanelTokens::ExpiresAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PanelTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PanelTokens {
    Table,
    Key,
    Action,
    ExpiresAt,
}
