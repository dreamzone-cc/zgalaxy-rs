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

log() { echo "[ZGALAXY-RS] $*"; }

ensure_cargo() {
  if ! command -v cargo >/dev/null 2>&1; then
    log "Installing Rust toolchain..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
}

build_and_install() {
  log "Building ZGALAXY-RS release binary..."
  cargo build --release
  install -m 0755 target/release/zgalaxy-rs "$BIN_DEST"
  ln -sf "$BIN_DEST" /usr/local/bin/zgalaxy-cli
  ln -sf "$BIN_DEST" /usr/local/bin/zgalaxy-idtool

  mkdir -p /var/lib/zerotier-one
}

setup_service() {
  if [ -d /run/systemd/system ]; then
    log "Registering and starting systemd service..."
    install -m 0644 zgalaxy-client.service "$SERVICE_DEST"
    systemctl daemon-reload
    systemctl enable --now zgalaxy-client.service
    log "Service active and running ✓"
  fi
}

ensure_cargo
build_and_install
setup_service

log "Installation complete! Check status with: zgalaxy-cli status"
