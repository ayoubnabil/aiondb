#!/usr/bin/env bash
# benchmarks/run.sh — top-level dispatcher for the AionDB vs PostgreSQL harness.
#
# Usage:
#   benchmarks/run.sh <benchmark> [engine=both|aiondb|pg] [extra flags...]
#
# Benchmarks:
#   pgbench   — OLTP micro (native pgbench). Ready to run.
#   surreal-suite — SurrealDB 3 article-style tests against SurrealDB WS, AionDB, PostgreSQL stack.
#   crud-bench-official — official SurrealDB crud-bench Rust workload.
#   tpch      — TPC-H SF=1 analytical (needs external tpch-kit, auto-fetched).
#   job       — Join Order Benchmark (needs IMDb dump ~3.6 GB, auto-fetched).
#   tpcds     — TPC-DS SF=1 analytical (needs external tpcds-kit, auto-fetched).
#
# Common environment overrides (see benchmarks/common.sh for the full list):
#   BENCH_ENGINES="aiondb pg"    which engines to run (default: both)
#   AIONDB_PORT=15432            aiondb pgwire port
#   PG_PORT=5432                 postgres port
#   PG_DB=bench_ref              postgres database name (will be created)
#   TPCH_SCALE=1                 TPC-H scale factor
#   TPCDS_SCALE=1                TPC-DS scale factor
#
# Example:
#   BENCH_ENGINES=aiondb benchmarks/run.sh pgbench
#   PG_USER=postgres TPCH_SCALE=1 benchmarks/run.sh tpch

set -euo pipefail

BENCH_ROOT="$(cd "$(dirname "$0")" && pwd)"
BENCH_AUTO_CLEAN="${BENCH_AUTO_CLEAN:-1}"
BENCH_CLEAN_PHASE="${BENCH_CLEAN_PHASE:-end}" # end | both | start
BENCH_MAX_STATE_MB="${BENCH_MAX_STATE_MB:-1024}"
BENCH_KEEP_LOGS="${BENCH_KEEP_LOGS:-30}"
BENCH_KEEP_OUTS="${BENCH_KEEP_OUTS:-30}"
BENCH_CLEAN_ALL="${BENCH_CLEAN_ALL:-0}" # 1 => also remove .data and .tools at selected phase

usage() {
    sed -n '2,23p' "$0" | sed 's|^# \{0,1\}||'
}

if [[ $# -lt 1 ]]; then
    usage
    exit 2
fi

case "$1" in
    -h|--help|help) usage; exit 0 ;;
    pgbench) bench_script="$BENCH_ROOT/pgbench/run.sh" ;;
    surreal-suite|surrealsuite) bench_script="$BENCH_ROOT/surreal-suite/run.sh" ;;
    crud-bench-official|crudbench-official|crud-bench) bench_script="$BENCH_ROOT/crud-bench-official/run.sh" ;;
    tpch)    bench_script="$BENCH_ROOT/tpch/run.sh" ;;
    job)     bench_script="$BENCH_ROOT/job/run.sh" ;;
    tpcds)   bench_script="$BENCH_ROOT/tpcds/run.sh" ;;
    *)
        printf 'unknown benchmark: %s\n\n' "$1" >&2
        usage >&2
        exit 2
        ;;
esac
shift

if [[ ("$BENCH_AUTO_CLEAN" == "1" || "$BENCH_AUTO_CLEAN" == "true") \
   && ("$BENCH_CLEAN_PHASE" == "both" || "$BENCH_CLEAN_PHASE" == "start") ]]; then
    clean_args=(
        --max-state-mb "$BENCH_MAX_STATE_MB"
        --keep-logs "$BENCH_KEEP_LOGS"
        --keep-outs "$BENCH_KEEP_OUTS"
    )
    if [[ "$BENCH_CLEAN_ALL" == "1" || "$BENCH_CLEAN_ALL" == "true" ]]; then
        clean_args+=(--all)
    fi
    "$BENCH_ROOT/clean.sh" \
        "${clean_args[@]}" \
        >/dev/null || true
fi

if [[ ("$BENCH_AUTO_CLEAN" == "1" || "$BENCH_AUTO_CLEAN" == "true") \
   && ("$BENCH_CLEAN_PHASE" == "both" || "$BENCH_CLEAN_PHASE" == "end") ]]; then
    clean_args=(
        --max-state-mb "$BENCH_MAX_STATE_MB" \
        --keep-logs "$BENCH_KEEP_LOGS" \
        --keep-outs "$BENCH_KEEP_OUTS" \
    )
    if [[ "$BENCH_CLEAN_ALL" == "1" || "$BENCH_CLEAN_ALL" == "true" ]]; then
        clean_args+=(--all)
    fi
fi

set +e
"$bench_script" "$@"
status=$?
set -e

if [[ ("$BENCH_AUTO_CLEAN" == "1" || "$BENCH_AUTO_CLEAN" == "true") \
   && ("$BENCH_CLEAN_PHASE" == "both" || "$BENCH_CLEAN_PHASE" == "end") ]]; then
    "$BENCH_ROOT/clean.sh" "${clean_args[@]}" >/dev/null || true
fi

exit "$status"
