#!/usr/bin/env bash
# benchmarks/common.sh — shared helpers for the AionDB vs PostgreSQL bench harness.
#
# Sourced by benchmarks/<name>/run.sh scripts. Provides:
#   - AionDB server lifecycle (build, start, stop, psql wrapper)
#   - PostgreSQL wrapper (uses the existing local cluster on $PG_PORT)
#   - per-query timed runner with status (OK / FAIL / TIMEOUT)
#   - side-by-side reporter
#
# All defaults are overridable via environment variables.

set -euo pipefail

BENCH_ROOT="${BENCH_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
REPO_ROOT="${REPO_ROOT:-$(cd "$BENCH_ROOT/.." && pwd)}"
STATE_DIR="${STATE_DIR:-$BENCH_ROOT/.state}"
DATA_DIR_BASE="${DATA_DIR_BASE:-$BENCH_ROOT/.data}"
TOOLS_DIR_BASE="${TOOLS_DIR_BASE:-$BENCH_ROOT/.tools}"

# ---- AionDB connection / process ---------------------------------------------
AIONDB_HOST="${AIONDB_HOST:-127.0.0.1}"
AIONDB_PORT="${AIONDB_PORT:-15432}"
AIONDB_USER="${AIONDB_USER:-bench}"
# Password must satisfy the server security baseline:
# ≥12 chars, lower, upper, digit, symbol, not equal to the role name.
AIONDB_PASSWORD="${AIONDB_PASSWORD:-BenchAion42!}"
AIONDB_DB="${AIONDB_DB:-default}"
AIONDB_BIN="${AIONDB_BIN:-$REPO_ROOT/target/release/aiondb}"
AIONDB_DATA_DIR="${AIONDB_DATA_DIR:-$STATE_DIR/aiondb-data}"
AIONDB_LOG="${AIONDB_LOG:-$STATE_DIR/aiondb.log}"
AIONDB_PIDFILE="${AIONDB_PIDFILE:-$STATE_DIR/aiondb.pid}"
# Default to durable so benchmark behavior stays close to PostgreSQL.
AIONDB_STORAGE="${AIONDB_STORAGE:-durable}"  # ephemeral | durable
AIONDB_START_TIMEOUT_S="${AIONDB_START_TIMEOUT_S:-30}"
AIONDB_COPY_IN_MAX_BUFFER="${AIONDB_COPY_IN_MAX_BUFFER:-67108864}" # 64 MiB hard max in server
AIONDB_COPY_IN_TOTAL_TIMEOUT_MS="${AIONDB_COPY_IN_TOTAL_TIMEOUT_MS:-900000}"
# Benchmark-focused execution limits (PG-like defaults: no statement timeout).
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS="${AIONDB_LIMITS_STATEMENT_TIMEOUT_MS:-0}"
AIONDB_LIMITS_MAX_RESULT_ROWS="${AIONDB_LIMITS_MAX_RESULT_ROWS:-2000000}"
AIONDB_LIMITS_MAX_RESULT_BYTES="${AIONDB_LIMITS_MAX_RESULT_BYTES:-67108864}"  # 64 MiB
AIONDB_LIMITS_MAX_MEMORY_BYTES="${AIONDB_LIMITS_MAX_MEMORY_BYTES:-536870912}" # 512 MiB
AIONDB_LIMITS_MAX_TEMP_BYTES="${AIONDB_LIMITS_MAX_TEMP_BYTES:-1073741824}"    # 1 GiB
AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY="${AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY:-4}"
AIONDB_ENGINE_POOL_WORKER_THREADS="${AIONDB_ENGINE_POOL_WORKER_THREADS:-8}"
# COPY ingest batching (executor COPY -> storage insert_batch).
AIONDB_COPY_INSERT_BATCH_ROWS="${AIONDB_COPY_INSERT_BATCH_ROWS:-32768}"
# Keep WAL durable but avoid full paged-state rematerialization per commit,
# closer to PostgreSQL checkpoint/writeback behavior.
AIONDB_PERSIST_PAGED_STATE_ON_COMMIT="${AIONDB_PERSIST_PAGED_STATE_ON_COMMIT:-0}"

# ---- PostgreSQL connection ---------------------------------------------------
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-$USER}"
PG_DB="${PG_DB:-bench_ref}"
PG_PASSWORD="${PG_PASSWORD:-}"

