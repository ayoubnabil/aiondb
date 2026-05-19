#!/usr/bin/env bash
# benchmarks/surreal-suite/run.sh — SurrealDB 3 article-style benchmark matrix.
#
# Matrix:
#   - surrealdb: SurrealDB over WebSocket
#   - aiondb:    AionDB over PostgreSQL wire
#   - pgstack:   PostgreSQL with pgvector and Apache AGE/Cypher when installed

set -euo pipefail

# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

SURREAL_BIN="${SURREAL_BIN:-$(command -v surreal || true)}"
if [[ -z "$SURREAL_BIN" && -x "$HOME/.surrealdb/surreal" ]]; then
    SURREAL_BIN="$HOME/.surrealdb/surreal"
fi

SURREAL_HOST="${SURREAL_HOST:-127.0.0.1}"
SURREAL_PORT="${SURREAL_PORT:-18000}"
SURREAL_USER="${SURREAL_USER:-root}"
SURREAL_PASS="${SURREAL_PASS:-root}"
SURREAL_NS="${SURREAL_NS:-bench}"
SURREAL_DB="${SURREAL_DB:-bench}"
SURREAL_PATH="${SURREAL_PATH:-surrealkv:$STATE_DIR/surreal-suite-surrealdb}"
SURREAL_LOG="${SURREAL_LOG:-$STATE_DIR/surreal-suite-surrealdb.log}"
SURREAL_PIDFILE="${SURREAL_PIDFILE:-$STATE_DIR/surreal-suite-surrealdb.pid}"

PG_LOCAL_SCRIPT="${PG_LOCAL_SCRIPT:-$BENCH_ROOT/pg-local.sh}"
PG_LOCAL_HOST="${PG_LOCAL_HOST:-127.0.0.1}"
PG_LOCAL_PORT="${PG_LOCAL_PORT:-55432}"
PG_LOCAL_USER="${PG_LOCAL_USER:-postgres}"
PGSTACK_DB="${PGSTACK_DB:-bench_stack}"

RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
RUN_DIR="${RUN_DIR:-$STATE_DIR/surreal-suite/$RUN_ID}"

SURREAL_SUITE_ENGINES="${SURREAL_SUITE_ENGINES:-surrealdb aiondb pgstack}"
SURREAL_SUITE_ROWS="${SURREAL_SUITE_ROWS:-2000}"
SURREAL_SUITE_WARMUP_SECONDS="${SURREAL_SUITE_WARMUP_SECONDS:-3}"
SURREAL_SUITE_ITERATIONS="${SURREAL_SUITE_ITERATIONS:-1}"
SURREAL_SUITE_DURATION_SECONDS="${SURREAL_SUITE_DURATION_SECONDS:-20}"
SURREAL_SUITE_OPERATION_TIMEOUT_SECONDS="${SURREAL_SUITE_OPERATION_TIMEOUT_SECONDS:-20}"
SURREAL_SUITE_TESTS="${SURREAL_SUITE_TESTS:-all}"
SURREAL_SUITE_UPDATE_DOCS="${SURREAL_SUITE_UPDATE_DOCS:-1}"

mkdir -p "$RUN_DIR"

surreal_storage_mode() {
    if [[ "$SURREAL_PATH" == "memory" ]]; then
        printf 'memory'
    else
        printf 'durable'
    fi
}

surreal_is_running() {
    [[ -f "$SURREAL_PIDFILE" ]] && kill -0 "$(cat "$SURREAL_PIDFILE" 2>/dev/null)" 2>/dev/null
}

surreal_wait_port() {
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if (exec 3<>/dev/tcp/"$SURREAL_HOST"/"$SURREAL_PORT") 2>/dev/null; then
            exec 3<&-; exec 3>&-
            return 0
        fi
        sleep 0.2
    done
    return 1
}

