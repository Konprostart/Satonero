#!/usr/bin/env bash
#
# Satonero quick-start — Cashu mode, NO domain, public relays.
#
# This is the simplest way to run a Satonero node for TESTING when you do not
# own a domain yet. It starts ONE Docker container (the daemon only):
#   - Cashu escrow mode (no LND, no Lightning node needed).
#   - Publishes to public relays (nos.lol + relay.damus.io) — the same relays
#     the mobile app already uses, so the app finds your node without any
#     self-hosted relay, TLS certificate or DNS name.
#
# What this script does NOT do (on purpose):
#   - no self-hosted Nostr relay / Caddy (needs a domain for wss/TLS),
#   - no LND / Lightning node,
#   - no PostgreSQL (the daemon uses SQLite).
#
# Usage (on a fresh Ubuntu VPS, as root):
#   curl -fsSL https://raw.githubusercontent.com/Konprostart/Satonero/main/deploy/quickstart.sh -o quickstart.sh
#   sudo bash quickstart.sh
#
# Re-running the script is safe (idempotent): it updates the repo, regenerates
# config, and recreates the container.
#
# First run builds the Rust daemon inside Docker — allow 10-20 minutes.

set -euo pipefail

REPO_URL="https://github.com/Konprostart/Satonero.git"
REPO_DIR="${HOME}/satonero"
APP_DIR="${HOME}/satonero-node"

log()  { printf '\n\033[1;34m[%s]\033[0m %s\n' "$(date +%T)" "$*"; }
warn() { printf '\n\033[1;33mNOTE:\033[0m %s\n' "$*"; }
die()  { printf '\n\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. Prerequisites
# ---------------------------------------------------------------------------
if [ "$(id -u)" -ne 0 ]; then
  die "run this as root: sudo bash quickstart.sh"
fi

MEM_MB="$(awk '/MemTotal/{print int($2/1024)}' /proc/meminfo)"
if [ "$MEM_MB" -lt 3000 ]; then
  warn "less than 3 GB RAM detected (${MEM_MB} MB). The release build may fail; 8 GB is recommended."
fi

if ! command -v docker >/dev/null 2>&1; then
  log "Installing Docker (official script)..."
  curl -fsSL https://get.docker.com | sh
  systemctl enable --now docker
else
  log "Docker already installed."
fi
if ! docker compose version >/dev/null 2>&1; then
  die "docker compose plugin is missing"
fi

# Basic tools (git/curl/python3); python3 is used only to generate the nsec key.
apt-get update -qq >/dev/null 2>&1 || true
apt-get install -y -qq git curl python3 >/dev/null 2>&1 || true

# Swap: the in-Docker release build can exceed 8 GB during linking. Add a
# 4 GB swap file if the host has none, so the build does not OOM.
if ! swapon --show 2>/dev/null | grep -q .; then
  log "No swap found — creating a 4 GB swap file for the build..."
  if fallocate -l 4G /swapfile 2>/dev/null || dd if=/dev/zero of=/swapfile bs=1M count=4096 status=none; then
    chmod 600 /swapfile
    mkswap /swapfile >/dev/null
    swapon /swapfile
    grep -q '^/swapfile ' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
  else
    warn "could not create swap; the build may run out of memory"
  fi
fi

# ---------------------------------------------------------------------------
# 2. Get the repository
# ---------------------------------------------------------------------------
if [ -d "$REPO_DIR/.git" ]; then
  log "Updating existing repo at $REPO_DIR ..."
  git -C "$REPO_DIR" pull --ff-only
else
  log "Cloning Satonero repo into $REPO_DIR ..."
  git clone "$REPO_URL" "$REPO_DIR"
fi

# ---------------------------------------------------------------------------
# 3. Prepare the runtime directory
# ---------------------------------------------------------------------------
mkdir -p "$APP_DIR/config"
# The container runs as a non-root user; let it create/write mostro.db here.
chmod 777 "$APP_DIR/config"

# ---------------------------------------------------------------------------
# 4. Generate the node identity (nsec = secret, npub = public)
# ---------------------------------------------------------------------------
log "Generating node identity..."
NSEC_NPUB="$(python3 - <<'PY'
import secrets
CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
def polymod(v):
    chk = 1
    for x in v:
        b = chk >> 25
        chk = (chk & 0x1ffffff) << 5 ^ x
        for i in range(5):
            chk ^= GEN[i] if ((b >> i) & 1) else 0
    return chk
def hrp_expand(h):
    return [ord(x) >> 5 for x in h] + [0] + [ord(x) & 31 for x in h]
def checksum(h, d):
    pm = polymod(hrp_expand(h) + d + [0,0,0,0,0,0]) ^ 1
    return [(pm >> 5 * (5 - i)) & 31 for i in range(6)]
def encode(h, d):
    c = d + checksum(h, d)
    return h + '1' + ''.join(CHARSET[x] for x in c)
def bits(d, fb, tb):
    acc = 0; n = 0; r = []
    for x in d:
        acc = (acc << fb) | x; n += fb
        while n >= tb:
            n -= tb; r.append((acc >> n) & 31)
    if n:
        r.append((acc << (tb - n)) & 31)
    return r
s = secrets.token_bytes(32)
d = bits(s, 8, 5)
print(encode('nsec', d))
print(encode('npub', d))
PY
)"
NSEC="$(printf '%s\n' "$NSEC_NPUB" | sed -n '1p')"
NPUB="$(printf '%s\n' "$NSEC_NPUB" | sed -n '2p')"

