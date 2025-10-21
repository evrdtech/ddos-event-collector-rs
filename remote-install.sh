#!/usr/bin/env bash
set -euo pipefail

# =====================================================
#  remote-install.sh
#  Install ddos-event-collector remotely under systemd
#  (interactive sudo on remote; no local sudo password kept)
# =====================================================

# === CONFIG ===
#  LOCAL_BINARY="target/release/ddos-event-collector"
LOCAL_BINARY="target/release.rl9/release/ddos-event-collector"
INSTALL_DIR="/opt/evrd"
REMOTE_BIN_PATH="$INSTALL_DIR/ddos-collector"
SERVICE_NAME="ddos-collector.service"
SERVICE_PATH="/etc/systemd/system/$SERVICE_NAME"
SERVICE_USER="everest"
SERVICE_GROUP="everest"
API_PORT_FIXED="9000"

# === SCRIPT START ===
REMOTE="${1:-}"
if [ -z "$REMOTE" ]; then
  echo "Usage: $0 <remote_user@remote_host>" >&2
  exit 1
fi

if [ ! -f "$LOCAL_BINARY" ]; then
  echo "❌ Local binary not found: $LOCAL_BINARY" >&2
  exit 2
fi

LOG_FILE="remote-install_$(date +%Y-%m-%d_%H-%M-%S).log"
# Log everything locally (but NOT passwords — we won't capture any)
exec > >(tee -a "$LOG_FILE") 2>&1

echo "========================================"
echo "🚀 Remote Installer - DDoS Collector"
echo "Target: $REMOTE"
echo "Local binary: $LOCAL_BINARY"
echo "Remote path: $REMOTE_BIN_PATH"
echo "Service user: $SERVICE_USER"
echo "API_PORT: $API_PORT_FIXED"
echo "Local log file: $LOG_FILE"
echo "========================================"
echo

echo "📤 Uploading binary to ${REMOTE}:/tmp/ddos-event-collector ..."
if ! scp -q "$LOCAL_BINARY" "${REMOTE}:/tmp/ddos-event-collector"; then
  echo "❌ ERROR: scp upload failed." >&2
  exit 3
fi
echo "✅ Upload complete."

echo "⚙️  Building remote script (will be placed at /tmp/remote-install.sh on remote)..."

# Build remote script as a template and substitute only placeholders we intend to expand.
REMOTE_TEMPLATE=$(cat <<'TEMPLATE'
#!/usr/bin/env bash
set -euo pipefail

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "ERROR: required command '$1' is not available on the remote host." >&2
    exit 5
  fi
}

log "Remote: starting installation."

# Check core dependencies (best-effort; 'strings' may not exist)
for tool in systemctl useradd install; do
  require_command "$tool"
  log "Dependency check: '$tool' available."
done

# NOTE: 'strings' may be missing; it's optional because we have fallbacks
if command -v strings >/dev/null 2>&1; then
  log "Optional helper 'strings' found."
else
  log "Optional helper 'strings' not found; GLIBC check will try alternate tools."
fi

NOLOGIN="/usr/sbin/nologin"
[ -x "$NOLOGIN" ] || NOLOGIN="/sbin/nologin"
[ -x "$NOLOGIN" ] || NOLOGIN="/bin/false"

# --- User setup ---
if ! id -u "@@SERVICE_USER@@" >/dev/null 2>&1; then
  log "Creating system user '@@SERVICE_USER@@'..."
  useradd -r -s "$NOLOGIN" -M -N "@@SERVICE_USER@@" >/dev/null 2>&1 || useradd -s "$NOLOGIN" -M -N "@@SERVICE_USER@@" || true
  log "User '@@SERVICE_USER@@' created."
else
  log "User '@@SERVICE_USER@@' already exists."
fi

# --- Directory setup ---
log "Preparing application directory at @@INSTALL_DIR@@"
mkdir -p "@@INSTALL_DIR@@"
chown root:root "@@INSTALL_DIR@@"
chmod 0755 "@@INSTALL_DIR@@"

if [ ! -f /tmp/ddos-event-collector ]; then
  echo "ERROR: uploaded file /tmp/ddos-event-collector not found." >&2
  exit 4
fi

# --- Backup existing binary ---
if [ -f "@@REMOTE_BIN_PATH@@" ]; then
  TS=$(date +%s)
  log "Backing up existing binary -> @@REMOTE_BIN_PATH@@.bak.$TS"
  cp -v "@@REMOTE_BIN_PATH@@" "@@REMOTE_BIN_PATH@@.bak.$TS"
fi

# Ensure sane perms before moving
find "@@INSTALL_DIR@@" -type d -exec chmod 0755 {} + || true
find "@@INSTALL_DIR@@" -type f -exec chmod 0644 {} + || true
chown -R "@@SERVICE_USER@@:@@SERVICE_GROUP@@" "@@INSTALL_DIR@@"

log "Moving /tmp/ddos-event-collector -> @@REMOTE_BIN_PATH@@"
install -m 0755 /tmp/ddos-event-collector "@@REMOTE_BIN_PATH@@"
chown "@@SERVICE_USER@@:@@SERVICE_GROUP@@" "@@REMOTE_BIN_PATH@@"
log "Binary deployed to @@REMOTE_BIN_PATH@@"

