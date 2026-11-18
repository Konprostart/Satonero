# Production Hardening Audit — Satonero daemon

Deep, whole-system audit of the Satonero daemon (Mostro fork) targeting production
readiness for 1000+ concurrent users. This document records the issues found,
the fixes applied, the architectural decisions taken, and the remaining risks
that need a follow-up (with concrete recommendations).

Scope: database transactions, Nostr transport, NIP-44, PoW gates, restore flow,
Telegram pairing, the LND gRPC surface, and error/panic handling.

---

## 1. Fixed — SQLite WAL journal mode (CRITICAL)

**Finding.** `db::connect()` built the pool with `SqlitePool::connect(db_url)`,
i.e. sqlx 0.9.0 defaults. sqlx deliberately does **not** set `journal_mode`, so
SQLite runs in the default **rollback-journal** mode. In that mode a writer takes
an exclusive lock that blocks every reader, so the event loop, the scheduler
jobs, the Telegram long-poller and the optional admin gRPC server (all sharing
one pool) serialise and can surface `SQLITE_BUSY` under load.

**Fix.** `db.rs` now builds every connection through
`sqlite_connect_options()`, which sets:

- `journal_mode(SqliteJournalMode::Wal)` — concurrent readers + single writer.
- `busy_timeout(30s)` — a briefly-held write lock is waited out rather than
  failing a user-facing operation (sqlx default is 5 s).
- `create_if_missing(true)` — preserved behaviour.

`foreign_keys` stays ON and `synchronous` stays FULL (a financial daemon must
not trade crash-durability for throughput). WAL conversion is permanent and
safe: SQLite atomically converts an existing rollback-journal database on first
connect.

## 2. Fixed — unbounded LND gRPC awaits (MAJOR)

**Finding.** `fedimint-tonic-lnd` sets no per-RPC deadline. The following calls
were awaited inline from the single-consumer event loop and scheduler jobs with
no `tokio::time::timeout`, so a half-dead LND node (connection accepted, never
answers) would block message processing forever:

- `settle_hold_invoice`
- `cancel_hold_invoice`
- `get_hold_invoice_expiry_height`
- `lookup_payment` / `check_payment_status` (`track_payment_v2` + first stream message)

**Fix.** Added `LND_RPC_TIMEOUT = 30s` in `src/lightning/mod.rs` and wrapped every
one of those calls (including the streaming `stream.message()` waits) in
`tokio::time::timeout`. Timeout maps to a stable `MostroInternalErr(LnNodeError)`
/ `LnPaymentError` carrying the operation name, mirroring the existing
`PAYOUT_INFLIGHT_COUNT_TIMEOUT` / `DUPLICATE_GUARD_LOOKUP_TIMEOUT` pattern.

## 3. Fixed — panic on attacker-supplied Cashu token (MAJOR, defense in depth)