# ---- Timeouts ----------------------------------------------------------------
AIONDB_QUERY_TIMEOUT_S="${AIONDB_QUERY_TIMEOUT_S:-300}"
PG_QUERY_TIMEOUT_S="${PG_QUERY_TIMEOUT_S:-300}"
BENCH_CAPTURE_QUERY_OUTPUT="${BENCH_CAPTURE_QUERY_OUTPUT:-0}"  # 0 => stdout discarded

# ---- Engine selection --------------------------------------------------------
# Space-separated subset of "aiondb pg".
BENCH_ENGINES="${BENCH_ENGINES:-aiondb pg}"

mkdir -p "$STATE_DIR" "$DATA_DIR_BASE" "$TOOLS_DIR_BASE"

log()  { printf '[bench] %s\n' "$*" >&2; }
warn() { printf '[bench][warn] %s\n' "$*" >&2; }
die()  { printf '[bench][FATAL] %s\n' "$*" >&2; exit 1; }

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

# ---- AionDB lifecycle --------------------------------------------------------

aiondb_needs_rebuild() {
    if [[ ! -x "$AIONDB_BIN" || "${AIONDB_FORCE_REBUILD:-0}" == "1" ]]; then
        return 0
    fi
    local newer_source
    newer_source=$(
        find "$REPO_ROOT/Cargo.toml" "$REPO_ROOT/Cargo.lock" "$REPO_ROOT/crates" \
            -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
            -newer "$AIONDB_BIN" -print -quit 2>/dev/null || true
    )
    [[ -n "$newer_source" ]]
}

aiondb_build() {
    if ! aiondb_needs_rebuild; then
        return 0
    fi
    log "building aiondb (release)"
    (cd "$REPO_ROOT" && cargo build --release -p aiondb-server --bin aiondb) >&2
    [[ -x "$AIONDB_BIN" ]] || die "build produced no binary at $AIONDB_BIN"
}

aiondb_is_running() {
    [[ -f "$AIONDB_PIDFILE" ]] && kill -0 "$(cat "$AIONDB_PIDFILE" 2>/dev/null)" 2>/dev/null
}

aiondb_wait_port() {
    local deadline=$((SECONDS + AIONDB_START_TIMEOUT_S))
    while (( SECONDS < deadline )); do
        if (exec 3<>/dev/tcp/"$AIONDB_HOST"/"$AIONDB_PORT") 2>/dev/null; then
            exec 3<&-; exec 3>&-
            return 0
        fi
        sleep 0.2
    done
    return 1
}

aiondb_start() {
    aiondb_build
    if aiondb_is_running; then
        log "aiondb already running (pid $(cat "$AIONDB_PIDFILE"))"
        return 0
    fi
    local args=()
    case "$AIONDB_STORAGE" in
        ephemeral) args+=(--ephemeral) ;;
        durable)   args+=(--data-dir "$AIONDB_DATA_DIR") ;;
        *) die "unknown AIONDB_STORAGE=$AIONDB_STORAGE (want: ephemeral | durable)" ;;
    esac
    rm -rf "$AIONDB_DATA_DIR"
    mkdir -p "$AIONDB_DATA_DIR" "$(dirname "$AIONDB_LOG")"
    log "starting aiondb on $AIONDB_HOST:$AIONDB_PORT storage=$AIONDB_STORAGE"
    AIONDB_PGWIRE_LISTEN_ADDR="$AIONDB_HOST:$AIONDB_PORT" \
    AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
    AIONDB_BOOTSTRAP_USER="$AIONDB_USER" \
    AIONDB_BOOTSTRAP_PASSWORD="$AIONDB_PASSWORD" \
    AIONDB_PGWIRE_COPY_IN_MAX_BUFFER="$AIONDB_COPY_IN_MAX_BUFFER" \
    AIONDB_PGWIRE_COPY_IN_TOTAL_TIMEOUT_MS="$AIONDB_COPY_IN_TOTAL_TIMEOUT_MS" \
    AIONDB_PGWIRE_IDLE_TIMEOUT_MS="${AIONDB_PGWIRE_IDLE_TIMEOUT_MS:-0}" \
    AIONDB_LIMITS_STATEMENT_TIMEOUT_MS="$AIONDB_LIMITS_STATEMENT_TIMEOUT_MS" \
    AIONDB_LIMITS_MAX_RESULT_ROWS="$AIONDB_LIMITS_MAX_RESULT_ROWS" \
    AIONDB_LIMITS_MAX_RESULT_BYTES="$AIONDB_LIMITS_MAX_RESULT_BYTES" \
    AIONDB_LIMITS_MAX_MEMORY_BYTES="$AIONDB_LIMITS_MAX_MEMORY_BYTES" \
    AIONDB_LIMITS_MAX_TEMP_BYTES="$AIONDB_LIMITS_MAX_TEMP_BYTES" \
    AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY="$AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY" \
    AIONDB_ENGINE_POOL_WORKER_THREADS="$AIONDB_ENGINE_POOL_WORKER_THREADS" \
    AIONDB_COPY_INSERT_BATCH_ROWS="$AIONDB_COPY_INSERT_BATCH_ROWS" \
    AIONDB_PERSIST_PAGED_STATE_ON_COMMIT="$AIONDB_PERSIST_PAGED_STATE_ON_COMMIT" \
        nohup "$AIONDB_BIN" "${args[@]}" > "$AIONDB_LOG" 2>&1 &
    echo $! > "$AIONDB_PIDFILE"
    if aiondb_wait_port; then
        log "aiondb up (pid $(cat "$AIONDB_PIDFILE"))"
    else
        warn "aiondb failed to open port — last 30 log lines:"
        tail -n 30 "$AIONDB_LOG" >&2 || true
        aiondb_stop
        die "aiondb startup timeout after ${AIONDB_START_TIMEOUT_S}s"
    fi
}

