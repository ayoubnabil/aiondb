#!/usr/bin/env bash
# benchmarks/job/run.sh — load the IMDb dataset and run the 113 JOB queries
# against each engine in $BENCH_ENGINES.
#
# JOB ships 21 tables as `.csv` files with POSIX-flavored CSV quoting
# (doublequote escapes). AionDB's COPY FROM STDIN is hard-wired to
# tab-delimited text with \N null markers. We therefore convert each CSV
# to that format on the fly using a small Python helper, then pipe it into
# COPY FROM STDIN. The same converted stream is also fed to PostgreSQL so
# the comparison is strictly engine-vs-engine, not format-parser-vs-format-parser.

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd psql
require_cmd python3

JOB_REPO="$TOOLS_DIR_BASE/join-order-benchmark"
JOB_DATA="$DATA_DIR_BASE/job"

if [[ ! -f "$JOB_DATA/title.csv" || ! -f "$JOB_REPO/schema.sql" ]]; then
    log "JOB data/repo missing — running setup.sh"
    bash "$(dirname "$0")/setup.sh" >/dev/null
fi

JOB_TABLES=(
    aka_name aka_title cast_info char_name comp_cast_type company_name
    company_type complete_cast info_type keyword kind_type link_type
    movie_companies movie_info movie_info_idx movie_keyword movie_link
    name person_info role_type title
)

# Python one-liner: read CSV on stdin (dialect = excel, doublequote=True,
# escapechar='\\'), write tab-delimited text on stdout with empty fields
# translated to '\\N' (PG text-format NULL marker). This is the only place
# the harness assumes anything about the input format.
CSV_TO_TSV='
import csv, sys
r = csv.reader(sys.stdin, delimiter=",", quotechar="\"", escapechar="\\", doublequote=True)
out = sys.stdout
for row in r:
    # Empty value → NULL marker; otherwise escape TAB/NEWLINE/backslash.
    fields = []
    for v in row:
        if v == "":
            fields.append("\\N")
        else:
            v = v.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r")
            fields.append(v)
    out.write("\t".join(fields))
    out.write("\n")
'

REPORT="$STATE_DIR/job-report.tsv"
report_header "$REPORT"

load_job_on_engine() {
    local engine="$1" psql_fn="$2"
    local failed=0
    log "creating JOB schema on $engine"
    if ! "$psql_fn" -qAtf "$JOB_REPO/schema.sql" >/dev/null 2>"$STATE_DIR/job-schema-$engine.log"; then
        tail -20 "$STATE_DIR/job-schema-$engine.log" >&2 || true
        warn "JOB schema.sql failed on $engine — aborting load"
        return 1
    fi
    for tbl in "${JOB_TABLES[@]}"; do
        local src="$JOB_DATA/${tbl}.csv"
        [[ -f "$src" ]] || { warn "missing $src"; continue; }
        log "  COPY $tbl (from $src)"
        local start end s
        start=$(date +%s.%N)
        if python3 -c "$CSV_TO_TSV" < "$src" \
            | "$psql_fn" -c "COPY $tbl FROM STDIN;" >/dev/null 2>"$STATE_DIR/job-copy-$engine-$tbl.log"; then
            end=$(date +%s.%N)
            s=$(awk -v s="$start" -v e="$end" 'BEGIN{printf "%.2f", e-s}')
            log "    loaded in ${s}s"
        else
            tail -5 "$STATE_DIR/job-copy-$engine-$tbl.log" >&2 || true
            warn "COPY $tbl FAILED on $engine"
            failed=1
        fi
    done
    if (( failed != 0 )); then
        warn "JOB load incomplete on $engine; skipping query execution"
        return 1
    fi
    return 0
}

run_job_queries() {
    local engine="$1"
    local tmo="$AIONDB_QUERY_TIMEOUT_S"
    [[ "$engine" == "pg" ]] && tmo="$PG_QUERY_TIMEOUT_S"
    # JOB query files are named `<num><letter>.sql` e.g. 1a.sql, 17c.sql.
    # schema.sql and fkindexes.sql sit alongside — exclude them.
    local qfiles=()
    mapfile -t qfiles < <(find "$JOB_REPO" -maxdepth 1 -type f -name '[0-9]*.sql' | sort -V)
    for qfile in "${qfiles[@]}"; do
        local name; name=$(basename "$qfile" .sql)
        case "$name" in schema|fkindexes) continue ;; esac
        local outfile="$STATE_DIR/job-$engine-$name.out"
        local line
        line=$(run_query_timed "$name" "$engine" "$qfile" "$outfile" "$tmo")
        printf 'job\t%s\n' "$line" >> "$REPORT"
    done
}

install_aiondb_exit_trap

for engine in $BENCH_ENGINES; do
    case "$engine" in
        aiondb)
            aiondb_start
            load_job_on_engine aiondb aiondb_psql || { aiondb_stop; continue; }
            run_job_queries aiondb
            aiondb_stop
            ;;
        pg)
            pg_ensure_db
            pg_reset_public_schema
            load_job_on_engine pg pg_psql || continue
            run_job_queries pg
            ;;
        *) warn "unknown engine $engine, skipping" ;;
    esac
done

summarize_report "$REPORT"
compare_report_engines "$REPORT"
