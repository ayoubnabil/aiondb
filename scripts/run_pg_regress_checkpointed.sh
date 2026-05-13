#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
RUN_DIR="${1:-$ROOT_DIR/pg_regress_runs/$TIMESTAMP}"
PROGRESS_DIR="$RUN_DIR/progress"
STDOUT_LOG="$RUN_DIR/pg_regress.stdout.txt"
STDERR_LOG="$RUN_DIR/pg_regress.stderr.txt"
LATEST_SUMMARY="$PROGRESS_DIR/latest_summary.md"
FINAL_SUMMARY="$RUN_DIR/final_or_latest_summary.md"
RSS_LIMIT_KB="${PG_REGRESS_RSS_LIMIT_KB:-280000}"
TIME_LIMIT_SEC="${PG_REGRESS_TIME_LIMIT_SEC:-900}"
MAX_RESULT_ROWS="${PG_REGRESS_MAX_RESULT_ROWS:-120000}"
MAX_RESULT_MB="${PG_REGRESS_MAX_RESULT_MB:-16}"
MAX_MEMORY_MB="${PG_REGRESS_MAX_MEMORY_MB:-256}"
MAX_TEMP_MB="${PG_REGRESS_MAX_TEMP_MB:-256}"
INCLUDE_LOCAL_PROBES="${PG_REGRESS_INCLUDE_LOCAL_PROBES:-0}"
INCLUDE_SUITES="${PG_REGRESS_INCLUDE_SUITES:-}"
EXCLUDE_SUITES="${PG_REGRESS_EXCLUDE_SUITES:-}"
RSS_BREACH_COUNT_RAW="${PG_REGRESS_RSS_BREACH_COUNT:-3}"
MONITOR_INTERVAL_RAW="${PG_REGRESS_MONITOR_INTERVAL_SEC:-1}"
STALL_LIMIT_RAW="${PG_REGRESS_STALL_LIMIT_SEC:-300}"
PRLIMIT_AS_KB_RAW="${PG_REGRESS_PRLIMIT_AS_KB:-1048576}"
PRLIMIT_AS_KB_MIN=1048576
if [[ "$RSS_BREACH_COUNT_RAW" =~ ^[0-9]+$ ]] && (( RSS_BREACH_COUNT_RAW > 0 )); then
  RSS_BREACH_COUNT="$RSS_BREACH_COUNT_RAW"
else
  RSS_BREACH_COUNT=3
fi
if [[ "$MONITOR_INTERVAL_RAW" =~ ^[0-9]+$ ]] && (( MONITOR_INTERVAL_RAW > 0 )); then
  MONITOR_INTERVAL_SEC="$MONITOR_INTERVAL_RAW"
else
  MONITOR_INTERVAL_SEC=1
fi
if [[ "$STALL_LIMIT_RAW" =~ ^[0-9]+$ ]]; then
  STALL_LIMIT_SEC="$STALL_LIMIT_RAW"
else
  STALL_LIMIT_SEC=300
fi
if [[ "$PRLIMIT_AS_KB_RAW" =~ ^[0-9]+$ ]] && (( PRLIMIT_AS_KB_RAW > 0 )) && command -v prlimit >/dev/null 2>&1; then
  PRLIMIT_AS_KB="$PRLIMIT_AS_KB_RAW"
  PRLIMIT_AS_KB_ADJUSTED_FROM=0
  if (( PRLIMIT_AS_KB < PRLIMIT_AS_KB_MIN )); then
    PRLIMIT_AS_KB_ADJUSTED_FROM="$PRLIMIT_AS_KB"
    PRLIMIT_AS_KB="$PRLIMIT_AS_KB_MIN"
  fi
  PRLIMIT_AS_BYTES="$((PRLIMIT_AS_KB * 1024))"
  PRLIMIT_ENABLED=1
else
  PRLIMIT_AS_KB=0
  PRLIMIT_AS_KB_ADJUSTED_FROM=0
  PRLIMIT_AS_BYTES=0
  PRLIMIT_ENABLED=0
fi

