use std::sync::Arc;

use futures::future::BoxFuture;
use teloxide::{error_handlers::ErrorHandler, prelude::*};

use crate::{bot::AssistantBot, context::AppContext};

/// Telegram rejects longer messages.
pub const MAX_MESSAGE_CHARS: usize = 4096;

/// Logs handler errors and forwards them to the error logs chat, which is a
/// runtime setting read for each error.
pub struct ErrorReporter {
    bot: AssistantBot,
    ctx: Arc<AppContext>,
}

impl ErrorReporter {
    pub fn new(bot: AssistantBot, ctx: Arc<AppContext>) -> Self {
        Self { bot, ctx }
    }
}

impl ErrorHandler<anyhow::Error> for ErrorReporter {
    fn handle_error(self: Arc<Self>, error: anyhow::Error) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            tracing::error!(error = format!("{error:#}"), "failed to handle an update");

            // Plain text on purpose: the error may contain anything.
            let report = format_report(&error);
            let chat_id = self
                .ctx
                .settings
                .current()
                .config
                .telegram
                .error_logs_chat_id;
            if let Err(send_error) = self.bot.send_message(chat_id, report).await {
                tracing::warn!(error = %send_error, "failed to report the error to Telegram");
            }
        })
    }
}

/// The error with its causes, cut to fit in a single message.
fn format_report(error: &anyhow::Error) -> String {
    let report = format!("⚠️ Failed to handle an update\n\n{error:?}");
    truncate_chars(report, MAX_MESSAGE_CHARS)
}

pub fn truncate_chars(text: String, max: usize) -> String {
    const ELLIPSIS: char = '…';

    if text.chars().count() <= max {
        return text;
    }
    let mut truncated: String = text.chars().take(max - 1).collect();
    truncated.push(ELLIPSIS);
    truncated
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn report_includes_the_causes() {
        let error = Err::<(), _>(anyhow::anyhow!("root cause"))
            .context("outer")
            .unwrap_err();
        let report = format_report(&error);
        assert!(report.contains("outer"), "{report}");
        assert!(report.contains("root cause"), "{report}");
    }

    #[test]
    fn long_reports_are_truncated_on_char_boundaries() {
        let report = format_report(&anyhow::anyhow!("é".repeat(10_000)));
        assert_eq!(report.chars().count(), MAX_MESSAGE_CHARS);
        assert!(report.ends_with('…'));
    }

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate_chars("abc".into(), 3), "abc");
    }
}