aiondb_stop() {
    if aiondb_is_running; then
        local pid; pid=$(cat "$AIONDB_PIDFILE")
        log "stopping aiondb (pid $pid)"
        kill "$pid" 2>/dev/null || true
        # Wait up to 10s for graceful shutdown
        for _ in $(seq 1 50); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.2
        done
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$AIONDB_PIDFILE"
}

# Install an exit trap that stops aiondb on script exit.
# Call once from each run.sh after sourcing common.sh.
install_aiondb_exit_trap() {
    trap 'aiondb_stop' EXIT INT TERM
}

# ---- psql wrappers -----------------------------------------------------------

aiondb_psql() {
    PGPASSWORD="$AIONDB_PASSWORD" psql \
        -h "$AIONDB_HOST" -p "$AIONDB_PORT" \
        -U "$AIONDB_USER" -d "$AIONDB_DB" \
        --no-psqlrc -v ON_ERROR_STOP=1 "$@"
}

pg_psql() {
    PGPASSWORD="$PG_PASSWORD" psql \
        -h "$PG_HOST" -p "$PG_PORT" \
        -U "$PG_USER" -d "$PG_DB" \
        --no-psqlrc -v ON_ERROR_STOP=1 "$@"
}

# Ensure the PG reference database exists and is empty. Safe to call repeatedly.
pg_ensure_db() {
    require_cmd psql
    PGPASSWORD="$PG_PASSWORD" psql \
        -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
        --no-psqlrc -tAc "SELECT 1 FROM pg_database WHERE datname='$PG_DB'" \
        | grep -q 1 \
    || PGPASSWORD="$PG_PASSWORD" psql \
        -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
        --no-psqlrc -c "CREATE DATABASE \"$PG_DB\""
}

pg_terminate_db_backends() {
    PGPASSWORD="$PG_PASSWORD" psql \
        -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
        --no-psqlrc -qAt \
        -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='$PG_DB' AND pid <> pg_backend_pid();" \
        >/dev/null || true
}

pg_reset_public_schema() {
    pg_terminate_db_backends
    pg_psql -c "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;" >/dev/null
}

# ---- timed query runner ------------------------------------------------------

