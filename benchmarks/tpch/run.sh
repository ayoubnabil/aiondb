#!/usr/bin/env bash
# benchmarks/tpch/run.sh — load TPC-H SF=$TPCH_SCALE and run the 22 queries
# against each engine in $BENCH_ENGINES.
#
# TPC-H `dbgen` produces pipe-delimited `.tbl` files with a trailing pipe on
# each line. We stream them through `sed` + `tr` to convert to
# tab-delimited text, then pipe into `COPY ... FROM STDIN`. This works
# identically against AionDB (which hard-wires tab-delim text in its COPY
# parser) and PostgreSQL (default text format is also tab).

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd psql
require_cmd awk
require_cmd python3

TPCH_SCALE="${TPCH_SCALE:-1}"
TPCH_TOOLS_DIR="$TOOLS_DIR_BASE/tpch-kit"
TPCH_DATA_DIR="$DATA_DIR_BASE/tpch-sf${TPCH_SCALE}"
TPCH_SCHEMA_FILE="$TPCH_TOOLS_DIR/dbgen/dss.ddl"
TPCH_QUERIES_DIR="$DATA_DIR_BASE/tpch-queries"

if [[ ! -f "$TPCH_DATA_DIR/lineitem.tbl" || ! -f "$TPCH_QUERIES_DIR/1.sql" ]]; then
    log "tpch data/queries missing — running setup.sh"
    bash "$(dirname "$0")/setup.sh" >/dev/null
fi