# ---------------------------------------------------------------------------
# 5. Write settings.toml (complete — every field the daemon requires)
# ---------------------------------------------------------------------------
log "Writing $APP_DIR/config/settings.toml ..."
cat > "$APP_DIR/config/settings.toml" <<EOF
# Satonero daemon — Cashu mode, public relays (generated by quickstart.sh).

[lightning]
# Cashu mode ignores this whole section; these are placeholders only.
lnd_cert_file = '/config/lnd/tls.cert'
lnd_macaroon_file = '/config/lnd/admin.macaroon'
lnd_grpc_host = 'https://127.0.0.1:10009'
invoice_expiration_window = 3600
hold_invoice_cltv_delta = 144
hold_invoice_expiration_window = 300
payment_attempts = 3
payment_retries_interval = 60

[nostr]
# Secret key of this node. Keep it secret; back it up off the server.
nsec_privkey = '$NSEC'
# Public relays — no domain/self-hosted relay needed for testing.
relays = ['wss://nos.lol', 'wss://relay.damus.io']

[mostro]
name = "Satonero"
about = "Independent Bitcoin peer-to-peer marketplace"
fee = 0
max_routing_fee = 0.002
max_order_amount = 1000000
min_payment_amount = 100
expiration_hours = 24
max_expiration_days = 15
expiration_seconds = 900
user_rates_sent_interval_seconds = 3600
publish_relays_interval = 60
pow = 0
publish_mostro_info_interval = 300
fiat_currencies_accepted = ['EUR', 'RSD', 'BAM', 'HRK', 'USD', 'ARS', 'CUP']
max_orders_per_response = 10
dev_fee_percentage = 0.30
dev_fee_enabled = false

[database]
url = "sqlite:///config/mostro.db"

[rpc]
enabled = false
listen_address = "127.0.0.1"
port = 50051

[cashu]
# Cashu escrow — no LND. Empty mint_urls accepts any public mint for orders.
enabled = true
mint_urls = []
escrow_locktime_days = 15
EOF
chmod 644 "$APP_DIR/config/settings.toml"

# ---------------------------------------------------------------------------
# 6. Write the minimal Docker Compose file (daemon only)
# ---------------------------------------------------------------------------
log "Writing $APP_DIR/docker-compose.yml ..."
cat > "$APP_DIR/docker-compose.yml" <<EOF
services:
  satonero:
    build:
      context: $REPO_DIR
      dockerfile: docker/Dockerfile
    container_name: satonero
    restart: unless-stopped
    volumes:
      - ./config:/config
    environment:
      - RUST_LOG=info,mostro=info
EOF

# ---------------------------------------------------------------------------
# 7. Build and start
# ---------------------------------------------------------------------------
log "Building the image (first run takes 10-20 minutes) and starting..."
cd "$APP_DIR"
docker compose up -d --build

# ---------------------------------------------------------------------------
# 8. Summary
# ---------------------------------------------------------------------------
log "Done. Satonero node is starting."
printf '\n\033[1;32m  npub: %s\033[0m\n' "$NPUB"
printf '\033[1;31m  nsec: %s\033[0m  <-- back this up off the server!\n' "$NSEC"
printf '\n  Config dir : %s/config/\n' "$APP_DIR"
printf '  See logs   : docker compose -f %s/docker-compose.yml logs -f satonero\n' "$APP_DIR"
printf '  Status     : docker compose -f %s/docker-compose.yml ps\n' "$APP_DIR"

warn "To see your node in the app: open Satonero-mobile -> community selection -> 'Add custom node' and paste the npub above. The app uses the same public relays (nos.lol / relay.damus.io), so no domain is required."
warn "For a REAL Cashu escrow trade you must later add a NUT-11 (P2PK 2-of-3) capable mint URL to the 'mint_urls' list in $APP_DIR/config/settings.toml and restart (docker compose restart satonero)."
