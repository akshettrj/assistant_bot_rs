use sea_orm::entity::prelude::*;

/// An entry awaiting confirmation on its card. Stored so that the card's
/// buttons, which only carry the draft's id, survive restarts.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "drafts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub trip_id: i32,
    pub chat_id: i64,
    /// The card's message, once sent.
    pub message_id: Option<i32>,
    /// The Telegram user the draft belongs to.
    pub created_by: i64,
    /// The draft itself, as JSON.
    pub json: String,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
