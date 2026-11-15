#!/usr/bin/env bash
#
# setup-satonero.sh — automated installer for the Satonero daemon (`mostrod`)
# on an Ubuntu VPS (Hetzner, 8 GB RAM is plenty).
#
# What it does:
#   1. Updates the system and installs build dependencies (gcc, cmake, sqlite,
#      openssl, pkg-config, git, python3, protobuf-compiler).
#   2. (optional) installs PostgreSQL — see NOTE: mostrod uses SQLite, not
#      PostgreSQL.
#   3. Installs Rust via rustup and the pinned toolchain (1.94.0).
#   4. Clones the Satonero repo and compiles `mostrod` (release).
#   5. Installs the binary into /usr/local/bin/mostrod.
#   6. Creates the `mostro` system user, the config directory (/opt/mostro)
#      and copies settings.toml from the template.
#   7. (optional) generates a Nostr nsec key into /opt/mostro/.env (mode 600).
#   8. Installs and enables the `mostro.service` systemd unit (it does NOT
#      start it: settings.toml + LND/Cashu must be configured first).
#
# The script is IDEMPOTENT — safe to run more than once.
#
# Usage (as root):
#   sudo bash setup-satonero.sh
#
# Options via environment variables (example):
#   INSTALL_POSTGRES=false GENERATE_NSEC=true sudo -E bash setup-satonero.sh
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration — change here or via environment variables
# ---------------------------------------------------------------------------
REPO_URL="${REPO_URL:-https://github.com/Konprostart/Satonero.git}"
REPO_BRANCH="${REPO_BRANCH:-main}"
SRC_DIR="${SRC_DIR:-/opt/satonero}"          # source checkout
CONFIG_DIR="${CONFIG_DIR:-/opt/mostro}"      # settings.toml + mostro.db + .env
SERVICE_USER="${SERVICE_USER:-mostro}"
SERVICE_NAME="${SERVICE_NAME:-mostro}"
BIN_NAME="mostrod"
INSTALL_POSTGRES="${INSTALL_POSTGRES:-true}" # PostgreSQL is NOT required by mostrod
GENERATE_NSEC="${GENERATE_NSEC:-true}"       # generate nsec into .env
CREATE_SWAP="${CREATE_SWAP:-true}"           # add swap only if none exists (safer build)

