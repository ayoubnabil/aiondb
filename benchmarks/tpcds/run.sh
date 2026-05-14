#!/usr/bin/env bash
# benchmarks/tpcds/run.sh — load TPC-DS SF=$TPCDS_SCALE and run the 99 standard
# queries against each engine in $BENCH_ENGINES.
#
# dsdgen outputs one `.dat` file per table, pipe-delimited with a trailing
# pipe (same convention as TPC-H dbgen). We convert on the fly with sed+tr.

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd psql

TPCDS_SCALE="${TPCDS_SCALE:-1}"
TPCDS_TOOLS_DIR="$TOOLS_DIR_BASE/tpcds-kit"
TPCDS_DATA_DIR="$DATA_DIR_BASE/tpcds-sf${TPCDS_SCALE}"
TPCDS_SCHEMA_FILE="$TPCDS_TOOLS_DIR/tools/tpcds.sql"
TPCDS_QUERIES_DIR="$DATA_DIR_BASE/tpcds-queries"

if [[ ! -f "$TPCDS_DATA_DIR/store_sales.dat" || ! -f "$TPCDS_QUERIES_DIR/query1.sql" ]]; then
    log "tpcds data/queries missing — running setup.sh"
    bash "$(dirname "$0")/setup.sh" >/dev/null
fi

# TPC-DS has 24 tables. The load order below respects PK→FK references, not
# that AionDB enforces them — but it keeps PostgreSQL happy if you later
# enable the foreign keys.
TPCDS_TABLES=(
    dbgen_version income_band customer_address customer_demographics
    date_dim warehouse ship_mode time_dim reason household_demographics
    promotion item store call_center customer_address_history
    web_site store_returns customer_demographics
    customer web_page catalog_page inventory catalog_returns
    web_returns web_sales catalog_sales store_sales
)
# Deduplicate while preserving order (some templates repeat entries above).
mapfile -t TPCDS_TABLES < <(printf '%s\n' "${TPCDS_TABLES[@]}" | awk '!seen[$0]++')

load_tpcds_on_engine() {
    local engine="$1" psql_fn="$2"
    local failed=0
    log "creating tpcds schema on $engine"
    if ! "$psql_fn" -qAtf "$TPCDS_SCHEMA_FILE" >/dev/null 2>"$STATE_DIR/tpcds-schema-$engine.log"; then
        tail -20 "$STATE_DIR/tpcds-schema-$engine.log" >&2 || true
        warn "tpcds.sql failed on $engine — aborting load"
        return 1
    fi
    local loaded=0
    for tbl in "${TPCDS_TABLES[@]}"; do
        local src="$TPCDS_DATA_DIR/${tbl}.dat"
        [[ -f "$src" ]] || continue
        log "  COPY $tbl"
        local start end s
        start=$(date +%s.%N)
        if sed 's/|$//' "$src" | tr '|' '\t' \
            | "$psql_fn" -c "COPY $tbl FROM STDIN;" >/dev/null 2>"$STATE_DIR/tpcds-copy-$engine-$tbl.log"; then
            end=$(date +%s.%N)
            s=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.2f", e-s}')
            log "    loaded in ${s}s"
            loaded=$((loaded + 1))
        else
            tail -5 "$STATE_DIR/tpcds-copy-$engine-$tbl.log" >&2 || true
            warn "COPY $tbl FAILED on $engine"
            failed=1
        fi
    done
    log "loaded $loaded tables on $engine"
    if (( failed != 0 )); then
        warn "TPC-DS load incomplete on $engine; skipping query execution"
        return 1
    fi
    return 0
}

run_tpcds_queries() {
    local engine="$1"
    local tmo="$AIONDB_QUERY_TIMEOUT_S"
    [[ "$engine" == "pg" ]] && tmo="$PG_QUERY_TIMEOUT_S"
    # dsqgen outputs query1.sql … query99.sql in TPCDS_QUERIES_DIR. Some of
    # them may be missing if dsqgen failed for a template.
    for q in $(seq 1 99); do
        local qfile="$TPCDS_QUERIES_DIR/query${q}.sql"
        [[ -f "$qfile" ]] || continue
        local outfile="$STATE_DIR/tpcds-$engine-q${q}.out"
        local line
        line=$(run_query_timed "q${q}" "$engine" "$qfile" "$outfile" "$tmo")
        printf 'tpcds\t%s\n' "$line" >> "$REPORT"
    done
}

REPORT="$STATE_DIR/tpcds-report.tsv"
report_header "$REPORT"

install_aiondb_exit_trap

for engine in $BENCH_ENGINES; do
    case "$engine" in
        aiondb)
            aiondb_start
            load_tpcds_on_engine aiondb aiondb_psql || { aiondb_stop; continue; }
            run_tpcds_queries aiondb
            aiondb_stop
            ;;
        pg)
            pg_ensure_db
            pg_reset_public_schema
            load_tpcds_on_engine pg pg_psql || continue
            run_tpcds_queries pg
            ;;
        *) warn "unknown engine $engine, skipping" ;;
    esac
done

summarize_report "$REPORT"
compare_report_engines "$REPORT"
