#!/usr/bin/env bash
# =============================================================================
# HiveBear Coordinator — Automatic Free HTTPS via sslip.io + Caddy (Let's Encrypt)
# Run on the Google Cloud VM (34.63.209.220):
#   curl -fsSL https://raw.githubusercontent.com/kuromi04/HiveBear/main/scripts/enable-https-sslip.sh | sudo bash
# =============================================================================
set -euo pipefail

DOMAIN="${1:-34-63-209-220.sslip.io}"
LEDGER_DIR="/var/lib/hivebear-coordinator"
IMAGE="ghcr.io/kuromi04/hivebear-coordinator:latest"

echo "==> [1/4] Preparing persistent Karma ledger directory at ${LEDGER_DIR}..."
mkdir -p "${LEDGER_DIR}"

echo "==> [2/4] Restarting hivebear-coordinator container on 127.0.0.1:7879..."
if docker image inspect hivebear-coordinator:prebuilt >/dev/null 2>&1; then
    IMAGE="hivebear-coordinator:prebuilt"
elif docker image inspect hivebear-coordinator:latest >/dev/null 2>&1; then
    IMAGE="hivebear-coordinator:latest"
fi

docker rm -f hivebear-coordinator >/dev/null 2>&1 || true
docker run -d \
    --name hivebear-coordinator \
    --restart unless-stopped \
    -p 127.0.0.1:7879:7879 \
    -v "${LEDGER_DIR}:/data" \
    -e HIVEBEAR_LEDGER_PATH=/data/karma_ledger.json \
    "${IMAGE}"

echo "==> [3/4] Installing Caddy (automatic Let's Encrypt HTTPS)..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl
if ! command -v caddy >/dev/null 2>&1; then
    apt-get install -y caddy
fi

echo "==> [4/4] Writing /etc/caddy/Caddyfile for https://${DOMAIN} + HTTP backward compatibility..."
cat >/etc/caddy/Caddyfile <<EOF
${DOMAIN} {
    reverse_proxy 127.0.0.1:7879
}

http://${DOMAIN}, http://34.63.209.220, :80 {
    reverse_proxy 127.0.0.1:7879
}
EOF

systemctl enable caddy
systemctl restart caddy

sleep 3
echo ""
echo "====================================================================="
echo " HiveBear Coordinator is LIVE on:"
echo "   HTTPS (SSL): https://${DOMAIN}/dashboard"
echo "   HTTP (DNS):  http://${DOMAIN}/dashboard"
echo "   HTTP (IP):   http://34.63.209.220/dashboard"
echo "====================================================================="
curl -fsS "http://127.0.0.1:7879/health" && echo ""
