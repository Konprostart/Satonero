use std::collections::HashSet;
use std::process::ExitCode;
use std::time::Duration;

use mostro_core::message::{Action, Message, Payload};
use mostro_core::order::Status;
use mostro_core::transport::unwrap_incoming;
use nostr_sdk::prelude::*;
use uuid::Uuid;

const DEFAULT_TIMEOUT_SECONDS: u64 = 15;
const MAX_TIMEOUT_SECONDS: u64 = 120;
const MAX_EVENTS: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExpectedAction {
    BuyerTookOrder,
    HoldInvoicePaymentAccepted,
}

impl ExpectedAction {
    fn parse(value: &str) -> Result<Self, ()> {
        match value {
            "buyer-took-order" => Ok(Self::BuyerTookOrder),
            "hold-invoice-payment-accepted" => Ok(Self::HoldInvoicePaymentAccepted),
            _ => Err(()),
        }
    }

    fn action(self) -> Action {
        match self {
            Self::BuyerTookOrder => Action::BuyerTookOrder,
            Self::HoldInvoicePaymentAccepted => Action::HoldInvoicePaymentAccepted,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::BuyerTookOrder => "buyer-took-order",
            Self::HoldInvoicePaymentAccepted => "hold-invoice-payment-accepted",
        }
    }
}

struct Config {
    relay: RelayUrl,
    daemon: PublicKey,
    recipient_keys: Keys,
    order_id: Uuid,
    since: u64,
    timeout: Duration,
    expected: ExpectedAction,
}

impl Config {
    fn parse<I>(args: I) -> Result<Self, ()>
    where
        I: IntoIterator<Item = String>,
    {
        let mut args = args.into_iter();
        let mut relay = None;
        let mut daemon = None;
        let mut recipient_secret_file = None;
        let mut order_id = None;
        let mut since = None;
        let mut timeout_seconds = None;
        let mut expected = None;

        while let Some(flag) = args.next() {
            let value = next_value(&mut args)?;
            match flag.as_str() {
                "--relay" => relay = Some(RelayUrl::parse(&value).map_err(|_| ())?),
                "--daemon" => daemon = Some(PublicKey::parse(&value).map_err(|_| ())?),
                "--recipient-secret-file" => recipient_secret_file = Some(value),
                "--order-id" => order_id = Some(Uuid::parse_str(&value).map_err(|_| ())?),
                "--since" => since = Some(value.parse::<u64>().map_err(|_| ())?),
                "--timeout-seconds" => {
                    timeout_seconds = Some(value.parse::<u64>().map_err(|_| ())?)
                }
                "--expect" => {
                    if expected.is_some() {
                        return Err(());
                    }
                    expected = Some(ExpectedAction::parse(&value)?);
                }
                _ => return Err(()),
            }
        }

        let timeout_seconds = timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(());
        }

        let secret = std::fs::read_to_string(recipient_secret_file.ok_or(())?).map_err(|_| ())?;
        let recipient_keys = Keys::parse(secret.trim()).map_err(|_| ())?;

        Ok(Self {
            relay: relay.ok_or(())?,
            daemon: daemon.ok_or(())?,
            recipient_keys,
            order_id: order_id.ok_or(())?,
            since: since.ok_or(())?,
            timeout: Duration::from_secs(timeout_seconds),
            expected: expected.ok_or(())?,
        })
    }
}

fn next_value<I>(args: &mut I) -> Result<String, ()>
where
    I: Iterator<Item = String>,
{
    args.next().ok_or(())
}

#[derive(Default, Clone, Copy)]
struct Counts {
    events_total: usize,
    event_ids_unique: usize,
    duplicate_event_ids: usize,
    event_invalid: usize,
    unwrap_none: usize,
    unwrap_errors: usize,
    message_invalid: usize,
    action_mismatch: usize,
    order_mismatch: usize,
    status_mismatch: usize,
    expected_matches: usize,
}

