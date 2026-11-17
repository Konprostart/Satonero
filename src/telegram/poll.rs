//! The Telegram `getUpdates` long-polling task.
//!
//! Spawned once at startup when `[telegram]` is enabled. It loops forever,
//! long-polling the Bot API and routing inbound text messages through the
//! linking handler. There is no webhook and no public domain requirement.

use crate::config::Settings;
use crate::telegram::api;
use crate::telegram::link::{handle_start, StartOutcome};
use sqlx::SqlitePool;
use std::sync::Arc;

/// Spawn the long-polling task. Returns immediately (does not spawn anything)
/// when `[telegram]` is disabled.
pub async fn start_polling(pool: Arc<SqlitePool>) {
    if !Settings::is_telegram_enabled() {
        return;
    }
    let cfg = match Settings::get_telegram() {
        Some(cfg) => cfg,
        None => return,
    };
    // Telegram caps the long-poll timeout at 50 seconds.
    let timeout_secs = cfg.polling_timeout_seconds.clamp(1, 50);
    let retry_secs = cfg.poll_retry_seconds.max(1);

    tokio::spawn(async move {
        // `offset` acknowledges updates as they are handled: pass
        // `update_id + 1` of the last handled update so Telegram drops it.
        let mut offset: i64 = 0;
        loop {
            match api::get_updates(offset, timeout_secs).await {
                Some(Ok(updates)) => {
                    for update in updates {
                        offset = update.update_id + 1;
                        let Some(message) = update.message else {
                            continue;
                        };
                        let Some(text) = message.text else {
                            continue;
                        };
                        handle_update(&pool, message.chat.id, &text).await;
                    }
                }
                Some(Err(e)) => {
                    tracing::warn!(target: "telegram", "getUpdates failed: {e}; retrying in {retry_secs}s");
                    tokio::time::sleep(std::time::Duration::from_secs(retry_secs)).await;
                }
                None => {
                    // Feature disabled or token vanished after a live
                    // reconfiguration; nothing more to do.
                    tracing::warn!(target: "telegram", "polling stopped: client unavailable");
                    return;
                }
            }
        }
    });
}

/// Route one inbound text message to the linking handler and answer the user.
async fn handle_update(pool: &SqlitePool, chat_id: i64, text: &str) {
    let outcome = match handle_start(pool, chat_id, text).await {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!(target: "telegram", "handle_start failed: {e}");
            return;
        }
    };
    let reply = match outcome {
        StartOutcome::NotAStartCommand => return,
        StartOutcome::MissingCode => {
            "Send /start followed by the code shown in the app.".to_string()
        }
        StartOutcome::InvalidCode => {
            "That code is invalid or already used. Ask for a new one in the app.".to_string()
        }
        StartOutcome::Linked(pubkey) => format!(
            "Linked. Your Mostro identity {pubkey} will now receive order notifications here."
        ),
    };
    if let Err(e) = api::send_message(chat_id, &reply).await {
        tracing::warn!(target: "telegram", "failed to answer chat {chat_id}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::mint_telegram_link_code;
    use nostr_sdk::prelude::Keys;

    async fn test_db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// The poller's update handler is exercised through the pure linking
    /// domain (`handle_start`), which is covered in `link.rs`. This test
    /// guards the DB wiring it depends on end to end: a code minted for an
    /// identity links exactly one chat and is single-use.
    #[tokio::test]
    async fn minted_code_links_once_through_the_db() {
        let pool = test_db().await;
        let pubkey = Keys::generate().public_key().to_string();
        let code = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();

        assert_eq!(
            handle_start(&pool, 1001, &format!("/start {code}"))
                .await
                .unwrap(),
            StartOutcome::Linked(pubkey)
        );
        assert_eq!(
            handle_start(&pool, 1002, &format!("/start {code}"))
                .await
                .unwrap(),
            StartOutcome::InvalidCode
        );
    }
}
