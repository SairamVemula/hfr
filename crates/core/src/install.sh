#!/usr/bin/env bash
# Runs on the VPS (Debian/Ubuntu) as root or via passwordless sudo.
#
# Safe to re-run: every step first checks what is already there and only installs what is missing or
# upgrades what is outdated. Nothing that could interrupt a running render (replacing the agent, upgrading
# hyperframes) is touched while a job is running; those steps are skipped with a note and picked up on the
# next run. HFR_FORCE=1 (set by `hfr setup --force`) reinstalls everything.
#
# Expects the agent binary already uploaded to /tmp/hfr-agent-upload.
set -euo pipefail
exec 2>&1
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a APT_LISTCHANGES_FRONTEND=none
LOG=/tmp/hfr-install.log
: > "$LOG"
# Run a noisy command quietly; show the end of its output only if it fails.
quiet() { "$@" >>"$LOG" 2>&1 || { echo "FAILED: $*"; tail -n 25 "$LOG"; return 1; }; }

FORCE="${HFR_FORCE:-}"
SUDO=""
if [ "$(id -u)" -ne 0 ]; then SUDO="sudo"; fi
step() { echo "==> $*"; }
ok() { echo "    ok: $*"; }
did() { echo "    done: $*"; }
skip() { echo "    skipped: $*"; }

command -v apt-get >/dev/null || { echo "only Debian/Ubuntu is supported by this installer" >&2; exit 1; }

# --- helpers ---------------------------------------------------------------------------------------------
pkg_installed() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"; }
# Ubuntu 24.04 renamed several libraries to <name>t64; either counts as installed.
have_pkg() { pkg_installed "$1" || pkg_installed "${1}t64"; }
have_cmd() { command -v "$1" >/dev/null 2>&1; }
# Highest of two versions (sort -V).
ver_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n1)" = "$1" ]; }

# True when the agent reports a running render. Replacing the agent restarts it (killing the render) and
# upgrading hyperframes swaps files under a live `npx`, so both wait until the VPS is idle.
BUSY=0
if [ -s /etc/hfr/agent.env ] || $SUDO test -s /etc/hfr/agent.env 2>/dev/null; then
  TOKEN_NOW=$($SUDO sed -n 's/^HFR_TOKEN=//p' /etc/hfr/agent.env 2>/dev/null || true)
  if [ -n "$TOKEN_NOW" ] && have_cmd curl \
    && curl -fsS -m 5 -H "Authorization: Bearer $TOKEN_NOW" http://127.0.0.1:7878/jobs 2>/dev/null | grep -q '"state":"running"'; then
    BUSY=1
  fi
fi
[ "$BUSY" = 1 ] && echo "==> A render is running on this VPS: upgrades that could interrupt it will be skipped"

# Jobs created by an older agent have no saved metadata: restarting that agent would drop them from the job list
# (their files would stay on disk). Hold the agent update until they are downloaded or deleted.
LEGACY=0
if $SUDO test -d /var/lib/hfr/jobs; then
  LEGACY=$($SUDO find /var/lib/hfr/jobs -mindepth 1 -maxdepth 1 -type d ! -exec test -e '{}/meta.json' \; -print 2>/dev/null | wc -l)
fi

# --- system packages -------------------------------------------------------------------------------------
step "System packages (ffmpeg, Chrome libraries, fonts)"
MISSING=()
for p in ca-certificates curl gnupg tar xz-utils openssl unzip fonts-liberation libnss3 libatk1.0-0 libatk-bridge2.0-0 \
  libcups2 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2; do
  have_pkg "$p" || MISSING+=("$p")
done
have_pkg libasound2 || have_pkg libasound2t64 || { apt-cache show libasound2t64 >/dev/null 2>&1 && MISSING+=(libasound2t64) || MISSING+=(libasound2); }
# ffmpeg may come from another source (static build, snap); only install the package when the tools are absent.
if ! { have_cmd ffmpeg && have_cmd ffprobe; }; then MISSING+=(ffmpeg); fi
if [ -n "$FORCE" ] || [ "${#MISSING[@]}" -gt 0 ]; then
  if [ -n "$FORCE" ]; then
    MISSING=(ca-certificates curl gnupg tar xz-utils openssl unzip ffmpeg fonts-liberation libnss3 libcups2 libdrm2 libxkbcommon0 libgbm1)
  fi
  quiet $SUDO apt-get update -qq
  quiet $SUDO apt-get install -y -qq "${MISSING[@]}"
  did "installed ${MISSING[*]}"
