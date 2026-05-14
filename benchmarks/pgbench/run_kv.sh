#!/usr/bin/env bash
# benchmarks/pgbench/run_kv.sh — minimal point-lookup KV micro-bench.
#
# Repro of the user-reported regression: a tiny PK-indexed table where
# SELECT/UPDATE per-key latency exposes per-commit overhead in the storage
# engine. Two scenarios:
#   1) kv-select  — SELECT v FROM bench_kv WHERE k = :k
#   2) kv-update  — UPDATE bench_kv SET v = v + 1 WHERE k = :k
#
# Environment (in addition to benchmarks/common.sh):
#   PGBENCH_KV_SCALE=1     row count = scale * 100
#   PGBENCH_CLIENTS=1      pgbench -c
#   PGBENCH_DURATION=5     pgbench -T (seconds)
#   PGBENCH_KV_SCENARIOS="kv-select kv-update"

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd pgbench
require_cmd psql

PGBENCH_KV_SCALE="${PGBENCH_KV_SCALE:-1}"
PGBENCH_CLIENTS="${PGBENCH_CLIENTS:-1}"
PGBENCH_DURATION="${PGBENCH_DURATION:-5}"
PGBENCH_KV_SCENARIOS="${PGBENCH_KV_SCENARIOS:-kv-select kv-update}"

REPORT="$STATE_DIR/pgbench-kv-report.tsv"
printf 'bench\tscenario\tengine\tstatus\trows\ttps\tlatency_ms\n' > "$REPORT"

install_aiondb_exit_trap

scenario_file() {
    case "$1" in
        kv-select) echo "$(dirname "$0")/kv_select.sql" ;;
        kv-update) echo "$(dirname "$0")/kv_update.sql" ;;
        *) die "unknown kv scenario: $1" ;;
    esac
}

init_kv_table() {
    local host="$1" port="$2" user="$3" db="$4" password="$5" rows="$6"
    PGPASSWORD="$password" psql -v ON_ERROR_STOP=1 -h "$host" -p "$port" -U "$user" "$db" >/dev/null <<SQL
DROP TABLE IF EXISTS bench_kv;
CREATE TABLE bench_kv (k INT PRIMARY KEY, v BIGINT);
INSERT INTO bench_kv SELECT i, i FROM generate_series(1, $rows) AS s(i);
SQL
}

run_on_engine() {
    local engine="$1" host port user db password
    case "$engine" in
        aiondb)
            aiondb_start
            host="$AIONDB_HOST"; port="$AIONDB_PORT"
            user="$AIONDB_USER"; db="$AIONDB_DB"
            password="$AIONDB_PASSWORD"
            ;;
        pg)
            pg_ensure_db
            pg_reset_public_schema
            host="$PG_HOST"; port="$PG_PORT"
            user="$PG_USER"; db="$PG_DB"
            password="$PG_PASSWORD"
            ;;
        *) die "unknown engine $engine" ;;
    esac

    local rows=$((PGBENCH_KV_SCALE * 100))
    log "=== kv init ($engine, rows=$rows) ==="
    if ! init_kv_table "$host" "$port" "$user" "$db" "$password" "$rows"; then
        warn "kv init FAILED on $engine"
        for scenario in $PGBENCH_KV_SCENARIOS; do
            printf 'pgbench-kv\t%s\t%s\tFAIL\t%s\t-\t-\n' "$scenario" "$engine" "$rows" >> "$REPORT"
        done
        [[ "$engine" == "aiondb" ]] && aiondb_stop
        return 1
    fi

    for scenario in $PGBENCH_KV_SCENARIOS; do
        local file run_log tps latency status
        file=$(scenario_file "$scenario")
        run_log="$STATE_DIR/pgbench-kv-run-$engine-$scenario.log"
        log "--- kv run ($engine / $scenario, ${PGBENCH_DURATION}s, c=$PGBENCH_CLIENTS) ---"
        if PGPASSWORD="$password" pgbench -n -h "$host" -p "$port" -U "$user" "$db" \
            -s "$PGBENCH_KV_SCALE" \
            -c "$PGBENCH_CLIENTS" -T "$PGBENCH_DURATION" -f "$file" > "$run_log" 2>&1; then
            status=OK
            tps=$(grep -E '^tps = ' "$run_log" | head -1 | awk '{print $3}')
            latency=$(grep -E '^latency average = ' "$run_log" | head -1 | awk '{print $4}')
        else
            status=FAIL
            tps="-"; latency="-"
            tail -5 "$run_log" >&2 || true
        fi
        printf 'pgbench-kv\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$scenario" "$engine" "$status" "$rows" "${tps:--}" "${latency:--}" >> "$REPORT"
    done

    [[ "$engine" == "aiondb" ]] && aiondb_stop
    return 0
}

for engine in $BENCH_ENGINES; do
    if ! run_on_engine "$engine"; then
        warn "engine $engine skipped remaining scenarios"
    fi
done

log ""
log "===== pgbench-kv report ====="
column -t -s $'\t' "$REPORT" >&2 || cat "$REPORT" >&2