**Finding.** `cashu::verify_escrow_conditions` contained `unreachable!` and
`.expect("…locktime")` on a token that is ultimately attacker-supplied (the
seller's escrow token). They were guarded by `verify_2of3_condition`, but any
gap in that upstream verifier would crash the whole daemon.

**Fix.** Both are now `Err(Error::Condition(…))`, so a malformed token is
rejected instead of panicking.

## 4. Fixed — startup and lock-poison panics (MINOR)

- `main.rs` — `clearscreen::clear().expect(…)` now best-effort (`let _ =`), so a
  non-TTY supervisor (systemd, dumb terminal) cannot abort startup.
- `price/store.rs` — four `.expect("price store lock poisoned")` replaced with
  `unwrap_or_else(|e| e.into_inner())`, so one poisoned lock no longer cascades
  into a panic on every subsequent price read/write.

## 5. Fixed — swallowed payout / DB-write results (MINOR)

- `release.rs` / `admin_settle.rs` — `let _ = do_payment(…)` now logs the failure
  at the call site (the failure is still recorded durably for the retry job, but
  it is no longer silent).
- `scheduler.rs` — `let _ = update_order_to_initial_state(…)` now logs a
  failure instead of claiming "Republishing order" while the reset silently
  failed.

## 6. Fixed — unused-import warning in the Telegram module

`find_telegram_link_code` was imported at module scope but used only in tests;
the import now lives in the test module.

---

## 7. Remaining risks — documented, with recommended follow-ups

These are pre-existing architectural issues in the money-critical paths. They are
**not** fixed here because a correct fix requires end-to-end verification against
a live LND node (the reconcile/retry paths must be exercised), and a wrong reorder
could cause a *worse* failure (paying the buyer out of an un-settled escrow). They
are surfaced loudly in logs via `tracing::error!` so an operator can reconcile.

### 7.1 "Act-then-check" — `release_action` / `admin_settle_action` (HIGH)

`settle_seller_hold_invoice` (an irreversible LND settle) runs **before** the
status CAS `UPDATE orders … WHERE status IN ('fiat-sent','dispute')`. If the CAS
misses (a concurrent admin settle/cancel via the gRPC server, or a scheduler
transition, while the settle awaits), the escrow is released but the order is not
advanced and the buyer payout is skipped.

The CAS-miss path now logs `tracing::error!` with an explicit "escrow settled but
order not advanced — reconcile manually" message.

**Recommended fix (needs LND integration testing):** transition the order with the
guarded CAS *first*, then settle; if the settle then fails, leave the order in
`settled-hold-invoice` and let the existing payout retry/reconciliation path
re-drive the settle. Verify that `do_payment`/`job_retry_failed_payments` never
pay the buyer from a wallet balance the settle has not funded.

### 7.2 `admin_cancel_action` non-transactional, unguarded write (HIGH)

The dispute row (`d.update(pool)`) and the order write (`order_updated.update(pool)`,
a full-row `Order::update` with **no** `WHERE status='dispute'` guard) are two
separate statements. A concurrent release/settle can be clobbered back to
`CanceledByAdmin`, and a crash between the two leaves a half-written cancel.

**Recommended fix:** wrap the dispute update + order write in one transaction and
guard the order write with `WHERE status='dispute'` (mirroring `release` and
`admin_settle`).

### 7.3 Unguarded full-row `.update(pool)` write-backs (MEDIUM)

`fiat_sent_action`, `add_invoice_action` (preimage path), `cancel_order_by_maker`,
and the cooperative-cancel step 1/2 write order rows with unguarded full-row
updates; a concurrent transition can be clobbered. Several are bounded by the
single-consumer event loop (only the gRPC server and scheduler run concurrently),
but each should eventually move to a guarded CAS like the pre-trade transitions
already use (`cas_pretrade_order_status`, `cas_complete_pretrade_take`).

### 7.4 `mint_telegram_link_code` read-before-write (LOW)

The rate gate reads `MAX(created_at)` inside the transaction before the DELETE +
INSERT. Under two concurrent mints for the *same* identity this can surface a
spurious `SQLITE_BUSY` (deferred-transaction snapshot upgrade). It fails closed —
the UNIQUE constraint still guarantees at most one outstanding code, so there is
no double-mint — but the error is ugly. With WAL enabled the window is narrower.

**Recommended fix:** acquire the write lock first (`BEGIN IMMEDIATE`) or move the
rate-gate read to a separate non-transactional statement, accepting the small
TOCTOU (the INSERT's UNIQUE on `code` plus the "delete previous unused codes"
invariant already bound the outcome).

---

## 8. Verified correct (no action needed)

The following were confirmed atomic/race-free during the audit and are listed
for completeness:

- Telegram `activate_telegram_link_code` — write-first guarded `UPDATE … WHERE
  used_at IS NULL AND expires_at >= now`, atomic link + unlink, 1:1 in both
  directions (`pubkey` PK, `chat_id` UNIQUE).
- Pre-trade transitions: `cas_pretrade_order_status`, `cas_promote_taker_context`,
  `cas_complete_pretrade_take`, `cas_rotate_maker_trade_pubkey`.
- Bond lock (`UPDATE … WHERE state='requested' AND NOT EXISTS(other locked taker bond)`),
  cashu escrow lock (`NOT EXISTS` token-uniqueness backstop), payout lifecycle
  (`claim_order_payout`/`payment_success` guarded updates), `rate_user`,
  `dispute::persist_dispute`, payer declarations, reputation import/revoke.
- `spam_gate.rs` — replay guard records only signature-verified event ids;
  poisoned locks degrade to the safe direction (fail-open).
- `telegram/poll.rs` long-poll loop — bounded by explicit `getUpdates`/`sendMessage`
  timeouts and a fixed retry delay.
- No `unsafe`, no indexing panic on network/short input in runtime code.

## 9. Verification

```sh
cargo check --bin mostrod        # clean (exit 0)
cargo test --bin mostrod telegram # see PR/commit output
cargo fmt --all
cargo clippy --bin mostrod --all-targets
```