# --- GLIBC version check (try several methods) ---
probe_glibc() {
  local bin="$1"
  # try strings
  if command -v strings >/dev/null 2>&1; then
    strings "$bin" 2>/dev/null | grep -Eo 'GLIBC_[0-9]+\.[0-9]+' | sort -u || true
    return 0
  fi
  # try readelf -V
  if command -v readelf >/dev/null 2>&1; then
    readelf -V "$bin" 2>/dev/null | grep -Eo 'GLIBC_[0-9]+\.[0-9]+' | sort -u || true
    return 0
  fi
  # try objdump -p
  if command -v objdump >/dev/null 2>&1; then
    objdump -p "$bin" 2>/dev/null | grep -Eo 'GLIBC_[0-9]+\.[0-9]+' | sort -u || true
    return 0
  fi
  # fallback: no reliable way to inspect
  return 1
}

log "Checking GLIBC symbols required by the binary (best-effort)..."
REQ_GLIBC=$(probe_glibc "@@REMOTE_BIN_PATH@@" || true)
if [ -n "$REQ_GLIBC" ]; then
  printf '%s\n' "$REQ_GLIBC" | sed -n '1,10p'
  # Determine libc path
  LIBC_PATH=""
  [ -f /lib64/libc.so.6 ] && LIBC_PATH=/lib64/libc.so.6
  [ -z "$LIBC_PATH" ] && [ -f /lib/libc.so.6 ] && LIBC_PATH=/lib/libc.so.6
  if [ -n "$LIBC_PATH" ]; then
    SYS_GLIBC=$(probe_glibc "$LIBC_PATH" || true)
    MISSING=""
    for v in $REQ_GLIBC; do
      if ! printf '%s\n' "$SYS_GLIBC" | grep -Fxq "$v"; then
        MISSING="$MISSING $v"
      fi
    done
    if [ -n "$MISSING" ]; then
      echo "ERROR: required GLIBC symbols not present on remote:$MISSING" >&2
      echo "Installation aborted to avoid enabling a non-working service." >&2
      exit 6
    fi
  else
    log "Could not locate libc on standard paths to validate GLIBC versions; skipping strict check."
  fi
else
  log "Could not extract GLIBC requirements (no probe tool available); skipping strict check."
fi

# --- ExecStart building ---
EXEC_START="/bin/bash -c 'API_PORT=@@API_PORT@@ @@REMOTE_BIN_PATH@@'"

log "Writing systemd unit to @@SERVICE_PATH@@"
cat > "@@SERVICE_PATH@@" <<SERVICE_EOF
[Unit]
Description=Webhook API Service - Wanguard Event Collector (by Everest)
After=network.target

[Service]
User=@@SERVICE_USER@@
Group=@@SERVICE_GROUP@@
WorkingDirectory=@@INSTALL_DIR@@
ExecStart=$EXEC_START
Restart=always
RestartSec=5s

# Security Hardening
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
ProtectHome=yes
ProtectControlGroups=yes
ProtectKernelTunables=yes
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
SERVICE_EOF

log "Reloading systemd and enabling service..."
systemctl daemon-reload
systemctl enable --now "@@SERVICE_NAME@@"

log "Checking service status..."
systemctl status "@@SERVICE_NAME@@" --no-pager || true

log "✅ Remote installation complete."
TEMPLATE
)

# Replace placeholders with local values (safe)
REMOTE_SCRIPT="$REMOTE_TEMPLATE"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@SERVICE_USER@@/$SERVICE_USER}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@SERVICE_GROUP@@/$SERVICE_GROUP}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@INSTALL_DIR@@/$INSTALL_DIR}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@REMOTE_BIN_PATH@@/$REMOTE_BIN_PATH}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@SERVICE_PATH@@/$SERVICE_PATH}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@SERVICE_NAME@@/$SERVICE_NAME}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//@@API_PORT@@/$API_PORT_FIXED}"

# Save generated remote script locally for inspection (private perms)
REMOTE_SCRIPT_PATH="remote-script-$(date +%Y-%m-%d_%H-%M-%S).sh"
printf '%s\n' "$REMOTE_SCRIPT" > "$REMOTE_SCRIPT_PATH"
chmod 700 "$REMOTE_SCRIPT_PATH"
echo "📝 Remote script saved to: $REMOTE_SCRIPT_PATH (also uploaded to remote)"

# Upload the generated script to remote /tmp (so it can be executed there)
scp -q "$REMOTE_SCRIPT_PATH" "${REMOTE}:/tmp/remote-install.sh"
if [ $? -ne 0 ]; then
  echo "❌ Failed to upload the remote script to $REMOTE:/tmp/remote-install.sh" >&2
  exit 1
fi
echo "📤 Remote script uploaded to $REMOTE:/tmp/remote-install.sh"

# Execute the uploaded script on the remote as root, *interactive sudo*
# This way sudo will prompt on the remote terminal (secure) and we never store the password locally.
echo "🔐 Now executing remote installer as root. You will be prompted for the remote sudo password on the remote host."
ssh -t "$REMOTE" "sudo bash /tmp/remote-install.sh"
SSH_EXIT=$?
if [ $SSH_EXIT -ne 0 ]; then
  echo "❌ Remote installation failed (ssh exit code $SSH_EXIT)." >&2
  echo "👉 Inspect the uploaded script on remote: /tmp/remote-install.sh"
  echo "👉 Inspect local generated script: $REMOTE_SCRIPT_PATH"
  exit $SSH_EXIT
fi

echo
echo "✅ Installation finished successfully."
echo "📄 Local log saved at: $LOG_FILE"
echo "🔍 To check logs remotely:"
echo "  ssh $REMOTE sudo journalctl -u $SERVICE_NAME -f"
