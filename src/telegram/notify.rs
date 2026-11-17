//! The Telegram notification sink.
//!
//! `notify_order_message` is the single choke point that turns an outbound
//! order message (the same one `send_dm` is about to deliver over Nostr) into
//! a Telegram notification for the recipient's linked chat. It is called from
//! the message-flush job, right alongside `send_dm`, so every order-status
//! transition already flowing through `queue_order_msg` is covered with no
//! per-transition wiring.
//!
//! The outbound queue addresses the recipient's **trade** key, while linking
//! binds the **master identity** to a chat. The sink therefore resolves the
//! recipient's trade key back to the master identity through the order
//! (`master_buyer_pubkey` / `master_seller_pubkey`), then looks up that
//! master's chat.
//!
//! Security: the message text is derived **only** from the [`Action`] and the
//! order id — never from the message payload — so a hold-invoice bolt11,
//! payment hash, payer history or any other sensitive field can never leak
//! into Telegram. Only a small whitelist of status-transition actions
//! produces a notification; every other action is a no-op.

use crate::config::Settings;
use crate::db::find_telegram_chat_id;
use mostro_core::db::Crud;
use mostro_core::message::Action;
use mostro_core::prelude::Order;
use nostr_sdk::prelude::PublicKey;
use sqlx::SqlitePool;
use uuid::Uuid;

/// Best-effort Telegram notification for an outbound order message.
///
/// Inert when `[telegram]` is disabled, when the recipient has no linked
/// chat, when the action is not a status transition, or when the order cannot
/// be resolved. Failures are logged and swallowed: a Telegram outage must
/// never affect Nostr delivery.
pub async fn notify_order_message(
    pool: &SqlitePool,
    recipient: &PublicKey,
    action: &Action,
    order_id: Option<Uuid>,
) {
    if !Settings::is_telegram_enabled() {
        return;
    }
    let Some(text) = notification_text(action, order_id) else {
        return;
    };
    let Some(identity) = master_identity_for(pool, recipient, order_id).await else {
        return;
    };
    let chat_id = match find_telegram_chat_id(pool, &identity).await {
        Ok(Some(chat_id)) => chat_id,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(target: "telegram", "chat lookup failed: {e}");
            return;
        }
    };
    if let Err(e) = crate::telegram::api::send_message(chat_id, &text).await {
        tracing::warn!(target: "telegram", "failed to notify chat {chat_id}: {e}");
    }
}

/// Resolve the master identity pubkey (hex) that owns the Telegram chat for
/// `recipient`, which is the trade key the outbound queue addresses.
///
/// When the order is present it maps the trade key to the order's master
/// buyer/seller key; without an order (or when the recipient is neither
/// party) the recipient itself is used, which is correct for full-privacy
/// identities whose trade key *is* their master key.
async fn master_identity_for(
    pool: &SqlitePool,
    recipient: &PublicKey,
    order_id: Option<Uuid>,
) -> Option<String> {
    let Some(order_id) = order_id else {
        return Some(recipient.to_string());
    };
    let order = match Order::by_id(pool, order_id).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            tracing::debug!(target: "telegram", "order {order_id} not found; using recipient key");
            return Some(recipient.to_string());
        }
        Err(e) => {
            tracing::warn!(target: "telegram", "order {order_id} lookup failed: {e}");
            return Some(recipient.to_string());
        }
    };

    let recipient = recipient.to_string();
    // Buyer side: trade key or its master identity.
    let is_buyer = order.buyer_pubkey.as_deref() == Some(recipient.as_str())
        || order.master_buyer_pubkey.as_deref() == Some(recipient.as_str());
    // Seller side: trade key or its master identity.
    let is_seller = order.seller_pubkey.as_deref() == Some(recipient.as_str())
        || order.master_seller_pubkey.as_deref() == Some(recipient.as_str());

    if is_buyer {
        order.master_buyer_pubkey.or(order.buyer_pubkey)
    } else if is_seller {
        order.master_seller_pubkey.or(order.seller_pubkey)
    } else {
        Some(recipient)
    }
}

/// A short, human-readable notification for a status transition, or `None`
/// for actions that are not status transitions (and therefore must not be
/// mirrored to Telegram).
fn notification_text(action: &Action, order_id: Option<Uuid>) -> Option<String> {
    let event = match action {
        Action::WaitingSellerToPay => "Waiting for the seller to pay",
        Action::WaitingBuyerInvoice => "Waiting for the buyer's invoice",
        Action::BuyerTookOrder => "Your order was taken",
        Action::BuyerInvoiceAccepted => "Buyer invoice accepted",
        Action::BondInvoiceAccepted => "Bond invoice accepted",
        Action::HoldInvoicePaymentAccepted => "Escrow payment received",
        Action::HoldInvoicePaymentSettled => "Escrow settled",
        Action::HoldInvoicePaymentCanceled => "Hold invoice payment canceled",
        Action::FiatSent => "Fiat payment marked as sent",
        Action::FiatSentOk => "Fiat payment confirmed",
        Action::Released => "Funds released",
        Action::Canceled => "Order canceled",
        Action::DisputeInitiatedByYou => "You opened a dispute",
        Action::DisputeInitiatedByPeer => "The other party opened a dispute",
        Action::CooperativeCancelInitiatedByYou => "You initiated a cooperative cancel",
        Action::CooperativeCancelInitiatedByPeer => {
            "The other party initiated a cooperative cancel"
        }
        Action::CooperativeCancelAccepted => "Cooperative cancel accepted",
        Action::AdminCanceled => "Order canceled by an admin",
        Action::AdminSettled => "Dispute settled by an admin",
        Action::PurchaseCompleted => "Purchase completed",
        Action::BondPayoutCompleted => "Bond payout completed",
        Action::BondSlashed => "Bond slashed",
        Action::CashuEscrowLocked => "Escrow locked",
        _ => return None,
    };
    Some(match order_id {
        Some(id) => format!("{event} — order #{short}", short = short_order_id(id)),
        None => event.to_string(),
    })
}

/// A compact, stable order reference for a chat notification. The full UUID
/// is unwieldy in a push notification; its first 8 characters are unique
/// enough to identify the order while staying readable.
fn short_order_id(id: Uuid) -> String {
    let mut s = id.to_string();
    s.truncate(8);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_transitions_produce_a_notification_with_the_order_ref() {
        let id = Uuid::parse_str("308e1272-d5f4-47e6-bd97-3504baea9c23").unwrap();
        let text = notification_text(&Action::Released, Some(id)).unwrap();
        assert!(text.starts_with("Funds released"));
        assert!(text.contains("308e1272"), "{text}");
    }

    #[test]
    fn non_transition_actions_are_silent() {
        for action in [
            Action::NewOrder,
            Action::PayInvoice,
            Action::AddInvoice,
            Action::Rate,
            Action::UserInfo,
            Action::Orders,
            Action::RequestTelegramCode,
            Action::TelegramCode,
            Action::CantDo,
        ] {
            assert!(
                notification_text(&action, Some(Uuid::new_v4())).is_none(),
                "{action:?} must not be mirrored to Telegram"
            );
        }
    }

    #[test]
    fn transition_without_an_order_is_still_announced() {
        let text = notification_text(&Action::Canceled, None).unwrap();
        assert_eq!(text, "Order canceled");
    }

    #[test]
    fn short_order_id_keeps_the_first_eight_characters() {
        let id = Uuid::parse_str("308e1272-d5f4-47e6-bd97-3504baea9c23").unwrap();
        assert_eq!(short_order_id(id), "308e1272");
    }
}