else
  ok "all required packages are already installed ($(ffmpeg -version 2>/dev/null | head -n1 | cut -d' ' -f1-3))"
fi

# --- Node.js ---------------------------------------------------------------------------------------------
step "Node.js"
NODE_MAJOR=0
if have_cmd node; then NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]'); fi
if [ -n "$FORCE" ] || [ "$NODE_MAJOR" -lt 22 ]; then
  [ "$NODE_MAJOR" -gt 0 ] && echo "    found Node.js $(node -v), need 22 or newer: upgrading"
  curl -fsSL https://deb.nodesource.com/setup_22.x -o /tmp/nodesource.sh
  quiet ${SUDO:+$SUDO -E} bash /tmp/nodesource.sh
  quiet $SUDO apt-get install -y -qq nodejs
  did "installed Node.js $(node -v)"
else
  ok "Node.js $(node -v) already installed"
fi
if ! have_cmd npm; then
  quiet $SUDO apt-get install -y -qq npm
  did "installed npm"
fi

# --- hyperframes -----------------------------------------------------------------------------------------
step "hyperframes"
HF_UPGRADED=0
LATEST=$(npm view hyperframes version 2>/dev/null || true)
if have_cmd hyperframes; then
  CUR=$(hyperframes --version 2>/dev/null | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n1 || true)
  if [ -n "$FORCE" ]; then
    quiet $SUDO npm install -g hyperframes; HF_UPGRADED=1; did "reinstalled hyperframes"
  elif [ -z "$LATEST" ] || [ -z "$CUR" ]; then
    ok "hyperframes ${CUR:-installed} (could not check for a newer version)"
  elif ver_ge "$CUR" "$LATEST"; then
    ok "hyperframes $CUR is up to date"
  elif [ "$BUSY" = 1 ]; then
    skip "hyperframes $CUR -> $LATEST upgrade postponed: a render is running (re-run setup when it is done)"
  else
    quiet $SUDO npm install -g hyperframes; HF_UPGRADED=1; did "upgraded hyperframes $CUR -> $LATEST"
  fi
else
  quiet $SUDO npm install -g hyperframes; HF_UPGRADED=1; did "installed hyperframes ${LATEST:-}"
fi

# --- service user and directories ------------------------------------------------------------------------
step "Service user and directories"
id hfr >/dev/null 2>&1 || { $SUDO useradd --system --create-home --home-dir /var/lib/hfr --shell /usr/sbin/nologin hfr; did "created user hfr"; }
$SUDO install -d -o hfr -g hfr /var/lib/hfr/jobs
$SUDO install -d -m 750 -o root -g hfr /etc/hfr
ok "user hfr and /var/lib/hfr ready"

# --- render browser --------------------------------------------------------------------------------------
step "Render browser (Chrome)"
CHROME_PATH=$($SUDO -u hfr -H hyperframes browser path 2>/dev/null | tail -n1 || true)
if [ -z "$FORCE" ] && [ "$HF_UPGRADED" = 0 ] && [ -n "$CHROME_PATH" ] && $SUDO test -x "$CHROME_PATH"; then
  ok "Chrome already present"
else
  # `ensure` itself finds a matching Chrome or downloads one; run it after a hyperframes upgrade too.
  if $SUDO -u hfr -H hyperframes browser ensure >>"$LOG" 2>&1; then did "Chrome is ready"; else echo "warning: 'hyperframes browser ensure' failed; run 'hyperframes doctor' as user hfr" >&2; fi
fi

# --- token -----------------------------------------------------------------------------------------------
# Test as root: /etc/hfr is not readable by the SSH user, so a plain `[ -s ]` would wrongly say "missing".
if ! $SUDO test -s /etc/hfr/agent.env; then
  TOKEN=$(openssl rand -hex 32)
  printf 'HFR_TOKEN=%s\n' "$TOKEN" | $SUDO tee /etc/hfr/agent.env >/dev/null
  $SUDO chown root:hfr /etc/hfr/agent.env
  $SUDO chmod 640 /etc/hfr/agent.env
