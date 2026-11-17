//! Telegram account linking: one-time codes and their activation.
//!
//! A user asks the node for a code with [`Action::RequestTelegramCode`]
//! (handled by [`request_telegram_code_action`]); the node mints a
//! single-use, expiring code bound to the proven identity and replies with
//! [`Action::TelegramCode`]. The user then sends `/start <code>` to the bot,
//! and [`handle_start`] activates it — linking the Telegram chat to that
//! identity. This is the only inbound command the bot accepts.

use crate::app::context::AppContext;
use crate::config::Settings;
use crate::config::MESSAGE_QUEUES;
use crate::db::{activate_telegram_link_code, find_telegram_link_code, mint_telegram_link_code};
use mostro_core::error::CantDoReason;
use mostro_core::error::MostroError;
use mostro_core::error::MostroError::*;
use mostro_core::message::Action;
use mostro_core::message::Message;
use mostro_core::message::MessageKind;
use mostro_core::message::Payload;
use mostro_core::transport::UnwrappedMessage;
use nostr_sdk::prelude::Keys;
use sqlx::SqlitePool;

/// Handles `Action::RequestTelegramCode`: mint a one-time linking code for the
/// proven identity and reply with it (`Action::TelegramCode`).
///
/// Refused with `InvalidAction` when the `[telegram]` feature is disabled.
/// The reply is queued for the scheduler (addressed to the requesting trade
/// key) rather than sent inline, so a slow relay never holds the event loop.
pub async fn request_telegram_code_action(
    ctx: &AppContext,
    msg: Message,
    event: &UnwrappedMessage,
    _my_keys: &Keys,
) -> Result<(), MostroError> {
    if !Settings::is_telegram_enabled() {
        return Err(MostroCantDo(CantDoReason::InvalidAction));
    }

    let ttl = Settings::get_telegram()
        .map(|c| c.link_code_ttl_seconds)
        .unwrap_or(900);
    let code = mint_telegram_link_code(ctx.pool(), event.identity.to_string(), ttl).await?;

    let reply = Message::Restore(MessageKind::new(
        None,
        msg.get_inner_message_kind().request_id,
        None,
        Action::TelegramCode,
        Some(Payload::TextMessage(code)),
    ));
    MESSAGE_QUEUES
        .queue_restore_session_msg
        .write()
        .await
        .push((reply, event.sender));
    Ok(())
}

/// The outcome of handling one inbound Telegram text message.
#[derive(Debug, PartialEq, Eq)]
pub enum StartOutcome {
    /// The message was not a `/start` command; the poller ignores it.
    NotAStartCommand,
    /// `/start` without a code (or with extra whitespace-only text).
    MissingCode,
    /// The code is unknown, already used, or expired.
    InvalidCode,
    /// The code activated and linked the chat to `0` (the identity pubkey).
    Linked(String),
}

