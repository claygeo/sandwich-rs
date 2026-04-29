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

echo "==> Bootstrap + sync via git + build + install + restart (idempotent, single SSH session)"
${SSH} bash -se <<'REMOTE'
set -euo pipefail

# 1. user + dirs
id sandwich >/dev/null 2>&1 || useradd --system --shell /usr/sbin/nologin --home /var/lib/sandwich-rs sandwich
mkdir -p /opt/sandwich-rs /etc/sandwich-rs /var/lib/sandwich-rs
chown -R sandwich:sandwich /var/lib/sandwich-rs

# 2. install prerequisites
if ! command -v git >/dev/null 2>&1; then apt-get update -qq && apt-get install -y -qq git; fi
if ! command -v curl >/dev/null 2>&1; then apt-get update -qq && apt-get install -y -qq curl ca-certificates; fi
# Build essentials for sqlx + rustls native backends
if ! dpkg -s build-essential pkg-config libssl-dev >/dev/null 2>&1; then
  apt-get update -qq && apt-get install -y -qq build-essential pkg-config libssl-dev
fi

# 3. sync source via git (avoids needing rsync on the dev box)
if [ -d /opt/sandwich-rs.src/.git ]; then
  cd /opt/sandwich-rs.src
  git fetch --depth 1 origin main
  git reset --hard origin/main
else
  git clone --depth 1 https://github.com/claygeo/sandwich-rs.git /opt/sandwich-rs.src
fi

# 4. ensure rust toolchain (use rustup if missing)
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"

# 5. build
cd /opt/sandwich-rs.src
cargo build --release --bin sandwich-rs --bin backtest

# 6. install binaries
install -m 0755 target/release/sandwich-rs /opt/sandwich-rs/sandwich-rs
install -m 0755 target/release/backtest /opt/sandwich-rs/backtest

# 7. install systemd unit
install -m 0644 ops/sandwich-rs.service /etc/systemd/system/sandwich-rs.service

# 8. seed env template if /etc/sandwich-rs/env doesn't exist yet
if [ ! -f /etc/sandwich-rs/env ]; then
  install -m 0640 -g sandwich ops/sandwich-rs.env.example /etc/sandwich-rs/env
  echo ""
  echo "!!  /etc/sandwich-rs/env was seeded from the example."
  echo "!!  Edit it with real Helius + Supabase credentials, then systemctl restart sandwich-rs."
  echo ""
fi
chmod 0640 /etc/sandwich-rs/env
chown root:sandwich /etc/sandwich-rs/env

# 9. ownership + restart
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