impl Counts {
    fn invalid_total(self) -> usize {
        self.event_invalid
            + self.unwrap_none
            + self.unwrap_errors
            + self.message_invalid
            + self.order_mismatch
            + self.status_mismatch
    }

    fn ok(self) -> bool {
        self.expected_matches == 1 && self.invalid_total() == 0
    }
}

struct Summary {
    config_valid: bool,
    fetch_ok: bool,
    expected: ExpectedAction,
    counts: Counts,
}

impl Summary {
    fn ok(&self) -> bool {
        self.config_valid && self.fetch_ok && self.counts.ok()
    }
}

fn build_filter(config: &Config) -> Filter {
    Filter::new()
        .kind(Kind::PrivateDirectMessage)
        .author(config.daemon)
        .pubkey(config.recipient_keys.public_key())
        .since(Timestamp::from(config.since))
        .limit(MAX_EVENTS)
}

fn targets_recipient(event: &Event, recipient: PublicKey) -> bool {
    let expected = recipient.to_hex();
    let mut found = false;

    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        if parts.first().map(String::as_str) == Some("p") {
            if found
                || parts.len() != 2
                || parts.get(1).map(String::as_str) != Some(expected.as_str())
            {
                return false;
            }
            found = true;
        }
    }

    found
}

async fn classify_events(events: Vec<Event>, config: &Config) -> Counts {
    let mut counts = Counts::default();
    let mut seen = HashSet::new();

    for event in events {
        counts.events_total += 1;

        if !seen.insert(event.id) {
            counts.duplicate_event_ids += 1;
            continue;
        }
        counts.event_ids_unique += 1;

        if event.kind != Kind::PrivateDirectMessage
            || event.pubkey != config.daemon
            || event.verify().is_err()
            || !targets_recipient(&event, config.recipient_keys.public_key())
        {
            counts.event_invalid += 1;
            continue;
        }

        match unwrap_incoming(&event, &config.recipient_keys).await {
            Ok(Some(unwrapped)) => {
                if !matches!(&unwrapped.message, Message::Order(_)) || !unwrapped.message.verify() {
                    counts.message_invalid += 1;
                    continue;
                }

                let kind = unwrapped.message.get_inner_message_kind();
                if kind.action != config.expected.action() {
                    counts.action_mismatch += 1;
                    continue;
                }
                if kind.id != Some(config.order_id) {
                    counts.order_mismatch += 1;
                    continue;
                }

                match &kind.payload {
                    Some(Payload::Order(order)) => {
                        if order.id != Some(config.order_id) {
                            counts.order_mismatch += 1;
                            continue;
                        }
                        if order.status != Some(Status::Active) {
                            counts.status_mismatch += 1;
                            continue;
                        }
                        counts.expected_matches += 1;
                    }
                    _ => counts.message_invalid += 1,
                }
            }
            Ok(None) => counts.unwrap_none += 1,
            Err(_) => counts.unwrap_errors += 1,
        }
    }

    counts
}

async fn collect_stream_events<S, E>(mut stream: S, max_events: usize) -> Result<Vec<Event>, ()>
where
    S: futures::Stream<Item = (RelayUrl, Result<Event, E>)> + Unpin,
{
    let mut events = Vec::new();
    let mut seen = HashSet::new();

    while let Some((_, result)) = stream.next().await {
        let event = result.map_err(|_| ())?;
        if seen.insert(event.id) {
            if events.len() >= max_events {
                return Err(());
            }
            events.push(event);
        }
    }

    Ok(events)
}

async fn query(config: &Config) -> Result<Counts, ()> {
    let client = Client::default();
    client
        .add_relay(config.relay.clone())
        .await
        .map_err(|_| ())?;
    client.connect().and_wait(config.timeout).await;

    let relay = client
        .relay(config.relay.clone())
        .await
        .map_err(|_| ())?
        .ok_or(())?;
    if relay.status() != RelayStatus::Connected {
        return Err(());
    }

    let events = tokio::time::timeout(config.timeout, async {
        let stream = client
            .stream_events(build_filter(config))
            .await
            .map_err(|_| ())?;
        collect_stream_events(stream, MAX_EVENTS).await
    })
    .await
    .map_err(|_| ())??;

    Ok(classify_events(events, config).await)
}

