#!/usr/bin/env bash
# Runs on the VPS (Debian/Ubuntu) as root or via passwordless sudo. Idempotent.
# Expects the agent binary already uploaded to /tmp/hfr-agent-upload.
set -euo pipefail

SUDO=""
if [ "$(id -u)" -ne 0 ]; then SUDO="sudo"; fi
step() { echo "==> $*"; }

command -v apt-get >/dev/null || { echo "only Debian/Ubuntu is supported by this installer" >&2; exit 1; }
export DEBIAN_FRONTEND=noninteractive

step "Installing system packages (ffmpeg, Chrome libraries, fonts)"
$SUDO apt-get update -qq
ALSA=libasound2; apt-cache show libasound2t64 >/dev/null 2>&1 && ALSA=libasound2t64
$SUDO apt-get install -y -qq ca-certificates curl gnupg tar xz-utils openssl unzip ffmpeg fonts-liberation \
  libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 \
  libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 "$ALSA" >/dev/null

NODE_MAJOR=0
if command -v node >/dev/null; then NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]'); fi
if [ "$NODE_MAJOR" -lt 22 ]; then
  step "Installing Node.js 22"
  curl -fsSL https://deb.nodesource.com/setup_22.x | $SUDO -E bash - >/dev/null
  $SUDO apt-get install -y -qq nodejs >/dev/null
else
  step "Node.js $(node -v) already present"
fi

step "Installing hyperframes"
$SUDO npm install -g hyperframes >/dev/null 2>&1

step "Creating service user and directories"
id hfr >/dev/null 2>&1 || $SUDO useradd --system --create-home --home-dir /var/lib/hfr --shell /usr/sbin/nologin hfr
$SUDO install -d -o hfr -g hfr /var/lib/hfr/jobs
$SUDO install -d -m 750 -o root -g hfr /etc/hfr

step "Installing hfr-agent"
$SUDO systemctl stop hfr-agent 2>/dev/null || true
$SUDO install -m 755 /tmp/hfr-agent-upload /usr/local/bin/hfr-agent
rm -f /tmp/hfr-agent-upload

if [ ! -s /etc/hfr/agent.env ]; then
  TOKEN=$(openssl rand -hex 32)
  printf 'HFR_TOKEN=%s\n' "$TOKEN" | $SUDO tee /etc/hfr/agent.env >/dev/null
  $SUDO chown root:hfr /etc/hfr/agent.env
  $SUDO chmod 640 /etc/hfr/agent.env
else
  step "Keeping existing token"
fi

$SUDO tee /etc/systemd/system/hfr-agent.service >/dev/null <<'UNIT'
[Unit]
Description=hfr agent (Hyperframes remote renderer)
After=network.target

[Service]
User=hfr
Group=hfr
Environment=HOME=/var/lib/hfr
Environment=HFR_ROOT=/var/lib/hfr/jobs
Environment=HFR_ADDR=127.0.0.1:7878
EnvironmentFile=/etc/hfr/agent.env
ExecStart=/usr/local/bin/hfr-agent
Restart=on-failure

[Install]
WantedBy=multi-user.target
UNIT

step "Downloading the render browser (Chrome) for the service user"
$SUDO -u hfr -H hyperframes browser ensure >/dev/null 2>&1 || echo "warning: 'hyperframes browser ensure' failed; run 'hyperframes doctor' as user hfr" >&2

step "Starting the service"
$SUDO systemctl daemon-reload
$SUDO systemctl enable --now hfr-agent >/dev/null 2>&1
sleep 1
$SUDO systemctl is-active --quiet hfr-agent || { $SUDO journalctl -u hfr-agent -n 20 --no-pager >&2; echo "agent failed to start" >&2; exit 1; }

echo "HFR_TOKEN=$($SUDO sed -n 's/^HFR_TOKEN=//p' /etc/hfr/agent.env)"
