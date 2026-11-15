# Satonero Daemon — Server Deployment Guide

Automated + manual installation of the Satonero daemon (`mostrod`) on an
Ubuntu VPS (Hetzner, 8 GB RAM). Covers **Rust**, **protoc**, **PostgreSQL
(optional)**, the daemon build, configuration, and a systemd service.

> **Important — PostgreSQL is not required.** `mostrod` stores its state in
> **SQLite** (`mostro.db`), not PostgreSQL. The `sqlx` features in `Cargo.toml`
> enable only `sqlite`. The installer still offers PostgreSQL because it was
> requested, but it is **optional and unused by the daemon**. Skip it with
> `INSTALL_POSTGRES=false` unless you need PostgreSQL for something else.

## Table of contents

1. [Prerequisites](#prerequisites)
2. [Quick start (one command)](#quick-start)
3. [What the script does](#what-the-script-does)
4. [Manual installation (step by step)](#manual-installation)
5. [Configuration — `settings.toml`](#configuration)
6. [Nostr identity (nsec / `.env`)](#nostr-identity)
7. [Lightning node requirement (LND vs Cashu)](#lightning-node-requirement)
8. [Managing the systemd service](#managing-the-service)
9. [Verifying it works](#verifying-it-works)
10. [Firewall](#firewall)
11. [Troubleshooting](#troubleshooting)
12. [Security notes](#security-notes)

## Prerequisites

- An Ubuntu VPS (22.04 LTS or 24.04 LTS) with **root** access over SSH.
- **8 GB RAM** is sufficient; the script adds a swap file automatically if none
  exists, so the Rust release build is not OOM-killed.
- Outbound internet (to fetch crates and the `satonero-core` git dependency).
- For **Lightning escrow mode**: a running **LND** node (its `tls.cert` and
  `admin.macaroon`). See [Lightning node requirement](#lightning-node-requirement).
  For **Cashu escrow mode** no LND is needed.

## Quick start

From your laptop, copy the script to the server and run it:

```bash
scp deploy/setup-satonero.sh root@YOUR_SERVER_IP:/root/
ssh root@YOUR_SERVER_IP
sudo bash /root/setup-satonero.sh
```

Or, if the repo is already on the server, run it directly:

```bash
ssh root@YOUR_SERVER_IP
sudo bash /opt/satonero/deploy/setup-satonero.sh
```

Useful environment overrides:

```bash
# Skip PostgreSQL (recommended unless you need it for other purposes):
INSTALL_POSTGRES=false sudo -E bash setup-satonero.sh

# Skip automatic nsec generation (you will set the key manually):
GENERATE_NSEC=false sudo -E bash setup-satonero.sh
```

## What the script does

`deploy/setup-satonero.sh` is idempotent (safe to re-run) and, in order:

1. `apt-get update` and installs build deps: `build-essential`, `cmake`,
   `pkg-config`, `git`, `curl`, `ca-certificates`, `libssl-dev`,
   `libsqlite3-dev`, `sqlite3`, `protobuf-compiler`, `python3`.
2. Optionally installs and enables **PostgreSQL** (unused by `mostrod`).
3. Installs **Rust** via `rustup` (not apt — see Pitfalls).
4. Creates a **4 GB swap file** if none is active (safer release build).
5. Clones `https://github.com/Konprostart/Satonero.git` (branch `main`) into
   `/opt/satonero`.
6. Installs the pinned toolchain (`rust-toolchain.toml`, channel `1.94.0`) and
   runs `cargo build --release --locked --bin mostrod`.
7. Installs the binary to `/usr/local/bin/mostrod`.
8. Creates the system user `mostro` and the config dir `/opt/mostro`, copying
   `settings.toml` from `settings.tpl.toml` if missing.
9. Optionally generates a Nostr `nsec` into `/opt/mostro/.env` (mode 600).
10. Sets ownership/permissions and installs + enables `mostro.service`
    (without starting it).

The service is **enabled but not started**, because `settings.toml` (and LND or
Cashu) must be configured first.

## Manual installation

If you prefer to run each step yourself:

```bash
# 1. Update and install build deps
apt update && apt upgrade -y
apt install -y build-essential cmake pkg-config git curl ca-certificates \
  libssl-dev libsqlite3-dev sqlite3 protobuf-compiler python3

# 2. Install Rust (press 1 when asked; or use the -y non-interactive form)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Clone and build
mkdir -p /opt
git clone --branch main https://github.com/Konprostart/Satonero.git /opt/satonero
cd /opt/satonero
cargo build --release --locked --bin mostrod

# 4. Install the binary
install -m 0755 target/release/mostrod /usr/local/bin/mostrod

# 5. Create the service user and config dir
useradd --system --no-create-home --home-dir /opt/mostro --shell /usr/sbin/nologin mostro
mkdir -p /opt/mostro
cp /opt/satonero/settings.tpl.toml /opt/mostro/settings.toml
chown -R mostro:mostro /opt/mostro
chmod 700 /opt/mostro

# 6. Install the systemd unit
cp /opt/satonero/deploy/mostro.service /etc/systemd/system/mostro.service
systemctl daemon-reload
systemctl enable mostro.service
```

Then continue with [Configuration](#configuration).

## Configuration

Edit `/opt/mostro/settings.toml` before starting. The sections that matter for
a fresh node:

```toml
[nostr]
# Your Nostr private key. Override via MOSTRO_NSEC_PRIVKEY (see below).
nsec_privkey = 'nsec1...'
# At least one relay the daemon publishes to and reads from.
relays = ['wss://relay.example.com']

[lightning]
lnd_cert_file = '/path/to/lnd/tls.cert'
lnd_macaroon_file = '/path/to/lnd/data/chain/bitcoin/mainnet/admin.macaroon'
lnd_grpc_host = 'https://127.0.0.1:10009'

[mostro]
# name = "Satonero"
# about = "Independent Bitcoin/Lightning peer-to-peer marketplace"
fee = 0
fiat_currencies_accepted = ['USD', 'EUR', 'ARS', 'CUP']
```

Key points:

- **`[nostr].relays`** — use `wss://` relays, not `ws://localhost:7000` (that
  default is for the local dev stack in `docker/`).
- **`[lightning].lnd_*`** — absolute paths to the LND TLS cert, the
  `admin.macaroon`, and the LND gRPC endpoint. `mostrod` must be able to read
  them (the `mostro` user needs read access).
- **`[mostro].dev_fee_*`** — see the box below.

> ⚠️ **Dev fee address.** `dev_fee_enabled` is `false` by default (good). If you
> enable it on mainnet, you **must** set
> `dev_fee_lightning_address = 'you@your-node.example.com'` to your own
> Satonero address. The compiled-in fallback in
> `src/config/constants.rs` is a non-routable placeholder (`dev@satonero.example`)
> because Satonero operates no dev-fee fund of its own; leaving it unset while
> `dev_fee_enabled = true` makes payments fail closed instead of funding anyone.

### Cashu mode (no Lightning node)

To run without LND, add/enable the `[cashu]` block:

```toml
[cashu]
enabled = true
# Mints accepted for new orders. Empty list accepts any mint with a public host.
mint_urls = ["https://mint.example.com"]
escrow_locktime_days = 15
```

With `[cashu].enabled = true`, `mostrod` skips LND entirely (see `src/main.rs`
and `docs/cashu/`). The admin gRPC `[rpc]` server is not started in Cashu mode.

## Nostr identity

`mostrod` derives its public key (npub) from the `nsec`. You can provide the
`nsec` in three places, with this precedence:

1. a real process environment variable `MOSTRO_NSEC_PRIVKEY`,
2. `/opt/mostro/.env` (auto-loaded at startup),
3. `settings.toml` `[nostr].nsec_privkey`.

The installer writes `MOSTRO_NSEC_PRIVKEY=nsec1...` into `/opt/mostro/.env`
(mode 600) when `GENERATE_NSEC=true`, which overrides the placeholder in the
template. To generate a key manually:

```bash
# Option A: use the interactive setup wizard (also configures LND + relays)
sudo -u mostro /usr/local/bin/mostrod -d /opt/mostro

# Option B: install a tiny generator and write the key yourself
cargo install nostril
nostril
# then put the nsec into /opt/mostro/.env:
#   MOSTRO_NSEC_PRIVKEY=nsec1...
# and chmod 600 /opt/mostro/.env
```

> **Back up your nsec.** Losing it loses control of the node's Nostr identity
> (and its on-Nostr order history). Keep a copy in a password manager.

## Lightning node requirement

- **Lightning mode (default):** `mostrod` connects to LND at startup
  (`[lightning].lnd_grpc_host`) and exits if it cannot reach it. So LND must be
  running **before** you start the service, and its `tls.cert`/`admin.macaroon`
  must be readable by the `mostro` user.
- **Cashu mode:** set `[cashu].enabled = true` and `mostrod` starts with no LND.

The daemon also guards against switching LND nodes while escrow is open
(`docs/MAINTENANCE_MODE_LN_MIGRATION.md`). A first boot records the LND
identity pubkey; a later different node with open escrow refuses to start
unless `[lightning].allow_node_change = true` (disaster recovery only).

## Managing the service

```bash
systemctl start mostro.service
systemctl status mostro.service
systemctl stop mostro.service
systemctl restart mostro.service

# Follow logs
journalctl -u mostro.service -f
# Last 50 lines without pager
journalctl -u mostro.service -n 50 --no-pager
```

## Verifying it works

1. Check the service is active:

   ```bash
   systemctl status mostro.service
   ```

2. Read the first lines of the log — the daemon prints its **npub** and hex
   pubkey right after the keys load:

   ```bash
   journalctl -u mostro.service -n 50 --no-pager
   ```

3. The database file and WAL appear in `/opt/mostro`:

   ```bash
   ls -l /opt/mostro
   # mostro.db, mostro.db-shm, mostro.db-wal
   ```

4. From another machine, verify the node is publishing its kind-38385 info
   event (see `INSTALL.md` for `nostreq`/`nostcat` example) using your npub.

## Firewall

`mostrod` is an outbound Nostr client (it connects to relays); it does **not**
listen for inbound connections by default. The admin gRPC `[rpc]` server, if
enabled, listens on `127.0.0.1:50051` by default. A minimal `ufw` policy:

```bash
ufw default deny incoming
ufw default allow outgoing
ufw allow OpenSSH
ufw enable
```

Open `50051` **only** if you bind `[rpc].listen_address` to a non-loopback
interface and need remote admin access — and then set `[rpc].auth_token`.

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| `protoc: command not found` | Install `protobuf-compiler` (`apt install -y protobuf-compiler`). `build.rs` needs `protoc` on PATH. |
| Build killed / `signal: 9` (OOM) | 8 GB + no swap. Re-run the script (it adds a 4 GB swap) or add swap manually. |
| `error[E0463]: can't find crate` / fetch failure for `mostro-core` | The `[patch.crates-io]` entry fetches `Konprostart/satonero-core` from git; ensure outbound git/https access to GitHub. |
| `Could not load settings: ...` | Invalid `settings.toml` (e.g. the `nsec1...` placeholder, a bad transport value, or a serbero/pubkey conflict). The error prints the reason as text. |
| Service exits right after start (Lightning mode) | LND unreachable. Check `lnd_grpc_host` + cert/macaroon paths and that LND is up. Or enable Cashu mode. |
| `REFUSING TO START: Lightning node changed ...` | The connected LND differs from the last one with open escrow. Drain first or set `allow_node_change = true` (disaster recovery). |
| First non-interactive run prints "Created settings file from template ..." and exits 0 | Expected when `settings.toml` is missing and stdin is not a TTY (systemd). Pre-create `settings.toml` (the installer does) or run the wizard interactively once. |

## Security notes

- **Do not commit a populated `settings.toml`** — the installer copies the
  template, never secrets.
- `/opt/mostro` is `chmod 700`; `settings.toml` and `.env` are `chmod 600`.
- Back up the `nsec` out of band; rotate it promptly if it leaks.
- Scrub logs before sharing — they can contain order details; avoid leaking
  invoices or keys.
- The service runs as an unprivileged `mostro` system user with
  `shell = /usr/sbin/nologin`.
