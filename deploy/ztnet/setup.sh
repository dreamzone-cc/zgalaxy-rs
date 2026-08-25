#!/bin/bash
# =============================================================================
# setup.sh — ZTNET stack powered by zgalaxy-rs (zerotier fully replaced)
# -----------------------------------------------------------------------------
# One-command automation for running the ztnet web UI on top of zgalaxy-rs
# instead of zerotier-one. Two paths, same end state:
#
#   fresh          Brand-new stack (postgres + zgalaxy-rs + official ztnet).
#                  zerotier is never installed anywhere.
#   migrate        Existing ztnet deployment running the original
#                  zyclonite/zerotier image: swap the engine to zgalaxy-rs
#                  in place — identity, networks and members preserved.
#
# Operational subcommands:
#   update-binary  Swap a new musl zgalaxy-rs binary into the running stack
#                  (backup binary + volume first, verify after).
#   verify         Full health check incl. the ztnet→controller call path.
#   rollback       Restore a binary backup created by update-binary.
#
# Requirements:
#   - Docker (+ compose plugin). Run as root, with passwordless sudo, or set
#     SUDO_PASS=<password> in the environment.
#   - A musl build of zgalaxy-rs:
#       cargo build --release --target x86_64-unknown-linux-musl
#     Provide it via --bin <path>. On a machine with the repo + cargo you can
#     instead pass --repo <repo-path> and the script builds it.
#
# Examples:
#   SUDO_PASS=... ./setup.sh fresh --bin /tmp/zgrs4 --url http://192.168.1.161:3000
#   SUDO_PASS=... ./setup.sh fresh --repo ~/zgalaxy-work/zgalaxy-rs --url http://10.0.0.5:3000
#   SUDO_PASS=... ./setup.sh migrate --dir ~/ztnet --bin /tmp/zgrs4
#   SUDO_PASS=... ./setup.sh update-binary --bin /tmp/zgrs5
#   ./setup.sh verify
#   ./setup.sh rollback 20260823-181253
# =============================================================================
set -u

CT=zerotier
IMAGE=zgalaxy-zerotier:rs
BIN=/usr/sbin/zerotier-one
DATA=/var/lib/zerotier-one
BACKUP_DIR="${HOME}/backups"
# Management CIDR: must match the compose network subnet (172.31.255.0/29).
MANAGE_CIDR="${ZTNET_MANAGE_CIDR:-172.31.255.0/29}"
SELF_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SELF_DIR/../.." && pwd)"

fail() { echo "SETUP-FAIL: $*"; exit 1; }
info() { echo "== $*"; }

# --- docker wrapper: root / passwordless sudo / SUDO_PASS -------------------
if docker info >/dev/null 2>&1; then
  dk() { docker "$@"; }
elif [ -n "${SUDO_PASS:-}" ]; then
  dk() { echo "$SUDO_PASS" | sudo -S docker "$@" 2>/dev/null; }
elif sudo -n docker info >/dev/null 2>&1; then
  dk() { sudo docker "$@"; }
else
  fail "docker needs privileges: run as root, configure passwordless sudo, or set SUDO_PASS"
fi

# --- shared helpers ----------------------------------------------------------
ctl() { # ctl <rest-path>  — controller REST from inside the engine container
  dk exec "$CT" sh -c "cd $DATA && $BIN rest GET '$1'" 2>/dev/null
}

wait_healthy() { # wait_healthy <container> <max-seconds>
  local c=$1 max=${2:-90} i=0
  while [ $i -lt $max ]; do
    st=$(dk inspect "$c" --format '{{.State.Health.Status}}' 2>/dev/null)
    [ "$st" = "healthy" ] && return 0
    sleep 3; i=$((i+3))
  done
  return 1
}

write_local_conf() {
  info "writing local.conf (allowManagementFrom=$MANAGE_CIDR)"
  # Canonical ZeroTier format — allowManagementFrom lives under `settings`.
  # zgalaxy-rs honors both shapes, but this one matches upstream docs.
  # Write via temp file + docker cp: immune to nested-quoting corruption.
  local tmp; tmp=$(mktemp)
  cat > "$tmp" <<EOF
{
  "settings": {
    "portMappingEnabled": true,
    "softwareUpdate": "disable",
    "allowManagementFrom": ["$MANAGE_CIDR"]
  }
}
EOF
  dk cp "$tmp" "$CT:$DATA/local.conf" || { rm -f "$tmp"; fail "write local.conf"; }
  rm -f "$tmp"
  dk exec "$CT" sh -c "cat $DATA/local.conf" || fail "local.conf unreadable"
}

