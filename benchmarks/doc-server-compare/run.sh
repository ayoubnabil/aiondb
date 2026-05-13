#!/usr/bin/env bash
# benchmarks/doc-server-compare/run.sh — server-mode version of the embedded docs benchmark.

set -euo pipefail

# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

SURREAL_BIN="${SURREAL_BIN:-$(command -v surreal || true)}"
if [[ -z "$SURREAL_BIN" && -x "$HOME/.surrealdb/surreal" ]]; then
    SURREAL_BIN="$HOME/.surrealdb/surreal"
fi

SURREAL_LISTEN_HOST="${SURREAL_LISTEN_HOST:-127.0.0.1}"
SURREAL_LISTEN_PORT="${SURREAL_LISTEN_PORT:-18000}"
SURREAL_USER_NAME="${SURREAL_USER_NAME:-root}"
SURREAL_USER_PASS="${SURREAL_USER_PASS:-root}"
SURREAL_NS_NAME="${SURREAL_NS_NAME:-docbench}"
SURREAL_DB_NAME="${SURREAL_DB_NAME:-docbench}"
SURREAL_STORE_PATH="${SURREAL_STORE_PATH:-memory}"
SURREAL_LOG_FILE="${SURREAL_LOG_FILE:-$STATE_DIR/doc-server-compare-surrealdb.log}"
SURREAL_PID_FILE="${SURREAL_PID_FILE:-$STATE_DIR/doc-server-compare-surrealdb.pid}"

RUN_ID="${RUN_ID:-doc-server-compare-$(date -u +%Y%m%dT%H%M%SZ)}"
RUN_DIR="${RUN_DIR:-$STATE_DIR/doc-server-compare/$RUN_ID}"

DOC_SERVER_ROWS="${DOC_SERVER_ROWS:-2000}"
DOC_SERVER_WARMUP_SECONDS="${DOC_SERVER_WARMUP_SECONDS:-1}"
DOC_SERVER_SECONDS="${DOC_SERVER_SECONDS:-2}"
DOC_SERVER_OPERATION_TIMEOUT_SECONDS="${DOC_SERVER_OPERATION_TIMEOUT_SECONDS:-60}"

mkdir -p "$RUN_DIR"

surreal_is_running() {
    [[ -f "$SURREAL_PID_FILE" ]] && kill -0 "$(cat "$SURREAL_PID_FILE" 2>/dev/null)" 2>/dev/null
}

surreal_wait_port() {
    local deadline=$((SECONDS + 30))
    while (( SECONDS < deadline )); do
        if (exec 3<>/dev/tcp/"$SURREAL_LISTEN_HOST"/"$SURREAL_LISTEN_PORT") 2>/dev/null; then
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
        log "surrealdb already running (pid $(cat "$SURREAL_PID_FILE"))"
        return 0
    fi
    mkdir -p "$(dirname "$SURREAL_LOG_FILE")"
    log "starting surrealdb ws://$SURREAL_LISTEN_HOST:$SURREAL_LISTEN_PORT path=$SURREAL_STORE_PATH"
    env -u SURREAL_PORT "$SURREAL_BIN" start \
        --no-banner \
        --allow-all \
        --username "$SURREAL_USER_NAME" \
        --password "$SURREAL_USER_PASS" \
        --bind "$SURREAL_LISTEN_HOST:$SURREAL_LISTEN_PORT" \
        "$SURREAL_STORE_PATH" >"$SURREAL_LOG_FILE" 2>&1 &
    echo $! > "$SURREAL_PID_FILE"
    if ! surreal_wait_port; then
        tail -50 "$SURREAL_LOG_FILE" >&2 || true
        die "surrealdb startup timeout"
    fi
}

surreal_stop() {
    if surreal_is_running; then
        local pid; pid=$(cat "$SURREAL_PID_FILE")
        log "stopping surrealdb (pid $pid)"
        kill "$pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.2
        done
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$SURREAL_PID_FILE"
}

cleanup() {
    surreal_stop || true
    aiondb_stop || true
}
trap cleanup EXIT INT TERM

surreal_start
aiondb_start

python3 "$BENCH_ROOT/doc-server-compare/runner.py" \
    --run-id "$RUN_ID" \
    --run-dir "$RUN_DIR" \
    --rows "$DOC_SERVER_ROWS" \
    --warmup-seconds "$DOC_SERVER_WARMUP_SECONDS" \
    --measure-seconds "$DOC_SERVER_SECONDS" \
    --operation-timeout-seconds "$DOC_SERVER_OPERATION_TIMEOUT_SECONDS" \
    --surreal-url "ws://$SURREAL_LISTEN_HOST:$SURREAL_LISTEN_PORT/rpc" \
    --surreal-user "$SURREAL_USER_NAME" \
    --surreal-pass "$SURREAL_USER_PASS" \
    --surreal-ns "$SURREAL_NS_NAME" \
    --surreal-db "$SURREAL_DB_NAME" \
    --aiondb-dsn "host=$AIONDB_HOST port=$AIONDB_PORT dbname=$AIONDB_DB user=$AIONDB_USER password=$AIONDB_PASSWORD sslmode=disable"

log "doc-server-compare traces: $RUN_DIR"