surreal_start() {
    [[ -x "$SURREAL_BIN" ]] || die "missing surreal binary; set SURREAL_BIN"
    if surreal_is_running; then
        log "surrealdb already running (pid $(cat "$SURREAL_PIDFILE"))"
        return 0
    fi
    mkdir -p "$(dirname "$SURREAL_LOG")"
    if [[ "$(surreal_storage_mode)" == "durable" ]]; then
        local surreal_fs_path="${SURREAL_PATH#*:}"
        rm -rf "$surreal_fs_path"
        mkdir -p "$(dirname "$surreal_fs_path")"
    fi
    log "starting surrealdb ws://$SURREAL_HOST:$SURREAL_PORT path=$SURREAL_PATH"
    nohup "$SURREAL_BIN" start \
        --no-banner \
        --allow-all \
        --username "$SURREAL_USER" \
        --password "$SURREAL_PASS" \
        --bind "$SURREAL_HOST:$SURREAL_PORT" \
        "$SURREAL_PATH" >"$SURREAL_LOG" 2>&1 &
    echo $! > "$SURREAL_PIDFILE"
    if ! surreal_wait_port; then
        tail -50 "$SURREAL_LOG" >&2 || true
        die "surrealdb startup timeout"
    fi
}

surreal_stop() {
    if surreal_is_running; then
        local pid; pid=$(cat "$SURREAL_PIDFILE")
        log "stopping surrealdb (pid $pid)"
        kill "$pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.2
        done
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$SURREAL_PIDFILE"
}

cleanup() {
    surreal_stop || true
    aiondb_stop || true
    "$PG_LOCAL_SCRIPT" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

prepare_pgstack() {
    "$PG_LOCAL_SCRIPT" start >&2
    PGPASSWORD="" psql -h "$PG_LOCAL_HOST" -p "$PG_LOCAL_PORT" -U "$PG_LOCAL_USER" -d postgres \
        --no-psqlrc -c "DROP DATABASE IF EXISTS \"$PGSTACK_DB\" WITH (FORCE)" >/dev/null
    PGPASSWORD="" psql -h "$PG_LOCAL_HOST" -p "$PG_LOCAL_PORT" -U "$PG_LOCAL_USER" -d postgres \
        --no-psqlrc -c "CREATE DATABASE \"$PGSTACK_DB\"" >/dev/null
}

for engine in $SURREAL_SUITE_ENGINES; do
    case "$engine" in
        surrealdb) surreal_start ;;
        aiondb) aiondb_start ;;
        pgstack) prepare_pgstack ;;
        *) die "unknown SURREAL_SUITE_ENGINES entry: $engine" ;;
    esac
done

SURREAL_PATH="$SURREAL_PATH" \
AIONDB_STORAGE="$AIONDB_STORAGE" \
python3 "$BENCH_ROOT/surreal-suite/runner.py" \
    --run-id "$RUN_ID" \
    --run-dir "$RUN_DIR" \
    --engines "$SURREAL_SUITE_ENGINES" \
    --rows "$SURREAL_SUITE_ROWS" \
    --warmup-seconds "$SURREAL_SUITE_WARMUP_SECONDS" \
    --iterations "$SURREAL_SUITE_ITERATIONS" \
    --duration-seconds "$SURREAL_SUITE_DURATION_SECONDS" \
    --operation-timeout-seconds "$SURREAL_SUITE_OPERATION_TIMEOUT_SECONDS" \
    --tests "$SURREAL_SUITE_TESTS" \
    --surreal-url "ws://$SURREAL_HOST:$SURREAL_PORT/rpc" \
    --surreal-user "$SURREAL_USER" \
    --surreal-pass "$SURREAL_PASS" \
    --surreal-ns "$SURREAL_NS" \
    --surreal-db "$SURREAL_DB" \
    --aiondb-dsn "host=$AIONDB_HOST port=$AIONDB_PORT dbname=$AIONDB_DB user=$AIONDB_USER password=$AIONDB_PASSWORD sslmode=disable" \
    --pgstack-dsn "host=$PG_LOCAL_HOST port=$PG_LOCAL_PORT dbname=$PGSTACK_DB user=$PG_LOCAL_USER sslmode=disable"

if [[ "$SURREAL_SUITE_UPDATE_DOCS" == "1" && " $SURREAL_SUITE_ENGINES " == *" aiondb "* ]]; then
    python3 "$BENCH_ROOT/surreal-suite/update_dashboard.py" "$RUN_DIR" --build-site >&2
fi

log "surreal-suite traces: $RUN_DIR"
