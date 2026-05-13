#!/usr/bin/env bash
# benchmarks/pgbench/run.sh — OLTP micro-benchmark using the native pgbench CLI.
#
# Runs the init phase (-I dtg: drop, tables, client-side generate) and then
# three standard scenarios against each engine:
#   1) SELECT-only        (-S)      — read-only, pure index point lookup.
#   2) Simple update      (-N)      — UPDATE accounts + INSERT history.
#   3) Full TPC-B-like    (default) — UPDATE accounts/tellers/branches + history.
#
# Default init keeps the primary key step ('p') enabled so the built-in
# select-only scenario uses indexed point lookups (standard pgbench behavior).
# We still skip vacuum ('v') by default to keep startup time low.
# If you want to include vacuum as well, set PGBENCH_INIT_STEPS=dtgvp.
#
# Environment (in addition to benchmarks/common.sh):
#   PGBENCH_SCALE=1          pgbench -s
#   PGBENCH_CLIENTS=1        pgbench -c
#   PGBENCH_DURATION=10      pgbench -T (seconds)
#   PGBENCH_INIT_STEPS=dtg   pgbench -I
#   PGBENCH_SCENARIOS="select-only simple-update tpcb"
#   PGBENCH_PROTOCOL=simple  pgbench -M (simple | extended | prepared)

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd pgbench
require_cmd psql

PGBENCH_SCALE="${PGBENCH_SCALE:-1}"
PGBENCH_CLIENTS="${PGBENCH_CLIENTS:-1}"
PGBENCH_DURATION="${PGBENCH_DURATION:-10}"
PGBENCH_INIT_STEPS="${PGBENCH_INIT_STEPS:-dtgp}"
PGBENCH_SCENARIOS="${PGBENCH_SCENARIOS:-select-only simple-update tpcb}"
PGBENCH_PROTOCOL="${PGBENCH_PROTOCOL:-simple}"

case "$PGBENCH_PROTOCOL" in
    simple|extended|prepared) ;;
    *) die "unknown PGBENCH_PROTOCOL=$PGBENCH_PROTOCOL (want: simple | extended | prepared)" ;;
esac

REPORT="$STATE_DIR/pgbench-report.tsv"
printf 'bench\tprotocol\tscenario\tengine\tstatus\tinit_s\ttps\tlatency_ms\n' > "$REPORT"

install_aiondb_exit_trap

scenario_flag() {
    case "$1" in
        select-only)    echo "-S" ;;
        simple-update)  echo "-N" ;;
        tpcb)           echo ""   ;;  # default TPC-B-like mix
        *) die "unknown pgbench scenario: $1" ;;
    esac
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

    local init_log="$STATE_DIR/pgbench-init-$engine.log"
    log "=== pgbench init ($engine, scale=$PGBENCH_SCALE, steps=$PGBENCH_INIT_STEPS) ==="
    local init_start init_end init_s
    init_start=$(date +%s.%N)
    if PGPASSWORD="$password" pgbench -h "$host" -p "$port" -U "$user" "$db" \
        -i -s "$PGBENCH_SCALE" -I "$PGBENCH_INIT_STEPS" > "$init_log" 2>&1; then
        init_end=$(date +%s.%N)
        init_s=$(awk -v s="$init_start" -v e="$init_end" 'BEGIN{printf "%.2f", (e-s)}')
        log "  init OK in ${init_s}s"
    else
        tail -20 "$init_log" >&2 || true
        warn "pgbench init FAILED on $engine — see $init_log"
        printf 'pgbench\t%s\tinit\t%s\tFAIL\t-\t-\t-\n' "$PGBENCH_PROTOCOL" "$engine" >> "$REPORT"
        [[ "$engine" == "aiondb" ]] && aiondb_stop
        return 1
    fi

    for scenario in $PGBENCH_SCENARIOS; do
        local flag run_log tps latency status
        flag=$(scenario_flag "$scenario")
        run_log="$STATE_DIR/pgbench-run-$engine-$scenario.log"
        log "--- pgbench run ($engine / $scenario, protocol=$PGBENCH_PROTOCOL, ${PGBENCH_DURATION}s, c=$PGBENCH_CLIENTS) ---"
        if PGPASSWORD="$password" pgbench -h "$host" -p "$port" -U "$user" "$db" \
            -s "$PGBENCH_SCALE" \
            -c "$PGBENCH_CLIENTS" -T "$PGBENCH_DURATION" -M "$PGBENCH_PROTOCOL" \
            $flag > "$run_log" 2>&1; then
            status=OK
            tps=$(grep -E '^tps = ' "$run_log" | head -1 | awk '{print $3}')
            latency=$(grep -E '^latency average = ' "$run_log" | head -1 | awk '{print $4}')
        else
            status=FAIL
            tps="-"; latency="-"
            tail -5 "$run_log" >&2 || true
        fi
        printf 'pgbench\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$PGBENCH_PROTOCOL" "$scenario" "$engine" "$status" "$init_s" "${tps:--}" "${latency:--}" >> "$REPORT"
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
log "===== pgbench report ====="
column -t -s $'\t' "$REPORT" >&2 || cat "$REPORT" >&2

awk -F'\t' '
    NR==1 {next}
    {
        protocol=$2; sc=$3; eng=$4
        key=protocol "|" sc
        status[key,eng]=$5
        tps[key,eng]=$7+0.0
        lat[key,eng]=$8+0.0
        scenario[key]=sc
        proto[key]=protocol
        seen[key]=1
    }
    END {
        printf "\n"
        for (key in seen) {
            if ((key, "aiondb") in status && (key, "pg") in status \
                && status[key, "aiondb"]=="OK" && status[key, "pg"]=="OK" \
                && tps[key, "pg"] > 0.0 && lat[key, "pg"] > 0.0) {
                tps_ratio = tps[key, "aiondb"] / tps[key, "pg"]
                lat_ratio = lat[key, "aiondb"] / lat[key, "pg"]
                printf "compare  protocol=%s  scenario=%s  tps_ratio_aiondb_over_pg=%.3f  latency_ratio_aiondb_over_pg=%.3f\n", \
                    proto[key], scenario[key], tps_ratio, lat_ratio
            }
        }
    }
' "$REPORT" >&2