/// Handle an inbound text message from `chat_id`.
///
/// Only `/start <code>` is acted on; every other message (including other
/// commands) returns [`StartOutcome::NotAStartCommand`] and is ignored. The
/// activation is atomic (`activate_telegram_link_code`), so a code can be
/// used exactly once.
pub async fn handle_start(
    pool: &SqlitePool,
    chat_id: i64,
    text: &str,
) -> Result<StartOutcome, MostroError> {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("/start") else {
        return Ok(StartOutcome::NotAStartCommand);
    };
    // `/start` exactly (no code) or `/start@SomeBot` style mention.
    let code = rest.trim();
    if code.is_empty() || code.starts_with('@') {
        return Ok(StartOutcome::MissingCode);
    }

    match activate_telegram_link_code(pool, code, chat_id).await? {
        Some(pubkey) => Ok(StartOutcome::Linked(pubkey)),
        None => Ok(StartOutcome::InvalidCode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::context::test_utils::{test_settings, TestContextBuilder};
    use nostr_sdk::prelude::{Keys, Timestamp};

    fn create_test_keys() -> Keys {
        Keys::generate()
    }

    async fn test_db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn test_ctx(pool: SqlitePool) -> AppContext {
        let _ = crate::config::MOSTRO_CONFIG.set(test_settings());
        TestContextBuilder::new()
            .with_pool(std::sync::Arc::new(pool))
            .with_settings(test_settings())
            .build()
    }

    #[tokio::test]
    async fn request_is_refused_when_telegram_is_disabled() {
        let ctx = test_ctx(test_db().await);
        let identity = create_test_keys();
        let event = UnwrappedMessage {
            message: Message::Restore(MessageKind::new(
                None,
                Some(7),
                None,
                Action::RequestTelegramCode,
                None,
            )),
            signature: None,
            sender: identity.public_key(),
            identity: identity.public_key(),
            created_at: Timestamp::now(),
        };

        let result =
            request_telegram_code_action(&ctx, event.message.clone(), &event, &create_test_keys())
                .await;

        assert_eq!(result, Err(MostroCantDo(CantDoReason::InvalidAction)));
    }

    #[tokio::test]
    async fn start_links_a_chat_and_can_only_be_used_once() {
        let pool = test_db().await;
        let pubkey = create_test_keys().public_key().to_string();
        let code = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();

        // First use links the chat.
        assert_eq!(
            handle_start(&pool, 42, &format!("/start {code}"))
                .await
                .unwrap(),
            StartOutcome::Linked(pubkey.clone())
        );

        // Second use of the same code is refused.
        assert_eq!(
            handle_start(&pool, 43, &format!("/start {code}"))
                .await
                .unwrap(),
            StartOutcome::InvalidCode
        );
    }

    #[tokio::test]
    async fn mint_refuses_repeat_requests_within_the_interval() {
        let pool = test_db().await;
        let pubkey = create_test_keys().public_key().to_string();

        // The first mint succeeds and leaves one outstanding code.
        let first = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();
        assert!(find_telegram_link_code(&pool, &first)
            .await
            .unwrap()
            .is_some());

        // An immediate re-mint is rate limited while the code is still unused.
        let err = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap_err();
        assert_eq!(err, MostroCantDo(CantDoReason::TooManyRequests));

        // Once the code is used, the gate opens again for a legitimate re-link.
        assert_eq!(
            handle_start(&pool, 42, &format!("/start {first}"))
                .await
                .unwrap(),
            StartOutcome::Linked(pubkey.clone())
        );
        let second = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn mint_revokes_a_previous_unused_code() {
        let pool = test_db().await;
        let pubkey = create_test_keys().public_key().to_string();

        let first = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();
        assert!(find_telegram_link_code(&pool, &first)
            .await
            .unwrap()
            .is_some());

        // Age the first code past the rate-limit window so a re-mint is
        // allowed, then mint again.
        sqlx::query("UPDATE telegram_link_codes SET created_at = created_at - 100 WHERE code = ?1")
            .bind(&first)
            .execute(&pool)
            .await
            .unwrap();

        let second = mint_telegram_link_code(&pool, pubkey.clone(), 900)
            .await
            .unwrap();
        assert_ne!(first, second);

        // The previous unused code is gone; exactly the new one is outstanding.
        assert!(find_telegram_link_code(&pool, &first)
            .await
            .unwrap()
            .is_none());
        assert!(find_telegram_link_code(&pool, &second)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn start_parsing_variants() {
        let pool = test_db().await;
        // Not a /start command.
        assert_eq!(
            handle_start(&pool, 1, "/help").await.unwrap(),
            StartOutcome::NotAStartCommand
        );
        assert_eq!(
            handle_start(&pool, 1, "hello").await.unwrap(),
            StartOutcome::NotAStartCommand
        );
        // /start without a code.
        assert_eq!(
            handle_start(&pool, 1, "/start").await.unwrap(),
            StartOutcome::MissingCode
        );
        assert_eq!(
            handle_start(&pool, 1, "  /start  ").await.unwrap(),
            StartOutcome::MissingCode
        );
        // Unknown code.
        assert_eq!(
            handle_start(&pool, 1, "/start DOESNOTEXIST").await.unwrap(),
            StartOutcome::InvalidCode
        );
    }
}