normalize_tpch_queries() {
    local changed=0
    local qfile
    for qfile in "$TPCH_QUERIES_DIR"/*.sql; do
        [[ -f "$qfile" ]] || continue
        if python3 - "$qfile" <<'PY'
import pathlib
import re
import sys

p = pathlib.Path(sys.argv[1])
s = p.read_text()
original = s

# q15 uses CREATE VIEW ... (column aliases) then SELECT + DROP VIEW.
# Some engines disagree on view-column alias exposure in that shape.
# Rewrite to an equivalent CTE that preserves semantics.
if p.name == "15.sql" and "create view revenue0" in s.lower():
    s = """-- normalized q15 (CTE form)
with revenue0 (supplier_no, total_revenue) as (
    select
        l_suppkey,
        sum(l_extendedprice * (1 - l_discount))
    from
        lineitem
    where
        l_shipdate >= date '1996-01-01'
        and l_shipdate < date '1996-01-01' + interval '3' month
    group by
        l_suppkey
)
select
    s_suppkey,
    s_name,
    s_address,
    s_phone,
    total_revenue
from
    supplier,
    revenue0
where
    s_suppkey = supplier_no
    and total_revenue = (
        select
            max(total_revenue)
        from
            revenue0
    )
order by
    s_suppkey;
"""

# qgen emits "interval '90' day (3)" for q1; PostgreSQL/AionDB parsers expect
# a regular interval literal.
s = re.sub(r"interval\s+'(\d+)'\s+day\s+\(3\)", r"interval '\1 day'", s, flags=re.IGNORECASE)

# qgen may terminate the SELECT before LIMIT:
#   ... order by ...;
#   limit 100;
# Normalize to a single SQL statement.
s = re.sub(r";\s*\n(\s*limit\s+-?\d+\s*;)", r"\n\1", s, flags=re.IGNORECASE)

# Some templates use "limit -1" as "no limit"; PostgreSQL rejects negative LIMIT.
s = re.sub(r"\n\s*limit\s+-1\s*;\s*$", "\n", s, flags=re.IGNORECASE | re.MULTILINE)

if s != original:
    p.write_text(s)
    raise SystemExit(3)  # signal "changed"
PY
        then
            :
        else
            rc=$?
            if [[ "$rc" -eq 3 ]]; then
                changed=$((changed + 1))
            else
                warn "failed to normalize query file: $qfile"
            fi
        fi
    done
    if (( changed > 0 )); then
        log "normalized $changed TPC-H query files for parser compatibility"
    fi
}
normalize_tpch_queries

REPORT="$STATE_DIR/tpch-report.tsv"
report_header "$REPORT"

# tpch tables in load order (parents before children for PG foreign keys)
TPCH_TABLES=(region nation supplier customer part partsupp orders lineitem)

load_tpch_on_engine() {
    local engine="$1" psql_fn="$2"
    local failed=0
    log "creating tpch schema on $engine"
    # The stock dss.ddl has `DROP TABLE` statements + CREATE; we pipe it once.
    # dss.ddl uses `set search_path` and single-table grant clauses that are
    # PG-specific but tolerated by our parser. If a DDL fails on aiondb we fall
    # back to an inline minimal schema.
    if ! "$psql_fn" -qAtf "$TPCH_SCHEMA_FILE" >/dev/null 2>"$STATE_DIR/tpch-schema-$engine.log"; then
        warn "dss.ddl failed on $engine — see $STATE_DIR/tpch-schema-$engine.log"
        warn "falling back to inline minimal TPC-H schema"
        "$psql_fn" -qAtf "$(dirname "$0")/schema_minimal.sql" >/dev/null
    fi

    for tbl in "${TPCH_TABLES[@]}"; do
        local src="$TPCH_DATA_DIR/${tbl}.tbl"
        [[ -f "$src" ]] || { warn "missing $src"; continue; }
        local n; n=$(wc -l < "$src")
        log "  COPY $tbl ($n lines)"
        # Strip trailing pipe and convert | to tab, pipe into COPY.
        local load_start load_end load_s
        load_start=$(date +%s.%N)
        if sed 's/|$//' "$src" | tr '|' '\t' \
            | "$psql_fn" -c "COPY $tbl FROM STDIN;" >/dev/null 2>"$STATE_DIR/tpch-copy-$engine-$tbl.log"; then
            load_end=$(date +%s.%N)
            load_s=$(awk -v s="$load_start" -v e="$load_end" 'BEGIN{printf "%.2f", e-s}')
            log "    loaded in ${load_s}s"
        else
            tail -5 "$STATE_DIR/tpch-copy-$engine-$tbl.log" >&2 || true
            warn "COPY $tbl FAILED on $engine"
            failed=1
        fi
    done
    if (( failed != 0 )); then
        warn "tpch load incomplete on $engine; skipping query execution"
        return 1
    fi
    log "running ANALYZE on $engine"
    if ! "$psql_fn" -qAtc "ANALYZE;" >/dev/null 2>"$STATE_DIR/tpch-analyze-$engine.log"; then
        warn "ANALYZE failed on $engine; continuing without fresh stats"
    fi
    return 0
}

run_tpch_queries() {
    local engine="$1"
    local tmo="$AIONDB_QUERY_TIMEOUT_S"
    [[ "$engine" == "pg" ]] && tmo="$PG_QUERY_TIMEOUT_S"
    for q in $(seq 1 22); do
        local qfile="$TPCH_QUERIES_DIR/${q}.sql"
        [[ -f "$qfile" ]] || { warn "missing query file $qfile"; continue; }
        local outfile="$STATE_DIR/tpch-$engine-q${q}.out"
        local line
        line=$(run_query_timed "q${q}" "$engine" "$qfile" "$outfile" "$tmo")
        printf 'tpch\t%s\n' "$line" >> "$REPORT"
    done
}

install_aiondb_exit_trap

for engine in $BENCH_ENGINES; do
    case "$engine" in
        aiondb)
            aiondb_start
            load_tpch_on_engine aiondb aiondb_psql || { aiondb_stop; continue; }
            run_tpch_queries aiondb
            aiondb_stop
            ;;
        pg)
            pg_ensure_db
            pg_reset_public_schema
            load_tpch_on_engine pg pg_psql || continue
            run_tpch_queries pg
            ;;
        *) warn "unknown engine $engine, skipping" ;;
    esac
done

summarize_report "$REPORT"
compare_report_engines "$REPORT"