resolve_binary() { # resolves $BIN_PATH from --bin/--repo
  if [ -n "${OPT_BIN:-}" ]; then
    [ -f "$OPT_BIN" ] || fail "binary not found: $OPT_BIN (must be a musl build)"
    BIN_PATH="$OPT_BIN"
  elif [ -n "${OPT_REPO:-}" ]; then
    [ -d "$OPT_REPO" ] || fail "repo not found: $OPT_REPO"
    info "building musl binary from $OPT_REPO"
    (cd "$OPT_REPO" && cargo build --release --target x86_64-unknown-linux-musl) \
      || fail "cargo build failed"
    BIN_PATH="$OPT_REPO/target/x86_64-unknown-linux-musl/release/zgalaxy-rs"
  elif [ -f "$REPO_ROOT/target/x86_64-unknown-linux-musl/release/zgalaxy-rs" ]; then
    BIN_PATH="$REPO_ROOT/target/x86_64-unknown-linux-musl/release/zgalaxy-rs"
  else
    fail "no binary: pass --bin <musl-binary> or --repo <zgalaxy-rs-path>"
  fi
  info "using binary: $BIN_PATH ($(md5sum "$BIN_PATH" | awk '{print $1}'))"
}

ensure_image() { # build zgalaxy-zerotier:rs from the repo containing this script
  if dk image inspect "$IMAGE" >/dev/null 2>&1 && [ -z "${OPT_REBUILD:-}" ]; then
    info "image $IMAGE already present"
    return
  fi
  info "building image $IMAGE (context: $REPO_ROOT)"
  dk build -f "$SELF_DIR/Dockerfile" -t "$IMAGE" "$REPO_ROOT" || fail "image build"
}

verify_stack() {
  info "verify: containers"
  for c in "$CT" ztnet postgres; do
    dk inspect "$c" --format "  $c {{.State.Status}}" >/dev/null 2>&1 || fail "$c missing"
  done
  info "verify: engine health + /status"
  wait_healthy "$CT" 60 || fail "engine container not healthy"
  local st; st=$(ctl /status) || fail "rest /status unreachable"
  echo "$st" | grep -q '"address"' || fail "/status malformed: $st"
  echo "$st" | grep -E '"address"|"online"|"version"' | head -3
  info "verify: controller networks"
  local nw; nw=$(ctl /controller/network) || fail "controller REST unreachable"
  [ -n "$nw" ] || fail "network listing empty"
  echo "$nw" | tr ',' '\n' | grep -oE '[a-f0-9]{16}' | sed 's/^/  network: /'
  info "verify: ztnet web UI"
  local code; code=$(dk exec "$CT" sh -c \
    "curl -s -o /dev/null -w '%{http_code}' --max-time 5 http://ztnet:3000/" 2>/dev/null)
  case "$code" in 200|302|307|308) echo "  ztnet-http:$code" ;; *) fail "ztnet http:$code" ;; esac
  info "verify: ztnet→controller call path (the path that broke with allowManagementFrom)"
  local t; t=$(dk exec "$CT" sh -c "cat $DATA/authtoken.secret" 2>/dev/null | tr -d '\n')
  local out; out=$(dk exec -e TOK="$t" ztnet node -e \
    "fetch('http://zerotier:9993/status',{headers:{'X-ZT1-Auth':process.env.TOK}}).then(r=>r.status).then(s=>{console.log('ztnet-to-controller HTTP '+s);process.exit(s===200?0:1)}).catch(e=>{console.log('FAIL '+e.message);process.exit(1)})" 2>&1)
  echo "  $out"
  echo "$out" | grep -q "HTTP 200" || fail "ztnet cannot reach the controller"
  info "verify: engine log has no refusals/errors"
  local n; n=$(dk logs "$CT" --since 5m 2>&1 | grep -ciE "refused|ERROR|panic")
  [ "$n" = "0" ] || fail "engine log shows $n refused/error lines"
  echo "VERIFY-PASS"
}