log()  { printf '\n\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\n\033[1;33m[!]\033[0m %s\n' "$*"; }
die()  { printf '\n\033[1;31m[x]\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Preconditions
# ---------------------------------------------------------------------------
[[ $EUID -eq 0 ]] || die "Run as root:  sudo bash $0"

if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  [[ "${ID:-}" == "ubuntu" ]] || warn "This script targets Ubuntu (current: ${PRETTY_NAME:-unknown})."
fi

export DEBIAN_FRONTEND=noninteractive

# ---------------------------------------------------------------------------
# 1. System + build dependencies
# ---------------------------------------------------------------------------
log "Updating packages and installing build dependencies..."
apt-get update -y
apt-get install -y \
  build-essential cmake pkg-config git curl ca-certificates \
  libssl-dev libsqlite3-dev sqlite3 \
  protobuf-compiler \
  python3

command -v protoc >/dev/null 2>&1 || die "protoc not found after installation."
log "protoc: $(protoc --version)"

# ---------------------------------------------------------------------------
# 2. (optional) PostgreSQL — NOTE: mostrod uses SQLite, NOT PostgreSQL
# ---------------------------------------------------------------------------
if [[ "$INSTALL_POSTGRES" == "true" ]]; then
  log "Installing PostgreSQL (optional; mostrod does NOT use it — the daemon's database is SQLite)..."
  apt-get install -y postgresql postgresql-contrib
  systemctl enable --now postgresql >/dev/null 2>&1 || true
  if command -v pg_lsclusters >/dev/null 2>&1; then
    pg_lsclusters || true
  fi
else
  log "Skipping PostgreSQL (INSTALL_POSTGRES=false)."
fi

# ---------------------------------------------------------------------------
# 3. Rust (rustup) + pinned toolchain
# ---------------------------------------------------------------------------
if command -v rustup >/dev/null 2>&1; then
  log "rustup already installed."
else
  log "Installing Rust via rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi

# shellcheck disable=SC1090
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
rustup --version
command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH after Rust install."

# ---------------------------------------------------------------------------
# 4. (optional) swap — so rustc does not get OOM-killed during the big build
# ---------------------------------------------------------------------------
if [[ "$CREATE_SWAP" == "true" ]] && [[ "$(awk 'NR > 1' /proc/swaps | wc -l)" -eq 0 ]]; then
  warn "No active swap; creating a 4 GB swap file for a safer build..."
  if command -v fallocate >/dev/null 2>&1; then
    fallocate -l 4G /swapfile || dd if=/dev/zero of=/swapfile bs=1M count=4096
  else
    dd if=/dev/zero of=/swapfile bs=1M count=4096
  fi
  chmod 600 /swapfile
  mkswap /swapfile >/dev/null
  swapon /swapfile
  grep -q '^/swapfile ' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

# ---------------------------------------------------------------------------
# 5. Clone the repository
# ---------------------------------------------------------------------------
if [[ -d "$SRC_DIR/.git" ]]; then
  log "Repository already exists at $SRC_DIR — updating..."
  git -C "$SRC_DIR" fetch --all --tags
  git -C "$SRC_DIR" checkout "$REPO_BRANCH"
  git -C "$SRC_DIR" pull --ff-only origin "$REPO_BRANCH"
else
  log "Cloning $REPO_URL (branch $REPO_BRANCH) into $SRC_DIR..."
  mkdir -p "$(dirname "$SRC_DIR")"
  git clone --branch "$REPO_BRANCH" "$REPO_URL" "$SRC_DIR"
fi

# ---------------------------------------------------------------------------
# 6. Compile (release)
# ---------------------------------------------------------------------------
cd "$SRC_DIR"

log "Installing the pinned Rust toolchain (see rust-toolchain.toml)..."
rustup show

log "Compiling $BIN_NAME (release). This takes 10–30 minutes..."
# --locked: use the committed Cargo.lock (reproducible build).
cargo build --release --locked --bin "$BIN_NAME"

# ---------------------------------------------------------------------------
# 7. Install the binary
# ---------------------------------------------------------------------------
log "Installing $BIN_NAME into /usr/local/bin..."
install -m 0755 "$SRC_DIR/target/release/$BIN_NAME" "/usr/local/bin/$BIN_NAME"
VERSION=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
log "$BIN_NAME v$VERSION installed."

# ---------------------------------------------------------------------------
# 8. System user + config directory
# ---------------------------------------------------------------------------
if id -u "$SERVICE_USER" >/dev/null 2>&1; then
  log "User '$SERVICE_USER' already exists."
else
  log "Creating system user '$SERVICE_USER'..."
  useradd --system --no-create-home --home-dir "$CONFIG_DIR" \
          --shell /usr/sbin/nologin "$SERVICE_USER"
fi

log "Preparing the config directory $CONFIG_DIR..."
mkdir -p "$CONFIG_DIR"

if [[ ! -f "$CONFIG_DIR/settings.toml" ]]; then
  cp "$SRC_DIR/settings.tpl.toml" "$CONFIG_DIR/settings.toml"
  warn "Created $CONFIG_DIR/settings.toml from the template — EDIT it before starting."
fi

# ---------------------------------------------------------------------------
# 9. (optional) generate a Nostr nsec key into .env
# ---------------------------------------------------------------------------
if [[ "$GENERATE_NSEC" == "true" && ! -f "$CONFIG_DIR/.env" ]]; then
  log "Generating a Nostr nsec key into $CONFIG_DIR/.env (bech32, pure Python stdlib)..."
  NSEC=$(python3 - <<'PY'
import os

CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"

def bech32_polymod(values):
    gen = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]
    chk = 1
    for v in values:
        b = chk >> 25
        chk = (chk & 0x1ffffff) << 5 ^ v
        for i in range(5):
            chk ^= gen[i] if ((b >> i) & 1) else 0
    return chk

def hrp_expand(hrp):
    return [ord(x) >> 5 for x in hrp] + [0] + [ord(x) & 31 for x in hrp]

def create_checksum(hrp, data):
    values = hrp_expand(hrp) + data
    poly = bech32_polymod(values + [0, 0, 0, 0, 0, 0]) ^ 1
    return [(poly >> 5 * (5 - i)) & 31 for i in range(6)]

def convertbits(data, frombits, tobits, pad=True):
    acc = 0
    bits = 0
    ret = []
    maxv = (1 << tobits) - 1
    max_acc = (1 << (frombits + tobits - 1)) - 1
    for value in data:
        acc = ((acc << frombits) | value) & max_acc
        bits += frombits
        while bits >= tobits:
            bits -= tobits
            ret.append((acc >> bits) & maxv)
    if pad:
        if bits:
            ret.append((acc << (tobits - bits)) & maxv)
    return ret

def encode(hrp, data):
    combined = data + create_checksum(hrp, data)
    return hrp + '1' + ''.join(CHARSET[d] for d in combined)

secret = os.urandom(32)
print(encode("nsec", convertbits(list(secret), 8, 5)))
PY
)
  umask 077
  printf 'MOSTRO_NSEC_PRIVKEY=%s\n' "$NSEC" > "$CONFIG_DIR/.env"
  warn "Generated nsec into $CONFIG_DIR/.env (mode 600). MAKE A BACKUP — losing it"
  warn "loses control of this Satonero node's identity."