# run_query_timed <label> <engine> <sql_file> <out_file> <timeout_s>
# Emits one tab-separated line: "<label>\t<engine>\t<status>\t<ms>"
run_query_timed() {
    local label="$1" engine="$2" sqlfile="$3" outfile="$4" tmo="$5"
    local start end ms status rc
    local -a cmd
    case "$engine" in
        aiondb)
            cmd=(env "PGPASSWORD=$AIONDB_PASSWORD" psql
                -h "$AIONDB_HOST" -p "$AIONDB_PORT"
                -U "$AIONDB_USER" -d "$AIONDB_DB"
                --no-psqlrc -v ON_ERROR_STOP=1 -qAtf "$sqlfile")
            ;;
        pg)
            local pg_stmt_timeout_ms
            pg_stmt_timeout_ms=$(( tmo * 1000 ))
            cmd=(env "PGPASSWORD=$PG_PASSWORD" psql
                -h "$PG_HOST" -p "$PG_PORT"
                -U "$PG_USER" -d "$PG_DB"
                -c "SET statement_timeout = ${pg_stmt_timeout_ms};"
                --no-psqlrc -v ON_ERROR_STOP=1 -qAtf "$sqlfile")
            ;;
        *) die "unknown engine '$engine' (want: aiondb | pg)" ;;
    esac
    start=$(date +%s.%N)
    if [[ "$BENCH_CAPTURE_QUERY_OUTPUT" == "1" || "$BENCH_CAPTURE_QUERY_OUTPUT" == "true" ]]; then
        if timeout --preserve-status "$tmo" "${cmd[@]}" > "$outfile" 2>&1; then
            status=OK
        else
            rc=$?
            if (( rc == 124 )); then
                status=TIMEOUT
            else
                status=FAIL
            fi
        fi
    elif timeout --preserve-status "$tmo" "${cmd[@]}" > /dev/null 2> "$outfile"; then
        status=OK
    else
        rc=$?
        if (( rc == 124 )); then
            status=TIMEOUT
        else
            status=FAIL
        fi
    fi
    end=$(date +%s.%N)
    ms=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.1f", (e-s)*1000}')
    printf '%s\t%s\t%s\t%s\n' "$label" "$engine" "$status" "$ms"
}

# ---- report helpers ----------------------------------------------------------

report_header() {
    local file="$1"
    printf 'bench\tquery\tengine\tstatus\tms\n' > "$file"
}

# summarize_report <file>  — pass/fail counts and mean OK time, per engine.
summarize_report() {
    local file="$1"
    [[ -f "$file" ]] || { warn "report not found: $file"; return; }
    log "report: $file"
    column -t -s $'\t' "$file" >&2 || cat "$file" >&2
    awk -F'\t' '
        NR==1 {next}
        {
            tot[$3]++
            if ($4=="OK") {
                ok[$3]++
                okms[$3] += $5
            } else if ($4=="TIMEOUT") {
                tmo[$3]++
            } else {
                fail[$3]++
            }
        }
        END {
            printf "\n"
            for (e in tot) {
                mean = (ok[e]>0) ? okms[e]/ok[e] : 0
                printf "summary  %s  OK=%d  FAIL=%d  TIMEOUT=%d  total=%d  mean_ok_ms=%.1f\n", \
                    e, ok[e]+0, fail[e]+0, tmo[e]+0, tot[e]+0, mean
            }
        }
    ' "$file" >&2
}

# compare_report_engines <file>
# For reports with columns:
#   bench \t query \t engine \t status \t ms
# prints a concise AionDB vs PostgreSQL comparison over query keys present on both.
compare_report_engines() {
    local file="$1"
    [[ -f "$file" ]] || { warn "report not found: $file"; return; }
    awk -F'\t' '
        NR==1 {next}
        {
            key = $2
            eng = $3
            status[key, eng] = $4
            ms[key, eng] = $5 + 0.0
            seen[key] = 1
        }
        END {
            both = 0
            both_ok = 0
            aion_fast = 0
            pg_fast = 0
            tie = 0
            sum_speedup = 0.0   # pg_ms / aion_ms for both OK
            for (k in seen) {
                if ((k, "aiondb") in status && (k, "pg") in status) {
                    both++
                    if (status[k, "aiondb"] == "OK" && status[k, "pg"] == "OK" \
                        && ms[k, "aiondb"] > 0.0 && ms[k, "pg"] > 0.0) {
                        both_ok++
                        ratio = ms[k, "pg"] / ms[k, "aiondb"]
                        sum_speedup += ratio
                        if (ms[k, "aiondb"] < ms[k, "pg"]) {
                            aion_fast++
                        } else if (ms[k, "aiondb"] > ms[k, "pg"]) {
                            pg_fast++
                        } else {
                            tie++
                        }
                    }
                }
            }
            if (both == 0) {
                printf "compare  no-overlap-between-engines\n"
                exit 0
            }
            mean_speedup = (both_ok > 0) ? (sum_speedup / both_ok) : 0.0
            printf "compare  overlapping_queries=%d  both_ok=%d  aiondb_faster=%d  pg_faster=%d  ties=%d  mean_speedup_pg_over_aiondb=%.3f\n", \
                both, both_ok, aion_fast, pg_fast, tie, mean_speedup
        }
    ' "$file" >&2
}
