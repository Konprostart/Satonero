# Satonero / Mostro — Multi-User Deployment Guide

> A complete, technically precise guide for choosing between **someone else's (public) infrastructure** and **your own server**, with step-by-step instructions for standing up your own instance.
>
> Sources: the `Satonero` repo (`settings.tpl.toml`, `Cargo.toml`, `docker/`, `deploy/`, `INSTALL.md`, `docs/LIGHTNING_OPS.md`). Daemon version: **mostrod v0.20.2** (Rust 1.94.0, GPL-3.0-or-later).

---

## 0. Architecture in 30 seconds (what you are actually running)

```text
Mobile app (Satonero-mobile)
        │  Nostr (kind-38383 order, kind-14 NIP-44 encrypted DM)
        ▼
Nostr relay(s)   ←→   Satonero daemon (mostrod)
                            │  gRPC (fedimint-tonic-lnd)
                            ▼
                    Lightning node (LND)   OR   Cashu mint (NUT-11)
```

- The **daemon (`mostrod`)** is the **trade coordinator**: it does not hold user bitcoin in a custodial wallet in the classic sense; it holds **escrow** via **hold-invoices** (Lightning mode) or **2-of-3 NUT-11 P2PK tokens** (Cashu mode).
- The **relay** is only transport — the daemon and clients exchange encrypted Nostr events over it.
- The **Lightning node (LND)** is what actually moves sats in Lightning mode.

Therefore "running Satonero for multiple users" means: **one daemon + one (or a couple of) reliable relays + one liquid LND** (or a Cashu mint as the no-LND alternative).

---

## 1. Comparative analysis — Option 1 vs Option 2

### 1.1 Summary table

| Criterion | Option 1 — someone else's gear (public Mostro daemon) | Option 2 — your own VPS (Satonero) |
|---|---|---|
| **Control** | None (their daemon, their rules, their fee) | Full (your `settings.toml`, fee, rules, moderation) |
| **User privacy** | Depends on an operator you do not know; your users trust an **unknown** third party | You are the operator; retention policy (`dm_days`, `order_days`) is in your hands |
| **Brand/identity** | Someone else's node npub and name | Your own npub, name, `about`, avatar (kind-0 metadata) |
| **Revenue** | 0 (goes to the other operator) | `[mostro].fee` + optional dev-fee → you |
| **Costs** | 0 € | VPS (~5–25 €/month) + LND liquidity (opportunity cost) |
| **Reliability** | Depends on someone else's uptime (not yours to control) | You control it (monitoring, restart, watchdog, backup) |
| **Scalability (more users)** | One shared daemon; you share capacity with everyone else | One daemon easily handles hundreds of orders; bottlenecks are **relay** and **LND liquidity/HTLC slots** — both solved by you |
| **Legal/regulatory risk** | Borne by an unknown operator; you do not know their jurisdiction | You are aware of, and control, your risk (KYB is not technically required, but the fiat ramp is your choice) |
| **Technical complexity** | Near zero (just paste an npub into the app) | Medium (SSH, build, systemd/Docker, LND) |
| **Dependence on the Mostro ecosystem** | High (you use a Mostro node) | Low (independent fork, your own identity) |

### 1.2 Option 1 — using someone else's gear

