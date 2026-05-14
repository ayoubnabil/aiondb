#!/usr/bin/env bash
# benchmarks/pg-local.sh — manage a local disposable PostgreSQL cluster for benchmarks.
#
# Usage:
#   benchmarks/pg-local.sh start|stop|restart|status
#
# Env:
#   PG_BIN_DIR=/usr/lib/postgresql/16/bin
#   PG_LOCAL_DIR=benchmarks/.state/pg-local
#   PG_LOCAL_PORT=55432
#   PG_LOCAL_HOST=127.0.0.1
#   PG_LOCAL_USER=postgres
#   PG_LOCAL_EXTRA_OPTS="-c fsync=off -c synchronous_commit=off"

set -euo pipefail

BENCH_ROOT="$(cd "$(dirname "$0")" && pwd)"
PG_BIN_DIR="${PG_BIN_DIR:-/usr/lib/postgresql/16/bin}"
INITDB="$PG_BIN_DIR/initdb"
PG_CTL="$PG_BIN_DIR/pg_ctl"
PSQL="$PG_BIN_DIR/psql"

PG_LOCAL_DIR="${PG_LOCAL_DIR:-$BENCH_ROOT/.state/pg-local}"
PG_LOCAL_PORT="${PG_LOCAL_PORT:-55432}"
PG_LOCAL_HOST="${PG_LOCAL_HOST:-127.0.0.1}"
PG_LOCAL_USER="${PG_LOCAL_USER:-postgres}"
PG_LOCAL_LOG="${PG_LOCAL_LOG:-$BENCH_ROOT/.state/pg-local.log}"
PG_LOCAL_EXTRA_OPTS="${PG_LOCAL_EXTRA_OPTS:-}"

log() { printf '[pg-local] %s\n' "$*" >&2; }

die() { printf '[pg-local][FATAL] %s\n' "$*" >&2; exit 1; }

require_bin() {
  [[ -x "$1" ]] || die "missing binary: $1"
}

is_running() {
  "$PG_CTL" -D "$PG_LOCAL_DIR" status >/dev/null 2>&1
}

ensure_init() {
  require_bin "$INITDB"
  require_bin "$PG_CTL"
  require_bin "$PSQL"
  mkdir -p "$(dirname "$PG_LOCAL_LOG")"
  if [[ ! -f "$PG_LOCAL_DIR/PG_VERSION" ]]; then
    log "initializing cluster in $PG_LOCAL_DIR"
    rm -rf "$PG_LOCAL_DIR"
    "$INITDB" -D "$PG_LOCAL_DIR" -U "$PG_LOCAL_USER" -A trust >/dev/null
  fi
}

start_pg() {
  ensure_init
  if is_running; then
    log "already running"
    return 0
  fi
  log "starting postgres on ${PG_LOCAL_HOST}:${PG_LOCAL_PORT}"
  mkdir -p "$PG_LOCAL_DIR/socket"
  "$PG_CTL" -D "$PG_LOCAL_DIR" -l "$PG_LOCAL_LOG" \
    -o "-h $PG_LOCAL_HOST -p $PG_LOCAL_PORT -k $PG_LOCAL_DIR/socket $PG_LOCAL_EXTRA_OPTS" start >/dev/null
  # smoke check
  "$PSQL" -h "$PG_LOCAL_HOST" -p "$PG_LOCAL_PORT" -U "$PG_LOCAL_USER" -d postgres \
    --no-psqlrc -tAc 'select 1' >/dev/null
  log "started"
}

stop_pg() {
  require_bin "$PG_CTL"
  if ! is_running; then
    log "not running"
    return 0
  fi
  log "stopping"
  "$PG_CTL" -D "$PG_LOCAL_DIR" stop -m fast >/dev/null || true
  log "stopped"
}

status_pg() {
  require_bin "$PG_CTL"
  if is_running; then
    log "running"
    "$PSQL" -h "$PG_LOCAL_HOST" -p "$PG_LOCAL_PORT" -U "$PG_LOCAL_USER" -d postgres \
      --no-psqlrc -tAc 'select version();' || true
  else
    log "stopped"
  fi
}

cmd="${1:-status}"
case "$cmd" in
  start) start_pg ;;
  stop) stop_pg ;;
  restart) stop_pg; start_pg ;;
  status) status_pg ;;
  *) die "unknown command: $cmd" ;;
esac
