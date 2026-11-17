//! Minimal Telegram Bot API client: `getUpdates` long-polling and
//! `sendMessage`.
//!
//! The client is built once, lazily, from the active [`TelegramSettings`] and
//! cached in a process-wide [`OnceLock`]. The bot token is read from the
//! settings (loaded from the environment at startup by
//! `attach_telegram_bot_token`) and never leaves this process.

use crate::config::types::TelegramSettings;
use secrecy::ExposeSecret;
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

/// The time a `sendMessage` call may take before it is abandoned.
const SEND_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// Headroom added to the long-poll timeout so Telegram's "hold the connection
/// open" behaviour is never cut short by our own client timeout.
const LONG_POLL_HEADROOM: Duration = Duration::from_secs(10);

static TELEGRAM_API: OnceLock<TelegramApi> = OnceLock::new();

/// A single Bot API update (we only care about incoming text messages).
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

/// An incoming Telegram message.
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub text: Option<String>,
}

/// A Telegram chat. The numeric `id` is what links back to
/// `telegram_accounts.chat_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
}

/// The Bot API envelope. `result` is absent on error responses; `description`
/// carries the human-readable reason.
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

/// The shared HTTP client plus the bot identity.
struct TelegramApi {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl TelegramApi {
    /// Build from active settings. `None` when the token has not been loaded
    /// (telegram disabled, or startup failed to attach it).
    fn from_settings(cfg: &TelegramSettings) -> Option<Self> {
        let token = cfg.bot_token.as_ref()?.expose_secret().to_owned();
        let http = reqwest::Client::builder()
            .user_agent(concat!("mostro/", env!("CARGO_PKG_VERSION")))
            .build()
            .ok()?;
        Some(Self {
            http,
            base_url: cfg.api_base_url.trim_end_matches('/').to_owned(),
            token,
        })
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    /// Long-poll for updates. `offset` is the `update_id` of the last handled
    /// update + 1 (or 0 to start from the oldest unconfirmed). `timeout_secs`
    /// is how long Telegram holds the connection open (clamped to 1..=50 by
    /// the caller).
    async fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, String> {
        let response = self
            .http
            .get(self.endpoint("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", timeout_secs.to_string()),
            ])
            .timeout(Duration::from_secs(timeout_secs) + LONG_POLL_HEADROOM)
            .send()
            .await
            .map_err(|e| format!("getUpdates request failed: {e}"))?;

        let body: ApiResponse<Vec<Update>> = response
            .json()
            .await
            .map_err(|e| format!("getUpdates decode failed: {e}"))?;

        if !body.ok {
            return Err(format!(
                "getUpdates returned not ok: {}",
                body.description.unwrap_or_default()
            ));
        }
        Ok(body.result.unwrap_or_default())
    }

    /// Send a text message to a chat.
    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String> {
        let response = self
            .http
            .post(self.endpoint("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "disable_web_page_preview": true,
            }))
            .timeout(SEND_MESSAGE_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("sendMessage request failed: {e}"))?;

        let body: ApiResponse<serde_json::Value> = response
            .json()
            .await
            .map_err(|e| format!("sendMessage decode failed: {e}"))?;

        if !body.ok {
            return Err(format!(
                "sendMessage returned not ok: {}",
                body.description.unwrap_or_default()
            ));
        }
        Ok(())
    }
}

/// Borrow the shared client, building it on first use from the active
/// settings. `None` when the feature is disabled or the token is absent.
fn get_api() -> Option<&'static TelegramApi> {
    if let Some(api) = TELEGRAM_API.get() {
        return Some(api);
    }
    let cfg = crate::config::Settings::get_telegram()?;
    let api = TelegramApi::from_settings(cfg)?;
    // If another task won the build race, keep the existing instance; the two
    // are equivalent because they read the same settings.
    let _ = TELEGRAM_API.set(api);
    TELEGRAM_API.get()
}

/// Long-poll for updates. `None` when the feature is disabled (no client).
pub async fn get_updates(offset: i64, timeout_secs: u64) -> Option<Result<Vec<Update>, String>> {
    let api = get_api()?;
    Some(api.get_updates(offset, timeout_secs).await)
}

/// Send a text message to a chat. `Ok(())` when the feature is disabled (the
/// caller treats that as "nothing to do"), `Err` only for a real send failure.
pub async fn send_message(chat_id: i64, text: &str) -> Result<(), String> {
    match get_api() {
        Some(api) => api.send_message(chat_id, text).await,
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_joins_base_url_token_and_method_without_double_slash() {
        let cfg = TelegramSettings {
            api_base_url: "https://api.telegram.org/".to_string(),
            bot_token: Some(secrecy::SecretString::from("123:abc".to_string())),
            ..Default::default()
        };
        let api = TelegramApi::from_settings(&cfg).expect("token present");
        assert_eq!(
            api.endpoint("getUpdates"),
            "https://api.telegram.org/bot123:abc/getUpdates"
        );
        assert_eq!(api.token, "123:abc");
    }

    #[test]
    fn from_settings_is_none_without_a_token() {
        let cfg = TelegramSettings::default();
        assert!(TelegramApi::from_settings(&cfg).is_none());
    }
}