sum_rss_kb() {
  local root_pid="$1"
  local rss
  if [[ "${USE_SETSID:-0}" == "1" ]]; then
    rss="$(ps -o rss= -g "$root_pid" 2>/dev/null | awk '{s+=$1} END {print s+0}')"
  else
    rss="$(ps -o rss= -p "$root_pid" --ppid "$root_pid" 2>/dev/null | awk '{s+=$1} END {print s+0}')"
  fi
  if [[ "$rss" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$rss"
  else
    printf '0\n'
  fi
}

sum_cpu_sec() {
  local root_pid="$1"
  local cpu
  if [[ "${USE_SETSID:-0}" == "1" ]]; then
    cpu="$(ps -o cputimes= -g "$root_pid" 2>/dev/null | awk '{s+=$1} END {print s+0}')"
  else
    cpu="$(ps -o cputimes= -p "$root_pid" --ppid "$root_pid" 2>/dev/null | awk '{s+=$1} END {print s+0}')"
  fi
  if [[ "$cpu" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$cpu"
  else
    printf '0\n'
  fi
}

latest_activity_ts() {
  local latest=0
  local file ts
  for file in "$@"; do
    [[ -e "$file" ]] || continue
    ts="$(stat -c %Y "$file" 2>/dev/null || echo 0)"
    if [[ "$ts" =~ ^[0-9]+$ ]] && (( ts > latest )); then
      latest="$ts"
    fi
  done
  printf '%s\n' "$latest"
}

terminate_target() {
  local root_pid="$1"
  if [[ "${USE_SETSID:-0}" == "1" ]]; then
    kill -TERM -- "-$root_pid" 2>/dev/null || true
    sleep 2
    kill -KILL -- "-$root_pid" 2>/dev/null || true
  else
    kill -TERM "$root_pid" 2>/dev/null || true
    sleep 2
    kill -KILL "$root_pid" 2>/dev/null || true
  fi
}

resolve_runner() {
  local candidate
  for candidate in \
    "$ROOT_DIR/.pg-regress/target/debug/pg-regress" \
    "$ROOT_DIR/.pg-regress/target/release/pg-regress"
  do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  return 1
}

mkdir -p "$RUN_DIR" "$PROGRESS_DIR"

cat >"$RUN_DIR/launch_info.txt" <<EOF
timestamp=$TIMESTAMP
root_dir=$ROOT_DIR
run_dir=$RUN_DIR
stdout_log=$STDOUT_LOG
stderr_log=$STDERR_LOG
progress_dir=$PROGRESS_DIR
latest_summary=$LATEST_SUMMARY
final_summary=$FINAL_SUMMARY
pg_regress_file=${PG_REGRESS_FILE:-}
rss_limit_kb=$RSS_LIMIT_KB
time_limit_sec=$TIME_LIMIT_SEC
max_result_rows=$MAX_RESULT_ROWS
max_result_mb=$MAX_RESULT_MB
max_memory_mb=$MAX_MEMORY_MB
max_temp_mb=$MAX_TEMP_MB
include_local_probes=$INCLUDE_LOCAL_PROBES
include_suites=$INCLUDE_SUITES
exclude_suites=$EXCLUDE_SUITES
rss_breach_count=$RSS_BREACH_COUNT
monitor_interval_sec=$MONITOR_INTERVAL_SEC
stall_limit_sec=$STALL_LIMIT_SEC
prlimit_as_kb=$PRLIMIT_AS_KB
prlimit_as_kb_min=$PRLIMIT_AS_KB_MIN
prlimit_as_kb_adjusted_from=$PRLIMIT_AS_KB_ADJUSTED_FROM
prlimit_enabled=$PRLIMIT_ENABLED
rss_scope=$(if command -v setsid >/dev/null 2>&1; then echo "process_group"; else echo "pid_plus_children"; fi)
EOF

echo "Building pg-regress binary..."
CARGO_BUILD_JOBS=1 cargo build --quiet --manifest-path "$ROOT_DIR/.pg-regress/Cargo.toml" --bin pg-regress

if ! RUNNER="$(resolve_runner)"; then
  RUNNER=""
fi
if [[ -z "$RUNNER" ]]; then
  echo "Runner binary not found: $RUNNER" >&2
  exit 1
fi

echo "Launching checkpointed pg-regress run..."
echo "Run directory: $RUN_DIR"
echo "Latest progress summary: $LATEST_SUMMARY"
echo "Guards: rss<=${RSS_LIMIT_KB}kB (streak>=${RSS_BREACH_COUNT}), time<=${TIME_LIMIT_SEC}s, sample=${MONITOR_INTERVAL_SEC}s, stall<=${STALL_LIMIT_SEC}s (0=disabled)"
echo "Engine caps: rows<=${MAX_RESULT_ROWS}, result<=${MAX_RESULT_MB}MB, mem<=${MAX_MEMORY_MB}MB, temp<=${MAX_TEMP_MB}MB"
echo "Suite config: include_local_probes=${INCLUDE_LOCAL_PROBES}, include_suites='${INCLUDE_SUITES}', exclude_suites='${EXCLUDE_SUITES}'"
if (( PRLIMIT_ENABLED == 1 )); then
  echo "Hard virtual memory cap: ${PRLIMIT_AS_KB}kB (via prlimit)"
  if (( PRLIMIT_AS_KB_ADJUSTED_FROM > 0 )); then
    echo "Requested PG_REGRESS_PRLIMIT_AS_KB=${PRLIMIT_AS_KB_ADJUSTED_FROM} adjusted to safe minimum ${PRLIMIT_AS_KB_MIN}kB"
  fi
fi

set +e
guard_reason=""
max_rss_kb=0
start_ts="$(date +%s)"
rss_breach_hits=0
rss_breach_streak_max=0
max_idle_sec=0
max_cpu_sec=0

USE_SETSID=0
if command -v setsid >/dev/null 2>&1; then
  USE_SETSID=1
fi
PRLIMIT_CMD=()
if (( PRLIMIT_ENABLED == 1 )); then
  PRLIMIT_CMD=(prlimit --as="$PRLIMIT_AS_BYTES")
fi

cleanup_runner() {
  if [[ -n "${PID:-}" ]] && kill -0 "$PID" 2>/dev/null; then
    terminate_target "$PID"
  fi
}
trap cleanup_runner EXIT

if command -v ionice >/dev/null 2>&1; then
  if [[ "$USE_SETSID" == "1" ]]; then
    setsid env \
      PG_REGRESS_PROGRESS_DIR="$PROGRESS_DIR" \
      PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
      PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
      PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
      PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
      PG_REGRESS_INCLUDE_LOCAL_PROBES="$INCLUDE_LOCAL_PROBES" \
      PG_REGRESS_INCLUDE_SUITES="$INCLUDE_SUITES" \
      PG_REGRESS_EXCLUDE_SUITES="$EXCLUDE_SUITES" \
      "${PRLIMIT_CMD[@]}" \
      ionice -c3 nice -n 10 "$RUNNER" >"$STDOUT_LOG" 2>"$STDERR_LOG" &
  else
    env \
      PG_REGRESS_PROGRESS_DIR="$PROGRESS_DIR" \
      PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
      PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
      PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
      PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
      PG_REGRESS_INCLUDE_LOCAL_PROBES="$INCLUDE_LOCAL_PROBES" \
      PG_REGRESS_INCLUDE_SUITES="$INCLUDE_SUITES" \
      PG_REGRESS_EXCLUDE_SUITES="$EXCLUDE_SUITES" \
      "${PRLIMIT_CMD[@]}" \
      ionice -c3 nice -n 10 "$RUNNER" >"$STDOUT_LOG" 2>"$STDERR_LOG" &
  fi
else
  if [[ "$USE_SETSID" == "1" ]]; then
    setsid env \
      PG_REGRESS_PROGRESS_DIR="$PROGRESS_DIR" \
      PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
      PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
      PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
      PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
      PG_REGRESS_INCLUDE_LOCAL_PROBES="$INCLUDE_LOCAL_PROBES" \
      PG_REGRESS_INCLUDE_SUITES="$INCLUDE_SUITES" \
      PG_REGRESS_EXCLUDE_SUITES="$EXCLUDE_SUITES" \
      "${PRLIMIT_CMD[@]}" \
      nice -n 10 "$RUNNER" >"$STDOUT_LOG" 2>"$STDERR_LOG" &
  else
    env \
      PG_REGRESS_PROGRESS_DIR="$PROGRESS_DIR" \
      PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
      PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
      PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
      PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
      PG_REGRESS_INCLUDE_LOCAL_PROBES="$INCLUDE_LOCAL_PROBES" \
      PG_REGRESS_INCLUDE_SUITES="$INCLUDE_SUITES" \
      PG_REGRESS_EXCLUDE_SUITES="$EXCLUDE_SUITES" \
      "${PRLIMIT_CMD[@]}" \
      nice -n 10 "$RUNNER" >"$STDOUT_LOG" 2>"$STDERR_LOG" &
  fi
fi
PID=$!
last_progress_ts="$start_ts"
last_cpu_sec=0
PROGRESS_CSV="$PROGRESS_DIR/per_file_progress.csv"
PROGRESS_STATUS="$PROGRESS_DIR/status.txt"

while kill -0 "$PID" 2>/dev/null; do
  rss_raw="$(sum_rss_kb "$PID" || true)"
  rss_kb="${rss_raw:-0}"
  if [[ "$rss_kb" =~ ^[0-9]+$ ]] && (( rss_kb > max_rss_kb )); then
    max_rss_kb="$rss_kb"
  fi
  if (( rss_kb > RSS_LIMIT_KB )); then
    ((rss_breach_hits += 1))
    if (( rss_breach_hits > rss_breach_streak_max )); then
      rss_breach_streak_max="$rss_breach_hits"
    fi
  else
    rss_breach_hits=0
  fi
  cpu_raw="$(sum_cpu_sec "$PID" || true)"
  cpu_sec="${cpu_raw:-0}"
  if [[ "$cpu_sec" =~ ^[0-9]+$ ]] && (( cpu_sec > max_cpu_sec )); then
    max_cpu_sec="$cpu_sec"
  fi

  now_ts="$(date +%s)"
  if [[ "$cpu_sec" =~ ^[0-9]+$ ]] && (( cpu_sec > last_cpu_sec )); then
    last_progress_ts="$now_ts"
    last_cpu_sec="$cpu_sec"
  fi
  activity_ts="$(latest_activity_ts "$STDOUT_LOG" "$STDERR_LOG" "$LATEST_SUMMARY" "$PROGRESS_CSV" "$PROGRESS_STATUS")"
  if [[ "$activity_ts" =~ ^[0-9]+$ ]] && (( activity_ts > last_progress_ts )); then
    last_progress_ts="$activity_ts"
  fi
  idle_sec="$((now_ts - last_progress_ts))"
  if (( idle_sec > max_idle_sec )); then
    max_idle_sec="$idle_sec"
  fi
  elapsed="$((now_ts - start_ts))"
  if (( rss_breach_hits >= RSS_BREACH_COUNT )); then
    guard_reason="rss_limit"
    terminate_target "$PID"
    break
  fi
  if (( STALL_LIMIT_SEC > 0 && idle_sec > STALL_LIMIT_SEC )); then
    guard_reason="stall_limit"
    terminate_target "$PID"
    break
  fi
  if (( elapsed > TIME_LIMIT_SEC )); then
    guard_reason="time_limit"
    terminate_target "$PID"
    break
  fi
  sleep "$MONITOR_INTERVAL_SEC"
done

wait "$PID"
STATUS=$?
end_ts="$(date +%s)"
elapsed="$((end_ts - start_ts))"
set -e

if [[ -f "$LATEST_SUMMARY" ]]; then
  cp -f "$LATEST_SUMMARY" "$FINAL_SUMMARY"
fi

cat >"$RUN_DIR/status.txt" <<EOF
exit_status=$STATUS
guard_reason=${guard_reason:-none}
max_rss_kb=$max_rss_kb
elapsed_sec=$elapsed
rss_breach_streak_max=$rss_breach_streak_max
max_idle_sec=$max_idle_sec
max_cpu_sec=$max_cpu_sec
completed_at=$(date --iso-8601=seconds)
EOF

echo "pg-regress exit status: $STATUS"
echo "guard_reason: ${guard_reason:-none}"
echo "max_rss_kb: $max_rss_kb"
echo "elapsed_sec: $elapsed"
echo "max_idle_sec: $max_idle_sec"
echo "stdout: $STDOUT_LOG"
echo "stderr: $STDERR_LOG"
echo "progress: $LATEST_SUMMARY"

exit "$STATUS"