# --- option parsing ----------------------------------------------------------
MODE=${1:-}; shift || true
OPT_BIN=""; OPT_REPO=""; OPT_DIR=""; OPT_URL=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bin)    OPT_BIN="$2"; shift 2 ;;
    --repo)   OPT_REPO="$2"; shift 2 ;;
    --dir)    OPT_DIR="$2"; shift 2 ;;
    --url)    OPT_URL="$2"; shift 2 ;;
    --rebuild) OPT_REBUILD=1; shift ;;
    *) fail "unknown option: $1" ;;
  esac
done

# =============================================================================
case "$MODE" in

# ---------------------------------------------------------------------------
fresh)
  # Full new stack: postgres + zgalaxy-rs (as `zerotier` service) + ztnet.
  # ---------------------------------------------------------------------------
  resolve_binary
  ensure_image

  STACK_DIR="${OPT_DIR:-$HOME/ztnet-zgalaxy}"
  info "provisioning stack directory: $STACK_DIR"
  mkdir -p "$STACK_DIR"

  if [ ! -f "$STACK_DIR/.env" ] || ! grep -q NEXTAUTH_SECRET "$STACK_DIR/.env" 2>/dev/null; then
    SECRET=$(openssl rand -base64 32 2>/dev/null || head -c 32 /dev/urandom | base64)
    [ -n "${OPT_URL}" ] || fail "pass --url http://<server-ip>:3000 on first install"
    cat > "$STACK_DIR/.env" <<EOF
NEXTAUTH_URL=$OPT_URL
NEXTAUTH_SECRET=$SECRET
EOF
    chmod 600 "$STACK_DIR/.env"
    info "generated .env (NEXTAUTH_URL=$OPT_URL)"
  fi

  info "starting stack (this builds the image on first run)"
  dk compose -p ztnet -f "$SELF_DIR/docker-compose.yml" --env-file "$STACK_DIR/.env" \
    up -d --build || fail "compose up"

  info "waiting for engine health"
  wait_healthy "$CT" 120 || fail "engine not healthy after 120s"

  write_local_conf
  dk restart "$CT" >/dev/null || fail "restart after local.conf"
  sleep 5
  wait_healthy "$CT" 60 || fail "engine not healthy after local.conf reload"

  verify_stack
  info "DONE — ztnet is on http://<server>:3000 running over zgalaxy-rs."
  echo "   First visit: create the ztnet admin account in the browser."
  echo "   Local controller appears automatically (ZT_ADDR=http://zerotier:9993)."
  ;;

# ---------------------------------------------------------------------------
migrate)
  # In-place engine swap on an existing zyclonite/zerotier-based ztnet stack.
  # Identity, networks and members are preserved (same named volume).
  # ---------------------------------------------------------------------------
  STACK_DIR="${OPT_DIR:-$HOME/ztnet}"
  COMPOSE="$STACK_DIR/docker-compose.yml"
  [ -f "$COMPOSE" ] || fail "no docker-compose.yml in $STACK_DIR (pass --dir)"
  resolve_binary
  ensure_image

  info "recording current node identity (must survive the swap)"
  OLD_ADDR=$(dk exec "$CT" sh -c "cd $DATA && cat identity.public" 2>/dev/null \
    | head -c 10)
  [ -n "$OLD_ADDR" ] || fail "cannot read current identity from $CT"

  STAMP=$(date +%Y%m%d-%H%M%S)
  mkdir -p "$BACKUP_DIR"
  info "backup: compose file + full engine volume (stamp $STAMP)"
  cp "$COMPOSE" "$BACKUP_DIR/docker-compose.pre-zgalaxy.$STAMP.yml"
  dk exec "$CT" sh -c "tar czf /tmp/volume.tgz -C $DATA ." 2>/dev/null
  dk cp "$CT:/tmp/volume.tgz" "$BACKUP_DIR/zerotier-volume.$STAMP.tgz" || fail "volume backup"
  dk exec "$CT" rm -f /tmp/volume.tgz
  echo "  rollback volume: $BACKUP_DIR/zerotier-volume.$STAMP.tgz"

  info "swapping the engine image in $COMPOSE"
  if grep -q "zgalaxy-zerotier:rs" "$COMPOSE"; then
    echo "  compose already points at zgalaxy — nothing to swap"
  else
    sed -i.bak-zgalaxy 's|image: *zyclonite/zerotier:.*|image: zgalaxy-zerotier:rs|' "$COMPOSE" \
      || fail "sed on compose"
    grep -q "zgalaxy-zerotier:rs" "$COMPOSE" || fail "no zyclonite image line found to replace"
  fi

  info "recreating the engine container (volume preserved)"
  (cd "$STACK_DIR" && dk compose up -d zerotier) || fail "compose up zerotier"
  wait_healthy "$CT" 120 || fail "engine not healthy after swap"

  write_local_conf
  dk restart "$CT" >/dev/null
  sleep 5
  wait_healthy "$CT" 60 || fail "engine not healthy after reload"

  NEW_ADDR=$(ctl /status | grep -oE '"address": *"[a-f0-9]{10}"' | grep -oE '[a-f0-9]{10}')
  [ "$OLD_ADDR" = "$NEW_ADDR" ] || fail "IDENTITY CHANGED: $OLD_ADDR -> $NEW_ADDR (restore $BACKUP_DIR/zerotier-volume.$STAMP.tgz)"
  info "identity preserved: $NEW_ADDR"

  verify_stack
  info "DONE — existing ztnet deployment now runs on zgalaxy-rs."
  ;;

