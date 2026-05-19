#!/usr/bin/env bash
# benchmarks/crud-bench-official/run.sh — run SurrealDB's official crud-bench
# workload against AionDB pgwire, SurrealDB WS, and PostgreSQL.

set -euo pipefail

# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

CRUD_BENCH_REPO_URL="${CRUD_BENCH_REPO_URL:-https://github.com/surrealdb/crud-bench.git}"
CRUD_BENCH_DIR="${CRUD_BENCH_DIR:-/tmp/surreal-crud-bench}"
CRUD_BENCH_BIN="$CRUD_BENCH_DIR/target/release/crud-bench"
CRUD_BENCH_SAMPLES="${CRUD_BENCH_SAMPLES:-2000}"
CRUD_BENCH_CLIENTS="${CRUD_BENCH_CLIENTS:-1}"
CRUD_BENCH_THREADS="${CRUD_BENCH_THREADS:-1}"
CRUD_BENCH_OPERATION_TIMEOUT="${CRUD_BENCH_OPERATION_TIMEOUT:-1800}"
CRUD_BENCH_ENGINES="${CRUD_BENCH_ENGINES:-aiondb surrealdb pgstack}"
CRUD_BENCH_UPDATE_DOCS="${CRUD_BENCH_UPDATE_DOCS:-1}"

SURREAL_BIN="${SURREAL_BIN:-$(command -v surreal || true)}"
if [[ -z "$SURREAL_BIN" && -x "$HOME/.surrealdb/surreal" ]]; then
    SURREAL_BIN="$HOME/.surrealdb/surreal"
fi
SURREAL_HOST="${SURREAL_HOST:-127.0.0.1}"
SURREAL_PORT="${SURREAL_PORT:-18000}"
SURREAL_USER="${SURREAL_USER:-root}"
SURREAL_PASS="${SURREAL_PASS:-root}"
SURREAL_PATH="${SURREAL_PATH:-surrealkv:$STATE_DIR/crud-bench-official-surrealdb}"
SURREAL_LOG="${SURREAL_LOG:-$STATE_DIR/crud-bench-official-surrealdb.log}"
SURREAL_PIDFILE="${SURREAL_PIDFILE:-$STATE_DIR/crud-bench-official-surrealdb.pid}"

PG_LOCAL_SCRIPT="${PG_LOCAL_SCRIPT:-$BENCH_ROOT/pg-local.sh}"
PG_LOCAL_HOST="${PG_LOCAL_HOST:-127.0.0.1}"
PG_LOCAL_PORT="${PG_LOCAL_PORT:-55432}"
PG_LOCAL_USER="${PG_LOCAL_USER:-postgres}"

RUN_ID="${RUN_ID:-crudbench-official-$(date -u +%Y%m%dT%H%M%SZ)}"
RUN_DIR="${RUN_DIR:-$STATE_DIR/crud-bench-official/$RUN_ID}"

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
            exec 3<&-
            exec 3>&-
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
        local pid
        pid=$(cat "$SURREAL_PIDFILE")
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

ensure_crud_bench() {
    if [[ ! -d "$CRUD_BENCH_DIR/.git" ]]; then
        log "cloning official crud-bench into $CRUD_BENCH_DIR"
        git clone --depth 1 "$CRUD_BENCH_REPO_URL" "$CRUD_BENCH_DIR" >&2
    fi
    # The comparison uses the official crud-bench workload and adapters. This
    # local build patch only disables embedded SurrealDB KV backends so the WS
    # client can compile on hosts without the bundled RocksDB C toolchain.
    if grep -q '"surrealdb/kv-rocksdb"' "$CRUD_BENCH_DIR/Cargo.toml"; then
        perl -0pi -e 's/surrealdb = \\[.*?\\]/surrealdb = [\n    "dep:surrealdb",\n    "surrealdb\\/protocol-http",\n    "surrealdb\\/protocol-ws",\n    "surrealdb\\/rustls",\n]/s' "$CRUD_BENCH_DIR/Cargo.toml"
    fi
    if [[ ! -x "$CRUD_BENCH_BIN" ]]; then
        log "building official crud-bench (postgres,surrealdb)"
        (
            cd "$CRUD_BENCH_DIR"
            cargo build --release --no-default-features --features postgres,surrealdb
        ) >&2
    fi
}

