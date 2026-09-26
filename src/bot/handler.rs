use std::sync::Arc;

use teloxide::{dptree, types::Update};

use crate::{
    context::AppContext,
    db::repositories::users,
    modules::{ModuleRegistry, UpdateHandler},
};

/// The root of the handler tree: cross-cutting concerns first, then the
/// modules.
pub fn schema(modules: &ModuleRegistry) -> UpdateHandler {
    dptree::entry()
        .inspect_async(track_user)
        .chain(modules.handler())
}

/// Keeps `users_info` up to date with every sender the bot sees.
///
/// Failures are logged but never stop the update from being handled.
async fn track_user(update: Update, ctx: Arc<AppContext>) {
    let Some(user) = update.from() else { return };

    if let Err(error) = users::upsert(&ctx.db, user).await {
        tracing::warn!(%error, user_id = %user.id, "failed to record the user");
    }
}
