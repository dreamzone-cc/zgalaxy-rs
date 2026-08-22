#!/bin/bash
# Unified zgalaxy-rs deploy + verify routine for the production container on 161
# (gate B5/B6). Repeatable one-command deploy with volume+binary backup and a
# one-command rollback. ztnet itself is NEVER modified — only read-only checks.
#
# Usage (on 161, as root):
#   deploy_161.sh deploy [binary]   # default binary: /tmp/zgrs2 (musl build)
#   deploy_161.sh verify            # run the ztnet smoke checks only
#   deploy_161.sh rollback [stamp]  # restore binary from backups/<stamp>
#
# REQUIREMENT: the binary must be a musl build — the image's glibc is older
# than the build host's:
#   cargo build --release --target x86_64-unknown-linux-musl
set -u
CT=zerotier
BIN=/usr/sbin/zerotier-one
DATA=/var/lib/zerotier-one
BACKUP_DIR=/home/dz161/backups
STAMP=$(date +%Y%m%d-%H%M%S)
MODE=${1:-deploy}
SRC=${2:-/tmp/zgrs2}

fail() { echo "DEPLOY-FAIL: $*"; exit 1; }

cli() { docker exec "$CT" sh -c "cd $DATA && $BIN rest GET $1" 2>/dev/null; }

verify() {
  echo "== verify: container health"
  docker inspect "$CT" --format '{{.Name}} {{.State.Status}} ({{.State.Health.Status}})' || fail "inspect"
  echo "== verify: daemon log has no errors"
  docker logs "$CT" --since 5m 2>&1 | grep -iE "ERROR|panic" | grep -v "0 errors" | head -5
  echo "== verify: /status"
  ST=$(cli /status) || fail "rest /status unreachable"
  echo "$ST" | grep -q '"address"' || fail "/status missing address: $ST"
  echo "$ST" | grep -E '"address"|"version"|"online"' | head -3
  echo "== verify: /controller/network (must still list pre-existing networks)"
  NW=$(cli /controller/network)
  [ -n "$NW" ] || fail "/controller/network empty/unreachable"
  for N in $(echo "$NW" | grep -oE '[a-f0-9]{16}' | sort -u); do echo "  network: $N"; done
  echo "$NW" | grep -q ef313fb5c9000001 || echo "  WARN: production network ef313fb5c9000001 not listed"
  echo "== verify: member listing of first network"
  FIRST=$(echo "$NW" | grep -oE '[a-f0-9]{16}' | sort -u | head -1)
  M=$(cli "/controller/network/$FIRST/member")
  [ -n "$M" ] || fail "member listing for $FIRST unreachable"
  echo "  members($FIRST): $(echo "$M" | tr -d ' \n' | head -c 120)"
  echo "== verify: local /network"
  cli /network | head -3
  echo "== verify: ztnet container + web UI"
  docker inspect ztnet --format 'ztnet {{.State.Status}}' | grep -q running || fail "ztnet not running"
  docker exec "$CT" sh -c "curl -s -o /dev/null -w 'ztnet-http:%{http_code}\n' --max-time 5 http://172.31.255.3:3000/" || fail "ztnet http"
  echo "VERIFY-PASS"
}

case "$MODE" in
  deploy)
    [ -f "$SRC" ] || fail "binary $SRC not found (musl build required)"
    file "$SRC" 2>/dev/null | grep -q musl || echo "  WARN: $SRC may not be a musl build"
    mkdir -p "$BACKUP_DIR"
    echo "== backup binary"
    OLD_MD5=$(docker exec "$CT" md5sum "$BIN" | awk '{print $1}')
    docker cp "$CT:$BIN" "$BACKUP_DIR/zerotier-one.$STAMP.bak" || fail "binary backup"
    docker exec "$CT" sh -c "tar czf /tmp/volume.tgz -C $DATA ." && \
      docker cp "$CT:/tmp/volume.tgz" "$BACKUP_DIR/ztnet_zerotier-volume.$STAMP.tgz" || fail "volume backup"
    docker exec "$CT" rm -f /tmp/volume.tgz
    echo "  backups: $BACKUP_DIR/zerotier-one.$STAMP.bak (md5 $OLD_MD5), ztnet_zerotier-volume.$STAMP.tgz"
    echo "  rollback: $0 rollback $STAMP"
    echo "== deploy"
    docker cp "$SRC" "$CT:$BIN.new" || fail "copy new binary"
    docker exec "$CT" sh -c "chmod 0755 $BIN.new && mv $BIN.new $BIN" || fail "swap binary"
    docker restart "$CT" >/dev/null || fail "restart"
    sleep 5
    NEW_MD5=$(docker exec "$CT" md5sum "$BIN" | awk '{print $1}')
    SRC_MD5=$(md5sum "$SRC" | awk '{print $1}')
    [ "$NEW_MD5" = "$SRC_MD5" ] || fail "deployed md5 $NEW_MD5 != source $SRC_MD5"
    echo "  deployed md5: $NEW_MD5"
    verify
    ;;
  verify) verify ;;
  rollback)
    S=${2:?usage: rollback <stamp>}
    [ -f "$BACKUP_DIR/zerotier-one.$S.bak" ] || fail "no backup for stamp $S"
    echo "== rollback to $S"
    docker cp "$BACKUP_DIR/zerotier-one.$S.bak" "$CT:$BIN.new" || fail "copy back"
    docker exec "$CT" sh -c "chmod 0755 $BIN.new && mv $BIN.new $BIN" || fail "swap back"
    docker restart "$CT" >/dev/null || fail "restart"
    sleep 5
    echo "  restored md5: $(docker exec "$CT" md5sum "$BIN" | awk '{print $1}')"
    verify
    ;;
  *) echo "usage: $0 {deploy|verify|rollback} [arg]"; exit 2 ;;
esac
