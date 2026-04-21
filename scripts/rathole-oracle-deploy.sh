#!/usr/bin/env bash
# rathole-oracle-deploy.sh
#
# One-shot setup of a self-hosted rathole relay on a fresh Linux VM.
# Works on any systemd-based distro (Ubuntu, Oracle Linux, Debian). Tested on
# Oracle Cloud Infrastructure's Always-Free ARM tier (Ampere A1 Flex).
#
# Usage:
#   1. Spin up an Oracle Cloud Free Tier VM (ARM, Ubuntu 22.04 recommended).
#   2. Copy this script + your server.toml (from the wizard) to the VM.
#   3. Run:   sudo bash rathole-oracle-deploy.sh /path/to/server.toml
#
# What it does:
#   - Installs rathole into /usr/local/bin (downloads release asset from GitHub).
#   - Copies server.toml to /etc/rathole/server.toml.
#   - Registers a systemd unit that runs rathole as a dedicated `rathole` user.
#   - Opens the control port + every per-service public port in `iptables`.
#   - Starts + enables the service.
#
# No Docker, no compilation, no extra deps. ~30 seconds start to finish.
#
# SECURITY
#   The unit runs as an unprivileged user with `ProtectSystem=strict` and
#   `NoNewPrivileges`. Rathole uses Noise Protocol end-to-end — no TLS certs
#   required.
#
# COST
#   Oracle's Always-Free ARM tier includes 4 OCPUs, 24GB RAM, and 10TB
#   egress/month at $0/month. More than enough for most self-hosted relays.

set -euo pipefail

RATHOLE_VERSION="${RATHOLE_VERSION:-v0.5.0}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
CONFIG_DIR="${CONFIG_DIR:-/etc/rathole}"
UNIT_PATH="/etc/systemd/system/rathole.service"
RATHOLE_USER="rathole"

# ─── Validate args ──────────────────────────────────────────────────────────

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <path-to-server.toml>" >&2
    exit 64
fi

SRC_TOML="$1"
if [[ ! -f "$SRC_TOML" ]]; then
    echo "ERROR: $SRC_TOML not found" >&2
    exit 66
fi

if [[ $EUID -ne 0 ]]; then
    echo "ERROR: must be run as root (use sudo)" >&2
    exit 77
fi

# ─── Detect architecture ────────────────────────────────────────────────────

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64)   RATHOLE_ARCH="x86_64-unknown-linux-gnu" ;;
    aarch64)  RATHOLE_ARCH="aarch64-unknown-linux-musl" ;;  # Oracle ARM
    *)
        echo "ERROR: unsupported arch: $ARCH" >&2
        exit 69
        ;;
esac

# ─── Install rathole ────────────────────────────────────────────────────────

ASSET="rathole-${RATHOLE_ARCH}.zip"
DOWNLOAD_URL="https://github.com/rapiz1/rathole/releases/download/${RATHOLE_VERSION}/${ASSET}"

echo ">>> downloading rathole ${RATHOLE_VERSION} (${RATHOLE_ARCH})"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
curl -fsSL "$DOWNLOAD_URL" -o "$TMPDIR/$ASSET"
unzip -q "$TMPDIR/$ASSET" -d "$TMPDIR"
install -m 0755 "$TMPDIR/rathole" "$INSTALL_DIR/rathole"

# ─── Install config ─────────────────────────────────────────────────────────

mkdir -p "$CONFIG_DIR"
install -m 0600 "$SRC_TOML" "$CONFIG_DIR/server.toml"

# ─── Create user ────────────────────────────────────────────────────────────

if ! id -u "$RATHOLE_USER" >/dev/null 2>&1; then
    echo ">>> creating $RATHOLE_USER user"
    useradd --system --no-create-home --shell /usr/sbin/nologin "$RATHOLE_USER"
fi
chown -R "$RATHOLE_USER:$RATHOLE_USER" "$CONFIG_DIR"

# ─── Parse ports from server.toml so we can open the firewall ──────────────

PORTS="$(grep -E '^\s*bind_addr\s*=' "$CONFIG_DIR/server.toml" \
         | sed -E 's/.*:([0-9]+)".*/\1/' | sort -u)"
if [[ -z "$PORTS" ]]; then
    echo "WARN: could not parse any bind_addr ports from server.toml" >&2
fi

# ─── Write systemd unit ─────────────────────────────────────────────────────

echo ">>> writing $UNIT_PATH"
cat >"$UNIT_PATH" <<EOF
[Unit]
Description=rathole reverse tunnel server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/rathole --server $CONFIG_DIR/server.toml
User=$RATHOLE_USER
Group=$RATHOLE_USER
Restart=on-failure
RestartSec=2
# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF

# ─── Open firewall ──────────────────────────────────────────────────────────

if command -v ufw >/dev/null 2>&1 && ufw status | grep -q "Status: active"; then
    for p in $PORTS; do
        echo ">>> ufw allow $p/tcp"
        ufw allow "$p/tcp" || true
    done
elif command -v firewall-cmd >/dev/null 2>&1; then
    for p in $PORTS; do
        echo ">>> firewall-cmd add-port $p/tcp"
        firewall-cmd --permanent --add-port="${p}/tcp" || true
    done
    firewall-cmd --reload || true
else
    # Oracle Cloud's default Ubuntu image uses iptables directly.
    for p in $PORTS; do
        echo ">>> iptables -I INPUT -p tcp --dport $p -j ACCEPT"
        iptables -C INPUT -p tcp --dport "$p" -j ACCEPT 2>/dev/null \
            || iptables -I INPUT -p tcp --dport "$p" -j ACCEPT
    done
    if command -v netfilter-persistent >/dev/null 2>&1; then
        netfilter-persistent save || true
    fi
fi

# Reminder: Oracle Cloud also has Security Lists (VCN-level firewall). The
# iptables rules above are host-level; the user still has to allow these
# same ports in the VCN Security List via the OCI console.
echo
echo "!! Oracle Cloud users: also open these ports in the VCN Security List:"
for p in $PORTS; do echo "   - TCP $p"; done
echo

# ─── Start service ──────────────────────────────────────────────────────────

systemctl daemon-reload
systemctl enable --now rathole
sleep 2
systemctl --no-pager status rathole | head -n 20

echo
echo "Done. Tail logs with: journalctl -u rathole -f"