fi

# --- agent and service -----------------------------------------------------------------------------------
step "hfr-agent"
cat > /tmp/hfr-agent.service.new <<'UNIT'
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
# Hardening: the agent only needs to write under /var/lib/hfr (job data, Chrome cache) and its private /tmp.
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/hfr
PrivateTmp=yes
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes

[Install]
WantedBy=multi-user.target
UNIT
UNIT_FILE=/etc/systemd/system/hfr-agent.service
BIN=/usr/local/bin/hfr-agent
SAME_BIN=0; SAME_UNIT=0
[ -z "$FORCE" ] && [ -x "$BIN" ] && cmp -s /tmp/hfr-agent-upload "$BIN" && SAME_BIN=1
[ -z "$FORCE" ] && [ -f "$UNIT_FILE" ] && cmp -s /tmp/hfr-agent.service.new "$UNIT_FILE" && SAME_UNIT=1

if [ "$SAME_BIN" = 1 ] && [ "$SAME_UNIT" = 1 ]; then
  ok "agent and service are already up to date"
elif [ "$BUSY" = 1 ]; then
  skip "agent update postponed: a render is running and restarting the agent would stop it (re-run setup when it is done)"
elif [ "$LEGACY" -gt 0 ] && [ -x "$BIN" ]; then
  skip "agent update postponed: $LEGACY job(s) from the previous agent version are still on this VPS and would disappear from the list on restart. Download or delete them (hfr pull / hfr rm), then re-run setup"
else
  $SUDO systemctl stop hfr-agent 2>/dev/null || true
  [ "$SAME_BIN" = 1 ] || $SUDO install -m 755 /tmp/hfr-agent-upload "$BIN"
  [ "$SAME_UNIT" = 1 ] || $SUDO install -m 644 /tmp/hfr-agent.service.new "$UNIT_FILE"
  $SUDO systemctl daemon-reload
  did "installed the agent ($([ "$SAME_BIN" = 1 ] || echo binary)$([ "$SAME_BIN" = 0 ] && [ "$SAME_UNIT" = 0 ] && echo ' + ')$([ "$SAME_UNIT" = 1 ] || echo service))"
fi
rm -f /tmp/hfr-agent-upload /tmp/hfr-agent.service.new

# Make sure it is enabled and running (starting an already running service is a no-op).
$SUDO systemctl enable hfr-agent >/dev/null 2>&1 || true
if ! $SUDO systemctl is-active --quiet hfr-agent; then
  $SUDO systemctl start hfr-agent
  sleep 1
fi
$SUDO systemctl is-active --quiet hfr-agent || { $SUDO journalctl -u hfr-agent -n 20 --no-pager >&2; echo "agent failed to start" >&2; exit 1; }
ok "agent is running"

# The client must get the token the RUNNING agent accepts. If the agent was not restarted above (a render was
# running), that is the one in its environment; make the file agree with it so a later restart keeps it.
FILE_TOKEN=$($SUDO sed -n 's/^HFR_TOKEN=//p' /etc/hfr/agent.env)
AGENT_PID=$($SUDO systemctl show -p MainPID --value hfr-agent 2>/dev/null || echo 0)
LIVE_TOKEN=""
if [ -n "$AGENT_PID" ] && [ "$AGENT_PID" != 0 ]; then
  LIVE_TOKEN=$($SUDO cat "/proc/$AGENT_PID/environ" 2>/dev/null | tr '\0' '\n' | sed -n 's/^HFR_TOKEN=//p' || true)
fi
if [ -n "$LIVE_TOKEN" ] && [ "$LIVE_TOKEN" != "$FILE_TOKEN" ]; then
  printf 'HFR_TOKEN=%s\n' "$LIVE_TOKEN" | $SUDO tee /etc/hfr/agent.env >/dev/null
  FILE_TOKEN=$LIVE_TOKEN
  echo "    note: agent.env did not match the running agent; restored it"
fi
echo "HFR_TOKEN=$FILE_TOKEN"