# ---------------------------------------------------------------------------
update-binary)
  # Swap the engine binary inside the running zgalaxy container.
  # ---------------------------------------------------------------------------
  resolve_binary
  STAMP=$(date +%Y%m%d-%H%M%S)
  mkdir -p "$BACKUP_DIR"
  info "backup binary + volume (stamp $STAMP)"
  OLD_MD5=$(dk exec "$CT" md5sum "$BIN" | awk '{print $1}')
  dk cp "$CT:$BIN" "$BACKUP_DIR/zerotier-one.$STAMP.bak" || fail "binary backup"
  dk exec "$CT" sh -c "tar czf /tmp/volume.tgz -C $DATA ." 2>/dev/null
  dk cp "$CT:/tmp/volume.tgz" "$BACKUP_DIR/ztnet_zerotier-volume.$STAMP.tgz" || fail "volume backup"
  dk exec "$CT" rm -f /tmp/volume.tgz
  echo "  rollback: $0 rollback $STAMP"

  info "deploying new binary"
  dk cp "$BIN_PATH" "$CT:$BIN.new" || fail "copy"
  dk exec "$CT" sh -c "chmod 0755 $BIN.new && mv $BIN.new $BIN" || fail "swap"
  dk restart "$CT" >/dev/null || fail "restart"
  sleep 5
  wait_healthy "$CT" 60 || fail "not healthy after update"

  NEW_MD5=$(dk exec "$CT" md5sum "$BIN" | awk '{print $1}')
  SRC_MD5=$(md5sum "$BIN_PATH" | awk '{print $1}')
  [ "$NEW_MD5" = "$SRC_MD5" ] || fail "md5 mismatch $NEW_MD5 != $SRC_MD5"
  echo "  deployed md5: $NEW_MD5"
  verify_stack
  ;;

# ---------------------------------------------------------------------------
verify)  verify_stack ;;
rollback)
  STAMP=${1:?usage: rollback <stamp>}
  [ -f "$BACKUP_DIR/zerotier-one.$STAMP.bak" ] || fail "no backup for stamp $STAMP"
  info "rolling engine binary back to $STAMP"
  dk cp "$BACKUP_DIR/zerotier-one.$STAMP.bak" "$CT:$BIN.new" || fail "copy back"
  dk exec "$CT" sh -c "chmod 0755 $BIN.new && mv $BIN.new $BIN" || fail "swap back"
  dk restart "$CT" >/dev/null || fail "restart"
  sleep 5
  wait_healthy "$CT" 60 || fail "not healthy after rollback"
  echo "  restored md5: $(dk exec "$CT" md5sum "$BIN" | awk '{print $1}')"
  verify_stack
  ;;

*) sed -n '2,30p' "$0"; exit 2 ;;
esac
