#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR_ARG="${1:-}"

read_meminfo_kb() {
  local key="$1"
  awk -v k="$key" '$1 == k ":" { print $2; exit }' /proc/meminfo 2>/dev/null || true
}

clamp_int() {
  local value="$1"
  local min="$2"
  local max="$3"
  if (( value < min )); then
    printf '%s\n' "$min"
    return 0
  fi
  if (( value > max )); then
    printf '%s\n' "$max"
    return 0
  fi
  printf '%s\n' "$value"
}

MEM_TOTAL_KB_RAW="$(read_meminfo_kb MemTotal)"
MEM_AVAILABLE_KB_RAW="$(read_meminfo_kb MemAvailable)"

if [[ "$MEM_TOTAL_KB_RAW" =~ ^[0-9]+$ ]] && (( MEM_TOTAL_KB_RAW > 0 )); then
  MEM_TOTAL_KB="$MEM_TOTAL_KB_RAW"
else
  MEM_TOTAL_KB=$((8 * 1024 * 1024))
fi
if [[ "$MEM_AVAILABLE_KB_RAW" =~ ^[0-9]+$ ]] && (( MEM_AVAILABLE_KB_RAW > 0 )); then
  MEM_AVAILABLE_KB="$MEM_AVAILABLE_KB_RAW"
else
  MEM_AVAILABLE_KB=$((MEM_TOTAL_KB / 2))
fi

# Keep a sizeable host reserve to avoid whole-machine OOM and UI freezes.
HOST_RESERVE_KB=$((MEM_TOTAL_KB / 3))
if (( HOST_RESERVE_KB < 1024 * 1024 )); then
  HOST_RESERVE_KB=$((1024 * 1024))
fi
if (( HOST_RESERVE_KB > MEM_AVAILABLE_KB - 256 * 1024 )); then
  HOST_RESERVE_KB=$((MEM_AVAILABLE_KB / 2))
fi
if (( HOST_RESERVE_KB < 0 )); then
  HOST_RESERVE_KB=0
fi

RUN_BUDGET_KB=$((MEM_AVAILABLE_KB - HOST_RESERVE_KB))
if (( RUN_BUDGET_KB < 256 * 1024 )); then
  RUN_BUDGET_KB=$((256 * 1024))
fi

RSS_DEFAULT_KB=$((RUN_BUDGET_KB / 2))
RSS_DEFAULT_KB="$(clamp_int "$RSS_DEFAULT_KB" 220000 3000000)"

PRLIMIT_DEFAULT_KB=$((RSS_DEFAULT_KB * 4))
PRLIMIT_DEFAULT_KB="$(clamp_int "$PRLIMIT_DEFAULT_KB" 1048576 6291456)"

MAX_MEMORY_DEFAULT_MB=$((RSS_DEFAULT_KB / 2048))
MAX_MEMORY_DEFAULT_MB="$(clamp_int "$MAX_MEMORY_DEFAULT_MB" 96 256)"
MAX_TEMP_DEFAULT_MB="$MAX_MEMORY_DEFAULT_MB"

MAX_RESULT_DEFAULT_MB=$((MAX_MEMORY_DEFAULT_MB / 8))
MAX_RESULT_DEFAULT_MB="$(clamp_int "$MAX_RESULT_DEFAULT_MB" 8 32)"

: "${PG_REGRESS_RSS_LIMIT_KB:=$RSS_DEFAULT_KB}"
: "${PG_REGRESS_RSS_BREACH_COUNT:=2}"
: "${PG_REGRESS_MONITOR_INTERVAL_SEC:=1}"
: "${PG_REGRESS_STALL_LIMIT_SEC:=300}"
: "${PG_REGRESS_TIME_LIMIT_SEC:=10800}"
: "${PG_REGRESS_PRLIMIT_AS_KB:=$PRLIMIT_DEFAULT_KB}"
: "${PG_REGRESS_MAX_RESULT_ROWS:=120000}"
: "${PG_REGRESS_MAX_RESULT_MB:=$MAX_RESULT_DEFAULT_MB}"
: "${PG_REGRESS_MAX_MEMORY_MB:=$MAX_MEMORY_DEFAULT_MB}"
: "${PG_REGRESS_MAX_TEMP_MB:=$MAX_TEMP_DEFAULT_MB}"
: "${CARGO_BUILD_JOBS:=1}"

export PG_REGRESS_RSS_LIMIT_KB
export PG_REGRESS_RSS_BREACH_COUNT
export PG_REGRESS_MONITOR_INTERVAL_SEC
export PG_REGRESS_STALL_LIMIT_SEC
export PG_REGRESS_TIME_LIMIT_SEC
export PG_REGRESS_PRLIMIT_AS_KB
export PG_REGRESS_MAX_RESULT_ROWS
export PG_REGRESS_MAX_RESULT_MB
export PG_REGRESS_MAX_MEMORY_MB
export PG_REGRESS_MAX_TEMP_MB
export CARGO_BUILD_JOBS

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
RUN_DIR="${RUN_DIR_ARG:-$ROOT_DIR/pg_regress_runs/${TIMESTAMP}-full-guarded-oomsafe}"

echo "OOM-safe pg_regress launcher"
echo "mem_total_kb=$MEM_TOTAL_KB"
echo "mem_available_kb=$MEM_AVAILABLE_KB"
echo "host_reserve_kb=$HOST_RESERVE_KB"
echo "run_budget_kb=$RUN_BUDGET_KB"
echo "PG_REGRESS_RSS_LIMIT_KB=$PG_REGRESS_RSS_LIMIT_KB"
echo "PG_REGRESS_PRLIMIT_AS_KB=$PG_REGRESS_PRLIMIT_AS_KB"
echo "PG_REGRESS_MAX_MEMORY_MB=$PG_REGRESS_MAX_MEMORY_MB"
echo "PG_REGRESS_MAX_TEMP_MB=$PG_REGRESS_MAX_TEMP_MB"
echo "PG_REGRESS_MAX_RESULT_MB=$PG_REGRESS_MAX_RESULT_MB"
echo "PG_REGRESS_TIME_LIMIT_SEC=$PG_REGRESS_TIME_LIMIT_SEC"
echo "run_dir=$RUN_DIR"

exec "$ROOT_DIR/scripts/run_pg_regress_checkpointed.sh" "$RUN_DIR"