fi

# ---------------------------------------------------------------------------
# 10. Ownership + permissions
# ---------------------------------------------------------------------------
chown -R "$SERVICE_USER":"$SERVICE_USER" "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"
chmod 600 "$CONFIG_DIR/settings.toml" 2>/dev/null || true
chmod 600 "$CONFIG_DIR/.env" 2>/dev/null || true

# ---------------------------------------------------------------------------
# 11. systemd service
# ---------------------------------------------------------------------------
log "Installing the systemd service $SERVICE_NAME.service..."
cat > "/etc/systemd/system/$SERVICE_NAME.service" <<EOF
[Unit]
Description=Satonero daemon (mostrod)
Documentation=https://github.com/Konprostart/Satonero
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
WorkingDirectory=$CONFIG_DIR
ExecStart=/usr/local/bin/$BIN_NAME -d $CONFIG_DIR
Restart=on-failure
RestartSec=5
# main() sets RUST_LOG itself (release: none,mostro=info). Leave it unset
# unless you want more verbose logs (e.g. RUST_LOG=debug,mostro=info).

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$SERVICE_NAME.service" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
cat <<SUMMARY

$(printf '\033[1;32m')==============================================================
 INSTALLATION COMPLETE
==============================================================$(printf '\033[0m')

 Installed:
   - Rust      : $(rustc --version 2>/dev/null || echo 'n/a')
   - protoc    : $(protoc --version 2>/dev/null || echo 'n/a')
   - daemon    : /usr/local/bin/$BIN_NAME (v$VERSION)
   - config    : $CONFIG_DIR/settings.toml
   - nsec      : $CONFIG_DIR/.env   ($([[ -f "$CONFIG_DIR/.env" ]] && echo generated || echo not generated))
   - service   : $SERVICE_NAME.service (enabled, NOT started)

 NEXT STEPS (required before starting):
   1. Edit the config:
        nano $CONFIG_DIR/settings.toml
      - [nostr] relays          → your relay(s)
      - [lightning] lnd_*       → paths to the LND cert/macaroon and gRPC host
        (OR enable [cashu] enabled = true to run WITHOUT an LND node)
      - [mostro] dev_fee_*      → MUST set your own dev_fee_lightning_address
                                  if you enable dev_fee on mainnet (the compiled-in
                                  fallback still points at the Mostro address!)
   2. (optional) read your npub from the logs after the first start:
        journalctl -u $SERVICE_NAME -n 50 --no-pager
   3. Start the service:
        systemctl start $SERVICE_NAME
        systemctl status $SERVICE_NAME

 NOTE: mostrod in Lightning mode requires a reachable LND node; without one the
       service exits on start. For Cashu mode (no LND), set [cashu] enabled = true.

 Full guide: deploy/README.md

SUMMARY