record_metadata() {
    {
        printf 'run_id=%s\n' "$RUN_ID"
        printf 'crud_bench_dir=%s\n' "$CRUD_BENCH_DIR"
        printf 'crud_bench_head=%s\n' "$(git -C "$CRUD_BENCH_DIR" rev-parse HEAD 2>/dev/null || true)"
        printf 'samples=%s\n' "$CRUD_BENCH_SAMPLES"
        printf 'clients=%s\n' "$CRUD_BENCH_CLIENTS"
        printf 'threads=%s\n' "$CRUD_BENCH_THREADS"
        printf 'operation_timeout=%s\n' "$CRUD_BENCH_OPERATION_TIMEOUT"
        printf 'engines=%s\n' "$CRUD_BENCH_ENGINES"
        printf 'aiondb_storage=%s\n' "$AIONDB_STORAGE"
        printf 'surreal_storage=%s\n' "$(surreal_storage_mode)"
        printf 'surreal_path=%s\n' "$SURREAL_PATH"
        printf 'pgstack_storage=durable\n'
        printf 'surreal_endpoint=ws://%s:%s\n' "$SURREAL_HOST" "$SURREAL_PORT"
        printf 'postgres_endpoint=host=%s port=%s dbname=postgres user=%s sslmode=disable\n' "$PG_LOCAL_HOST" "$PG_LOCAL_PORT" "$PG_LOCAL_USER"
    } > "$RUN_DIR/metadata.env"
}

run_one() {
    local engine="$1"
    local database endpoint name log_file
    case "$engine" in
        aiondb)
            aiondb_start
            database=postgres
            endpoint="host=$AIONDB_HOST port=$AIONDB_PORT dbname=$AIONDB_DB user=$AIONDB_USER password=$AIONDB_PASSWORD sslmode=disable"
            name=crudbench-aiondb
            ;;
        surrealdb)
            surreal_start
            database=surrealdb
            endpoint="ws://$SURREAL_HOST:$SURREAL_PORT"
            name=crudbench-surrealdb
            ;;
        pgstack)
            "$PG_LOCAL_SCRIPT" restart >&2
            database=postgres
            endpoint="host=$PG_LOCAL_HOST port=$PG_LOCAL_PORT dbname=postgres user=$PG_LOCAL_USER sslmode=disable"
            name=crudbench-pgstack
            ;;
        *)
            die "unknown CRUD_BENCH_ENGINES item: $engine"
            ;;
    esac

    log_file="$RUN_DIR/$name.log"
    log "running official crud-bench for $engine"
    (
        cd "$CRUD_BENCH_DIR"
        rm -f "result-$name.csv" "result-$name.json" "result-$name.html"
        SURREALDB_USERNAME="$SURREAL_USER" \
        SURREALDB_PASSWORD="$SURREAL_PASS" \
        CRUD_BENCH_OPERATION_TIMEOUT="$CRUD_BENCH_OPERATION_TIMEOUT" \
            "$CRUD_BENCH_BIN" \
                -d "$database" \
                -e "$endpoint" \
                -s "$CRUD_BENCH_SAMPLES" \
                -c "$CRUD_BENCH_CLIENTS" \
                -t "$CRUD_BENCH_THREADS" \
                -n "$name"
        cp "result-$name.csv" "$RUN_DIR/$name.csv"
        cp "result-$name.json" "$RUN_DIR/$name.json"
        cp "result-$name.html" "$RUN_DIR/$name.html"
    ) >"$log_file" 2>&1

    log "$engine results: $RUN_DIR/$name.csv"
}

ensure_crud_bench
record_metadata

for engine in $CRUD_BENCH_ENGINES; do
    run_one "$engine"
done

if [[ "$CRUD_BENCH_UPDATE_DOCS" == "1" || "$CRUD_BENCH_UPDATE_DOCS" == "true" ]]; then
    python3 "$BENCH_ROOT/crud-bench-official/import_results.py" "$RUN_DIR" \
        --run-id "$RUN_ID" \
        --build-site >&2
fi

log "official crud-bench traces: $RUN_DIR"