fn print_counts(counts: &Counts) {
    println!("dm_events_total={}", counts.events_total);
    println!("dm_event_ids_unique={}", counts.event_ids_unique);
    println!("dm_duplicate_event_ids={}", counts.duplicate_event_ids);
    println!("dm_event_invalid={}", counts.event_invalid);
    println!("dm_unwrap_none={}", counts.unwrap_none);
    println!("dm_unwrap_errors={}", counts.unwrap_errors);
    println!("dm_message_invalid={}", counts.message_invalid);
    println!("dm_action_mismatch={}", counts.action_mismatch);
    println!("dm_order_mismatch={}", counts.order_mismatch);
    println!("dm_status_mismatch={}", counts.status_mismatch);
    println!("dm_expected_matches={}", counts.expected_matches);
}

fn print_summary(summary: &Summary) {
    println!("dm_ok={}", u8::from(summary.ok()));
    println!("dm_config_valid={}", u8::from(summary.config_valid));
    println!("dm_fetch_ok={}", u8::from(summary.fetch_ok));
    print_counts(&summary.counts);
    println!(
        "{}={}",
        summary.expected.label(),
        summary.counts.expected_matches
    );
}

fn print_config_failure() {
    println!("dm_ok=0");
    println!("dm_config_valid=0");
    println!("dm_fetch_ok=0");
    print_counts(&Counts::default());
}

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(()) => {
            print_config_failure();
            return ExitCode::from(2);
        }
    };

    let (fetch_ok, counts) = match query(&config).await {
        Ok(counts) => (true, counts),
        Err(()) => (false, Counts::default()),
    };
    let summary = Summary {
        config_valid: true,
        fetch_ok,
        expected: config.expected,
        counts,
    };
    print_summary(&summary);

    if summary.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mostro_core::order::{Kind as OrderKind, SmallOrder};
    use mostro_core::transport::{wrap_message_nip44, WrapOptions};
    use nostr_sdk::prelude::*;

    use super::*;

    fn test_order(id: Option<Uuid>, status: Status) -> SmallOrder {
        SmallOrder::new(
            id,
            Some(OrderKind::Sell),
            Some(status),
            100,
            "usd".to_string(),
            None,
            None,
            10,
            "cash".to_string(),
            0,
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn test_message(kind_id: Uuid, payload_id: Uuid, action: Action, status: Status) -> Message {
        Message::new_order(
            Some(kind_id),
            Some(1),
            Some(1),
            action,
            Some(Payload::Order(test_order(Some(payload_id), status))),
        )
    }

    fn wrapped(message: &Message, daemon: &Keys, recipient: &Keys) -> Event {
        wrap_message_nip44(
            message,
            daemon,
            daemon,
            recipient.public_key(),
            WrapOptions::default(),
        )
        .expect("wrap message")
    }

    #[tokio::test]
    async fn rejects_stream_error_after_event_without_partial_success() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(order_id, order_id, Action::BuyerTookOrder, Status::Active),
            &daemon,
            &recipient,
        );
        let relay = RelayUrl::parse("ws://127.0.0.1:1").expect("relay url");
        let stream = futures::stream::iter(vec![(relay.clone(), Ok(event)), (relay, Err(()))]);

        let result = collect_stream_events(stream, MAX_EVENTS).await;

        assert!(result.is_err());
    }

    fn test_config(
        daemon: PublicKey,
        recipient: Keys,
        order_id: Uuid,
        expected: ExpectedAction,
    ) -> Config {
        Config {
            relay: RelayUrl::parse("ws://127.0.0.1:1").expect("relay url"),
            daemon,
            recipient_keys: recipient,
            order_id,
            since: 0,
            timeout: Duration::from_secs(1),
            expected,
        }
    }

    #[test]
    fn filter_uses_expected_shape() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let filter = build_filter(&config);

        assert_eq!(
            filter.kinds,
            Some(BTreeSet::from([Kind::PrivateDirectMessage]))
        );
        assert_eq!(filter.authors, Some(BTreeSet::from([daemon.public_key()])));
        assert_eq!(filter.since, Some(Timestamp::from(0)));
        assert_eq!(filter.limit, Some(MAX_EVENTS));
    }

    #[tokio::test]
    async fn accepts_one_active_expected_message() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(order_id, order_id, Action::BuyerTookOrder, Status::Active),
            &daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event], &config).await;

        assert_eq!(counts.events_total, 1);
        assert_eq!(counts.event_ids_unique, 1);
        assert_eq!(counts.expected_matches, 1);
        assert!(counts.ok());
    }

    #[tokio::test]
    async fn rejects_missing_match() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(Vec::new(), &config).await;

        assert_eq!(counts.expected_matches, 0);
        assert!(!counts.ok());
    }

    #[tokio::test]
    async fn rejects_wrong_action() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(
                order_id,
                order_id,
                Action::HoldInvoicePaymentAccepted,
                Status::Active,
            ),
            &daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event], &config).await;

        assert_eq!(counts.action_mismatch, 1);
        assert_eq!(counts.expected_matches, 0);
        assert!(!counts.ok());
    }

    #[tokio::test]
    async fn rejects_wrong_order_id() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(
                order_id,
                Uuid::new_v4(),
                Action::BuyerTookOrder,
                Status::Active,
            ),
            &daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event], &config).await;

        assert_eq!(counts.order_mismatch, 1);
        assert_eq!(counts.expected_matches, 0);
        assert!(!counts.ok());
    }

    #[tokio::test]
    async fn rejects_non_active_status() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(order_id, order_id, Action::BuyerTookOrder, Status::FiatSent),
            &daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event], &config).await;

        assert_eq!(counts.status_mismatch, 1);
        assert_eq!(counts.expected_matches, 0);
        assert!(!counts.ok());
    }

    #[tokio::test]
    async fn deduplicates_identical_event_ids() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(order_id, order_id, Action::BuyerTookOrder, Status::Active),
            &daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event.clone(), event], &config).await;

        assert_eq!(counts.events_total, 2);
        assert_eq!(counts.event_ids_unique, 1);
        assert_eq!(counts.duplicate_event_ids, 1);
        assert_eq!(counts.expected_matches, 1);
        assert!(counts.ok());
    }

    #[tokio::test]
    async fn rejects_duplicate_matching_events() {
        let daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let message = test_message(order_id, order_id, Action::BuyerTookOrder, Status::Active);
        let first = wrapped(&message, &daemon, &recipient);
        let second = wrapped(&message, &daemon, &recipient);
        assert_ne!(first.id, second.id);
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![first, second], &config).await;

        assert_eq!(counts.expected_matches, 2);
        assert!(!counts.ok());
    }

    #[tokio::test]
    async fn rejects_event_from_unexpected_author() {
        let daemon = Keys::generate();
        let other_daemon = Keys::generate();
        let recipient = Keys::generate();
        let order_id = Uuid::new_v4();
        let event = wrapped(
            &test_message(order_id, order_id, Action::BuyerTookOrder, Status::Active),
            &other_daemon,
            &recipient,
        );
        let config = test_config(
            daemon.public_key(),
            recipient,
            order_id,
            ExpectedAction::BuyerTookOrder,
        );

        let counts = classify_events(vec![event], &config).await;

        assert_eq!(counts.event_invalid, 1);
        assert_eq!(counts.expected_matches, 0);
        assert!(!counts.ok());
    }
}
