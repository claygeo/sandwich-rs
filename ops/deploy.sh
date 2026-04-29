#!/usr/bin/env bash
# Deploy sandwich-rs to a Linux VPS.
#
# Strategy: rsync source to the VPS and build there. Avoids cross-compile pain.
# First-time setup creates the user, dirs, env file template, and systemd unit.
#
# Required env (or pass on command line):
#   VPS_HOST  — default 77.42.83.22 (Hetzner)
#   VPS_USER  — default root
#   VPS_PORT  — default 2222

set -euo pipefail

VPS_HOST="${VPS_HOST:-77.42.83.22}"
VPS_USER="${VPS_USER:-root}"
VPS_PORT="${VPS_PORT:-2222}"
SSH="ssh -p ${VPS_PORT} ${VPS_USER}@${VPS_HOST}"
RSYNC_E="ssh -p ${VPS_PORT}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Sync source to VPS"
rsync -e "${RSYNC_E}" -avz \
  --exclude target/ \
  --exclude .git/ \
  --exclude node_modules/ \
  --exclude .env \
  "${REPO_ROOT}/" "${VPS_USER}@${VPS_HOST}:/opt/sandwich-rs.src/"

echo "==> First-run bootstrap (idempotent) + build + install + restart"
${SSH} bash -se <<'REMOTE'
set -euo pipefail

# 1. user + dirs
id sandwich >/dev/null 2>&1 || useradd --system --shell /usr/sbin/nologin --home /var/lib/sandwich-rs sandwich
mkdir -p /opt/sandwich-rs /etc/sandwich-rs /var/lib/sandwich-rs
chown -R sandwich:sandwich /var/lib/sandwich-rs

# 2. ensure rust toolchain (use rustup if missing)
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  source "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"

# 3. build
cd /opt/sandwich-rs.src
cargo build --release --bin sandwich-rs --bin backtest

# 4. install binaries
install -m 0755 target/release/sandwich-rs /opt/sandwich-rs/sandwich-rs
install -m 0755 target/release/backtest /opt/sandwich-rs/backtest

# 5. install systemd unit if missing or different
install -m 0644 ops/sandwich-rs.service /etc/systemd/system/sandwich-rs.service

# 6. seed env template if /etc/sandwich-rs/env doesn't exist yet
if [ ! -f /etc/sandwich-rs/env ]; then
  install -m 0640 -g sandwich ops/sandwich-rs.env.example /etc/sandwich-rs/env
  echo ""
  echo "!!  /etc/sandwich-rs/env was just seeded from the example."
  echo "!!  Edit it with real Helius + Supabase credentials before the service will work:"
  echo "!!     vim /etc/sandwich-rs/env"
  echo "!!     systemctl restart sandwich-rs"
  echo ""
fi
chmod 0640 /etc/sandwich-rs/env
chown root:sandwich /etc/sandwich-rs/env

# 7. ownership + restart
chown -R sandwich:sandwich /opt/sandwich-rs

systemctl daemon-reload
if systemctl is-enabled sandwich-rs.service >/dev/null 2>&1; then
  systemctl restart sandwich-rs.service
else
  systemctl enable --now sandwich-rs.service
fi

sleep 2
systemctl status sandwich-rs.service --no-pager | head -15
REMOTE

echo ""
echo "==> Tail logs (Ctrl+C to detach):"
exec ${SSH} 'journalctl -u sandwich-rs -f -n 50'
