//! Telegram read-only notifications (v1).
//!
//! The feature is deliberately **read-only on the trade side**: the bot
//! notifies users about buy/sell/escrow order transitions, and the only thing
//! it accepts back is the one-time `/start <code>` command that links a
//! Telegram chat to a Mostro identity. There are no release/cancel/inline
//! buttons and no other trade commands, so a compromised Telegram account
//! cannot move funds — worst case it stops receiving (or forwards) its own
//! notifications.
//!
//! Layout:
//!
//! * [`api`] — a minimal Telegram Bot API client (`getUpdates` long-polling
//!   and `sendMessage`).
//! * [`link`] — minting and activating the one-time linking codes, plus the
//!   `Action::RequestTelegramCode` message handler.
//! * [`notify`] — the notification sink that turns an outbound order message
//!   into a Telegram message for the recipient's linked chat.
//! * [`poll`] — the long-running `getUpdates` poller task.
//!
//! Every entry point is gated on [`Settings::is_telegram_enabled`]
//! (`crate::config::Settings`), so an absent or disabled `[telegram]` block
//! keeps the whole module inert — the daemon behaves exactly as before.

pub mod api;
pub mod link;
pub mod notify;
pub mod poll;

pub use link::request_telegram_code_action;
pub use notify::notify_order_message;
pub use poll::start_polling;
