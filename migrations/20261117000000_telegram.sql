-- Telegram read-only notifications (v1): which Telegram chat is linked to
-- which Mostro identity, and the one-time codes that perform the linking.

-- One row per linked identity. `pubkey` is the master identity pubkey (hex),
-- the same value the daemon proves for an inbound message via the transport
-- identity proof. `chat_id` is the Telegram chat the code was activated
-- from. `chat_id` is unique so a chat cannot be linked to two identities,
-- and `pubkey` is the primary key so an identity holds exactly one chat.
CREATE TABLE IF NOT EXISTS telegram_accounts (
  pubkey    TEXT PRIMARY KEY NOT NULL,
  chat_id   INTEGER NOT NULL,
  linked_at INTEGER NOT NULL,
  UNIQUE (chat_id)
);

-- One-time linking codes. A code is minted by the daemon on a user request,
-- shown to the user by the app, and activated with `/start <code>`. `used_at`
-- is NULL until activation; activation writes `chat_id` and `used_at` here
-- and upserts the link into `telegram_accounts`.
CREATE TABLE IF NOT EXISTS telegram_link_codes (
  code       TEXT PRIMARY KEY NOT NULL,
  pubkey     TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  chat_id    INTEGER
);

-- The `/start <code>` handler and the request-code handler both look up by
-- pubkey (revoke/replace and duplicate suppression), and notification
-- delivery looks up the chat by pubkey on every transition.
CREATE INDEX IF NOT EXISTS idx_telegram_link_codes_pubkey ON telegram_link_codes (pubkey);
CREATE INDEX IF NOT EXISTS idx_telegram_accounts_chat_id ON telegram_accounts (chat_id);
