#!/usr/bin/env bash
# =============================================================================
# ZGALAXY-RS — One-Line Sovereign Rust Client Installer
# =============================================================================
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "[ERROR] Please run as root: sudo bash install.sh"
  exit 1
fi

BIN_DEST="/usr/local/bin/zgalaxy-rs"
SERVICE_DEST="/etc/systemd/system/zgalaxy-client.service"

MODE="client"
INSTALL_DESKTOP=true

for arg in "$@"; do
  case "$arg" in
    --controller|--mode=controller|-C)
      MODE="controller"
      ;;
    --client|--mode=client)
      MODE="client"
      ;;
    --headless|--no-desktop)
      INSTALL_DESKTOP=false
      ;;
    *)
      ;;
  esac
done

log() { echo "[ZGALAXY-RS] $*"; }

ensure_cargo() {
  if [ ! -f target/release/zgalaxy-rs ]; then
    if ! command -v cargo >/dev/null 2>&1; then
      log "Installing Rust toolchain..."
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
      export PATH="$HOME/.cargo/bin:$PATH"
    fi
    log "Building ZGALAXY-RS release binary..."
    cargo build --release
  fi
}

build_and_install() {
  if [ -f target/release/zgalaxy-rs ]; then
    log "Using pre-built target/release/zgalaxy-rs binary..."
  else
    ensure_cargo
  fi

  log "Installing binary to ${BIN_DEST}..."
  install -m 0755 target/release/zgalaxy-rs "$BIN_DEST"
  ln -sf "$BIN_DEST" /usr/local/bin/zgalaxy-cli
  ln -sf "$BIN_DEST" /usr/local/bin/zgalaxy-idtool

  mkdir -p /var/lib/zerotier-one

  if [ "$MODE" = "controller" ]; then
    log "Configuring local.conf for Controller Mode (ZTNet)..."
    cat <<'EOF' > /var/lib/zerotier-one/local.conf
{
  "settings": {
    "controller": true,
    "allowManagementFrom": ["127.0.0.1", "::1"]
  }
}
EOF
  fi
}

setup_service() {
  if [ -d /run/systemd/system ]; then
    log "Registering and starting systemd service (Mode: ${MODE})..."
    local EXEC_CMD="/usr/local/bin/zgalaxy-rs"
    if [ "$MODE" = "controller" ]; then
      EXEC_CMD="/usr/local/bin/zgalaxy-rs --controller"
    fi

    cat <<EOF > "$SERVICE_DEST"
[Unit]
Description=ZGALAXY Sovereign ZeroTier Client Daemon (Rust) - ${MODE} Mode
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${EXEC_CMD}
Restart=always
RestartSec=3
User=root
LimitNOFILE=65536
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now zgalaxy-client.service
    log "Service active and running (${MODE} mode) ✓"
  fi
}

# Desktop client (zgalaxy-ui): applications-menu shortcut + login autostart.
# - Menu category: Utility (deliberately NOT Internet).
# - Autostart: /etc/xdg/autostart (session start for every user). The same
#   desktop-file ID also lives in ~/.config/autostart when the user toggles
#   the in-app preference — XDG treats identical IDs as an override, never a
#   duplicate launch.
install_desktop() {
  local UI_BIN=""
  for c in target/release/zgalaxy-ui target/debug/zgalaxy-ui \
           zgalaxy-ui/target/release/zgalaxy-ui zgalaxy-ui/target/debug/zgalaxy-ui; do
    [ -x "$c" ] && UI_BIN="$c" && break
  done
  if [ -z "$UI_BIN" ]; then
    log "Desktop client binary not found (build zgalaxy-ui first) — skipping menu/autostart."
    return 0
  fi

  log "Installing desktop client to /usr/local/bin/zgalaxy-ui..."
  install -m 0755 "$UI_BIN" /usr/local/bin/zgalaxy-ui

  log "Installing application icon..."
  if [ -f zgalaxy-ui/ui/assets/app-icon-gold.png ]; then
    install -D -m 0644 zgalaxy-ui/ui/assets/app-icon-gold.png \
      /usr/share/icons/hicolor/256x256/apps/zgalaxy.png
    install -m 0644 zgalaxy-ui/ui/assets/app-icon-gold.png /usr/share/pixmaps/zgalaxy.png
  fi

  log "Installing applications-menu shortcut (category: Utility)..."
  install -m 0644 zgalaxy-ui.desktop /usr/share/applications/zgalaxy-ui.desktop

  log "Enabling start-at-login (system-wide autostart)..."
  install -m 0644 zgalaxy-ui.desktop /etc/xdg/autostart/zgalaxy-ui.desktop

  if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate /usr/share/applications/zgalaxy-ui.desktop \
      || log "WARN: desktop file validation reported issues"
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications 2>/dev/null || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qf /usr/share/icons/hicolor 2>/dev/null || true
  fi
  log "Menu shortcut + login autostart installed ✓"
}

build_and_install
setup_service
if [ "$INSTALL_DESKTOP" = "true" ] && [ "$MODE" = "client" ]; then
  install_desktop
fi

log "Installation complete! Mode: ${MODE}. Check status with: zgalaxy-cli status"