**What this actually means:** in the app you connect to a **public Mostro daemon** (e.g. the default `82fa8cb9…` = Mostro's own node, or one of the `trustedCommunities` — Cuba, Spain, Argentina…), over **public relays** (`wss://relay.damus.io`, `wss://relay.mostro.network`, `wss://nos.lol`…).

**Advantages:**

- Zero cost and zero maintenance.
- Works immediately — a user just picks a community/node and trades.
- Existing liquidity and user base of other nodes.

**Disadvantages:**

- **No control whatsoever** over rules, fees, moderation, disputes.
- **Privacy:** your users trust an operator you do not know; you have no idea what they log.
- **No revenue** for you (dev-fee and fee go to the other operator).
- **Vanishing risk:** the node can be shut down, change rules, or be compromised.
- **Brand:** you trade "under someone else's flag" — you are not building your own community/reputation.

### 1.3 Option 2 — your own VPS (Satonero)

**What this actually means:** you run your own `mostrod` on a VPS, with your own npub, your own relay (optional) and your own LND (or Cashu mint).

**Advantages:**

- **Full control and independence** (the whole point of the Satonero fork).
- **Privacy:** you define retention (`dm_days`, `order_days`, `rating_days`, `dispute_days`, `fee_audit_days`).
- **Revenue:** `[mostro].fee` (e.g. `0.01` = 1%) and optional dev-fee (`dev_fee_enabled = true` → the Satonero dev address).
- **Scalability for multiple users:** one daemon is lightweight (Rust + SQLite); your bottlenecks are the relay and LND, which you scale yourself.
- **Your own brand** and reputation; later you can add `[reputation_import]`/`[reputation_export]` for reputation portability.

**Disadvantages:**

- Technical responsibility (24/7 operations, backup, security).
- Cost of the VPS + LND liquidity.
- Legal responsibility for what is traded through your node.

---

## 2. Recommendation (for "more users are coming soon")

> **Recommend Option 2 — your own VPS (Satonero), with your own relay and your own LND.**

**Why:**

1. **Control and privacy are strategic values** — they are the reason you forked Mostro in the first place. A public Mostro node negates them.
2. **More users = a business, not a hobby.** With your own node you choose the fee, moderation, disputes and retention; with someone else's you are just a free channel funding someone else's node.
3. **Scalability is solvable and cheap:** `mostrod` is lightweight, and the bottlenecks (relay + LND) are controllable. One Hetzner VPS (which you already have) covers **hundreds of users** without difficulty.
4. **Independence from Mostro** is already done in code (forks `satonero-core`, `dart-nip44`); infrastructure is the last step of that independence.

**When Option 1 would be justified:** only as a temporary "trial" step before you stand up your own node — never as a permanent solution for a growing community.

---

## 3. Option 2 — complete procedure (VPS over SSH)

### 3.1 Hardware and software — exactly what you need

| Component | Recommendation | Note |
|---|---|---|
| **VPS** | Hetzner CX32/CX42 (or equivalent) | CX32: 4 vCPU / 8 GB / 80 GB NVMe (~10 €/month) |
| **OS** | Ubuntu 22.04 LTS or 24.04 LTS | x86_64 |
| **RAM** | 8 GB | needed for `cargo build --release` (build needs ~4 GB; the installer adds swap) |
| **Disk** | 80 GB NVMe | `mostrod` + SQLite are tiny; **LND + bitcoind (if you run it) is what eats disk** |
| **Rust** | 1.94.0 (pinned in `rust-toolchain.toml`) | only via **rustup**, not apt |
| **Build deps** | `cmake build-essential libsqlite3-dev libssl-dev pkg-config protobuf-compiler git curl` | `protoc` is mandatory |
| **Database** | **SQLite** (not PostgreSQL!) | `[database] url = "sqlite://mostro.db"` |
| **Lightning** | **LND** (the only supported option) | see §5 |
| **No-LND alternative** | Cashu mint (NUT-11) | `[cashu] enabled = true` |

### 3.2 Ports

| Service | Port | Direction | Expose to whom |
|---|---|---|---|
| SSH | 22 | in | only you (with an IP allowlist or fail2ban) |
| LND gRPC | 10009 | in (local) | **do not expose publicly** — `127.0.0.1` / localhost only |
| LND REST | 8080 | in (local) | do not expose publicly |
| LND P2P | 9735 | in+out | **public** (to have inbound channels) |
| `mostrod` admin gRPC | 50051 | in (local) | default `127.0.0.1`; if you expose it → set `[rpc].auth_token` |
| nostr-rs-relay | 8080 (container) | in | behind a reverse proxy on 80/443 (wss) |
| strfry (relay alternative) | 7777 | in | behind a reverse proxy |

**Key rule:** `mostrod` is an **outbound-only** Nostr client — it listens on nothing public. The only things that must be publicly reachable are the **LND P2P port (9735)** and the **relay (80/443)**.

### 3.3 Step-by-step SSH install (automated)

The repository already has an **idempotent** installer — the fastest and most reliable path:

```bash
# From your laptop:
scp deploy/setup-satonero.sh root@YOUR_IP:/root/
ssh root@YOUR_IP

# On the server (without PostgreSQL — the daemon does not use it):
INSTALL_POSTGRES=false bash /root/setup-satonero.sh
```

The script does: apt update → build deps → rustup (1.94.0) → swap (4 GB) → clone `Konprostart/Satonero` into `/opt/satonero` → `cargo build --release --locked --bin mostrod` → install to `/usr/local/bin/mostrod` → `mostro` user + `/opt/mostro` + `settings.toml` → **generate an `nsec`** in `/opt/mostro/.env` (chmod 600) → systemd `mostro.service` (**enabled, but not started**).

After the install:

```bash
# 1. Edit the configuration
nano /opt/mostro/settings.toml

# 2. Start and follow the logs (you see your npub right after key loading)
systemctl start mostro
journalctl -u mostro -n 50 --no-pager -f

# 3. Check the status
systemctl status mostro
```

### 3.4 Production stack with Docker Compose (daemon + relay + TLS reverse proxy)

Ready-to-use files live in [`deploy/vps-production/`](../deploy/vps-production/): a `docker-compose.yml` that runs **Satonero daemon + your own relay + Caddy (automatic HTTPS/wss)**. LND is reached over gRPC (on the same host, or remotely).

```bash
# On the VPS, after cloning the repo:
cd deploy/vps-production
mkdir -p config/lnd config/relay/data config/caddy

cp settings.example.toml config/settings.toml && nano config/settings.toml
cp relay_config.toml            config/relay/config.toml
cp Caddyfile                    config/caddy/Caddyfile && nano config/caddy/Caddyfile

# LND files (so they appear at /config/lnd/ in the container):
cp /path/to/lnd/tls.cert     config/lnd/tls.cert
cp /path/to/lnd/data/chain/bitcoin/mainnet/admin.macaroon config/lnd/admin.macaroon

docker compose up -d
docker compose logs -f satonero
```

In `config/settings.toml` (paths are **inside the container**):

```toml
[lightning]
lnd_cert_file = '/config/lnd/tls.cert'
lnd_macaroon_file = '/config/lnd/admin.macaroon'
lnd_grpc_host = 'https://host.docker.internal:10009'   # LND on the same host
# lnd_grpc_host = 'https://your-lnd-host:10009'         # LND on another host

[nostr]
nsec_privkey = 'nsec1...'                 # your key (or env MOSTRO_NSEC_PRIVKEY)
relays = ['wss://relay.satonero.com']     # your relay + optionally public ones

[database]
url = "sqlite:///config/mostro.db"

[mostro]
name = "Satonero"
about = "Independent Bitcoin/Lightning peer-to-peer marketplace"
fee = 0                                    # 0.01 = 1% trading fee
fiat_currencies_accepted = ['EUR', 'RSD', 'BAM', 'HRK', 'USD', 'ARS', 'CUP']
```

> **Note on the image:** `ghcr.io/konprostart/satonero` currently publishes `dev-<short-sha>` tags; `:latest` does not exist yet. Until a release tag is published, building from the repo (`build:` in the compose file) is the most reliable. For a manual (non-Compose) VPS install, see `deploy/README.md` and `docker/README.md`.

---

## 4. Choosing a Nostr relay

### 4.1 Public relays (fastest, but someone else's)

Recommended, proven-stable ones (the repo itself uses them in examples):

- `wss://relay.damus.io`
- `wss://relay.mostro.network`
- `wss://nos.lol`
- `wss://relay.nostr.band`

**Rule:** always list **2–3** relays in `[nostr].relays` (the daemon publishes to all and reads from all — redundancy). Do not rely on a single one.

**Downsides of public relays:** you do not control uptime, rate-limiting (NIP-40/PoW), or whether they block the event kinds you need.

### 4.2 Your own relay (recommended for a growing community)

Two proven options:

| Relay | Language | Database | Comment |
|---|---|---|---|
| **nostr-rs-relay** (`scsibug/nostr-rs-relay`) | Rust | SQLite/Postgres | **Already in the repo** (`docker/relay_config.toml` + `docker/compose.yml`). Easiest path — in the compose file above. |
| **strfry** | C++ | LMDB | Excellent for **lots** of events/subscriptions; `--db` + `--no-nest`; often the choice of large relays. |

**Recommendation:** start with **nostr-rs-relay** (already integrated in the repo, minimal configuration), and move to **strfry** only if you measure it becoming a bottleneck (a large number of subscriptions).

**If you really want strfry** (instead of nostr-rs-relay), add it to the compose file:

```yaml
  relay:
    image: ghcr.io/hoytech/strfry:latest
    container_name: satonero-strfry
    restart: unless-stopped
    volumes:
      - ./config/strfry:/app/strfry-db
    ports:
      - "7777:7777"
    command: ["strfry", "relay", "--db=/app/strfry-db/strfry.db"]
```

and in `[nostr].relays` put `wss://relay.satonero.com` (Caddy reverse proxy → `relay:7777`).

**Critical for your own relay:** use **`wss://`** (TLS) — the app and daemon expect wss, not bare `ws://`. Caddy from the compose file handles this automatically.

---

## 5. Choosing the Lightning node — LND vs Core Lightning

### 5.1 Answer: LND, mandatory (CLN is not supported)

Verified in the code (`Cargo.toml` → `fedimint-tonic-lnd`; `src/lightning/`; README: *"Lightning Network node (LND required)"*):

- **The Satonero/Mostro daemon supports ONLY LND**, over gRPC (`fedimint-tonic-lnd`).
- **Core Lightning (CLN), Eclair, LDK — not supported.** There is no CLN connector in the code.

So "LND vs CLN" is not a choice here — **LND is the only integration**. (If you really want CLN, you would have to write a new connector in `src/lightning/`, which is outside the scope of the current integration.)

### 5.2 How LND is integrated

The daemon connects with **three parameters** from `[lightning]`:

```toml
[lightning]
lnd_cert_file     = '/path/to/lnd/tls.cert'
lnd_macaroon_file = '/path/to/lnd/data/chain/bitcoin/mainnet/admin.macaroon'
lnd_grpc_host     = 'https://127.0.0.1:10009'
```

- `tls.cert` — the LND TLS certificate (authenticates the gRPC connection).
- `admin.macaroon` — the **admin** macaroon (the daemon creates hold-invoices, pays, reads status). Do not use a read-only macaroon.
- `lnd_grpc_host` — the LND gRPC endpoint.

**Prerequisite:** LND must be **started before** `mostrod` — in Lightning mode the daemon **crashes if it cannot reach LND**. (There is also a boot guard: the daemon refuses to start if the LND identity changes while escrow is open — `allow_node_change` only for disaster recovery.)

### 5.3 How to run LND (backend options)

LND needs Bitcoin data. Three realistic options:

| Backend | Disk | Complexity | Recommendation |
|---|---|---|---|
| **bitcoind (full/pruned)** | 600 GB+ (full) or ~10 GB (pruned) | Medium | Best privacy and reliability; recommended for production |
| **Neutrino** (no full node) | Small | Low | Quick to stand up, but slower sync and weaker privacy |
| **Hosted LND** (Voltage, Voltage Cloud, Alby Hub, LNbits) | 0 | Lowest | Fastest start; but custodial/cloud — less control |

**To start with multiple users** I recommend: **LND + pruned bitcoind** on the VPS (or a second VPS) — full control, reasonable disk.

Minimal outline (LND on the same machine):

```bash
# bitcoind (pruned) + lnd
bitcoind -daemon -prune=10000 -txindex=0
lnd --bitcoin.mainnet --bitcoin.node=bitcoind --rpclisten=127.0.0.1:10009
```

Then in `settings.toml` point at `tls.cert`, `admin.macaroon` and `https://127.0.0.1:10009`.

### 5.4 Liquidity (key for multiple users)

LND is the **bottleneck** with multiple users, because escrow holds sats in hold-invoices while a trade is live. Set in `[lightning]`:

```toml
max_inflight_payouts = 100               # total in-flight HTLCs
max_inflight_payouts_per_destination = 10   # per destination (anti-abuse)
```

- **Open good channels** to large, liquid peers (maximize inbound so buyers can pay, and outbound so sellers receive).
- In `[mostro]` set `max_order_amount` and `min_payment_amount` to bound your exposure.
- Watch the `[health]` section (auto maintenance mode if LND is out of sync).

### 5.5 No-LND alternative — Cashu mode

If you do not want LND at all, Satonero supports **Cashu escrow** (2-of-3 NUT-11 P2PK), without LND:

```toml
[cashu]
enabled = true
mint_urls = ["https://mint.example.com"]   # empty [] = any public mint
escrow_locktime_days = 15
```

Note: in Cashu mode the admin gRPC `[rpc]` does not start, and the escrow cryptography goes through the mint — good for a start, but LND mode gives the classic Lightning behaviour users expect.

---

## 6. Configuration — the most important parts of `settings.toml`

```toml
[lightning]
lnd_cert_file = '...'
lnd_macaroon_file = '...'
lnd_grpc_host = 'https://127.0.0.1:10009'

[nostr]
nsec_privkey = 'nsec1...'                       # or MOSTRO_NSEC_PRIVKEY env
relays = ['wss://relay.satonero.com', 'wss://relay.damus.io']

[mostro]
name = "Satonero"
about = "Independent Bitcoin/Lightning peer-to-peer marketplace"
fee = 0                                          # 0.01 = 1%
max_routing_fee = 0.002
max_order_amount = 1000000                       # 1M sat = order ceiling
min_payment_amount = 100
expiration_hours = 24
fiat_currencies_accepted = ['EUR','RSD','BAM','HRK','USD','ARS','CUP']
pow = 0                                          # PoW anti-spam (raise later)
# pow_first_contact = 16                         # stricter PoW for first contact
dev_fee_percentage = 0.30
dev_fee_enabled = false                          # opt-in; pays to the Satonero dev address
# dev_fee_lightning_address = 'you@your-node.com'

[database]
url = "sqlite://mostro.db"

[expiration]
order_days = 30
rating_days = 90
dispute_days = 90
fee_audit_days = 365
dm_days = 30

[rpc]
enabled = false                                  # enable for admin; then set auth_token
listen_address = "127.0.0.1"
port = 50051
# auth_token = ""
```

**nsec precedence:** env `MOSTRO_NSEC_PRIVKEY` > `/opt/mostro/.env` > `settings.toml`.

---

## 7. Security and operations (checklist)

- **Back up the `nsec`** off the server (password manager). Losing the nsec = losing the node identity.
- `/opt/mostro` = `chmod 700`; `settings.toml` and `.env` = `chmod 600`; the service runs as the **unprivileged** `mostro` user.
- **Firewall (ufw):** `default deny incoming`, `default allow outgoing`, allow only `OpenSSH` (22) + LND P2P (9735) + 80/443. **Never** expose 10009 (LND gRPC) or 50051 (admin gRPC) without `auth_token`/an IP allowlist.
- **fail2ban** for SSH.
- **Monitoring:** `journalctl -u mostro -f`; `[health]` publishes a kind-38389 status event + optional `watchdog_pubkey` for encrypted detailed metrics.
- **Dev-fee:** default **off** (good). If you enable it, it pays to the Satonero address — be aware before doing so on mainnet.
- **Do not commit `settings.toml`** with secrets; use env or `.env` (600).
- **Disk:** after the build, `cargo clean` frees ~2 GB (if you did not use the Docker multi-stage build).

---

## 8. Scalability — what to do as the user count grows

1. **One daemon is enough for a large number of users** (lightweight Rust + SQLite). The daemon is not the first thing to scale — the **relay** and **LND** are.
2. **Relay:** start with 1 of your own + 2 public; add strfry/another relay when you see subscription slowdown.
3. **LND liquidity:** increase the number/size of channels and tune `max_inflight_payouts*`; open more peers.
4. **Horizontal (later):** you can run **multiple Satonero nodes** (each with its own npub and relay) as separate communities — the architecture is decentralized by design, with no central database.
5. **Watchdog:** `[health].watchdog_pubkey` for a central view over multiple nodes.

---

## 9. TL;DR decision

| Question | Answer |
|---|---|
| Someone else's gear or your own VPS? | **Your own VPS (Option 2)** — for control, privacy and revenue as more users arrive |
| Server | Hetzner CX32/CX42, Ubuntu 22/24, 8 GB RAM, 80 GB NVMe |
| Database | **SQLite** (not PostgreSQL) |
| Lightning node | **LND** (the only supported one; CLN is not) |
| Relay | your own **nostr-rs-relay** (already in the repo) + 1–2 public for redundancy |
| Without LND? | Yes — **Cashu mode** (`[cashu] enabled = true`) |
| Install | `deploy/setup-satonero.sh` (automated) or `deploy/vps-production/docker-compose.yml` |
| Operator fee | `[mostro].fee` (default 0; e.g. 0.01 = 1%) + optional dev-fee |
| Public ports | 22 (SSH), 9735 (LND P2P), 80/443 (relay wss) — **everything else local** |
