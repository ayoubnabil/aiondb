#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREV_ARG="${1:-}"
RUN_DIR_ARG="${2:-}"

if [[ -z "$PREV_ARG" ]]; then
  echo "Usage: $0 <previous-run-id-or-dir> [new-run-dir]" >&2
  exit 1
fi

if [[ -d "$PREV_ARG" ]]; then
  PREV_DIR="$PREV_ARG"
  PREV_ID="$(basename "$PREV_DIR")"
else
  PREV_ID="$PREV_ARG"
  PREV_DIR="$ROOT_DIR/pg_regress_runs/$PREV_ID"
fi

if [[ ! -d "$PREV_DIR" ]]; then
  echo "Previous run directory not found: $PREV_DIR" >&2
  exit 1
fi

PREV_PROGRESS_CSV="$PREV_DIR/progress/per_file_progress.csv"
if [[ ! -f "$PREV_PROGRESS_CSV" ]]; then
  echo "Missing checkpoint CSV: $PREV_PROGRESS_CSV" >&2
  exit 1
fi

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
RUN_DIR="${RUN_DIR_ARG:-$ROOT_DIR/pg_regress_runs/${TIMESTAMP}-resume-guarded-from-${PREV_ID}}"
LOG_DIR="$RUN_DIR/logs"
PROGRESS_DIR="$RUN_DIR/progress"
RSS_LIMIT_KB="${PG_REGRESS_RSS_LIMIT_KB:-280000}"
TIME_LIMIT_SEC="${PG_REGRESS_TIME_LIMIT_SEC:-900}"
MAX_RESULT_ROWS="${PG_REGRESS_MAX_RESULT_ROWS:-120000}"
MAX_RESULT_MB="${PG_REGRESS_MAX_RESULT_MB:-16}"
MAX_MEMORY_MB="${PG_REGRESS_MAX_MEMORY_MB:-256}"
MAX_TEMP_MB="${PG_REGRESS_MAX_TEMP_MB:-256}"
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
  sum_metric_for_target "$1" rss
}

sum_cpu_sec() {
  sum_metric_for_target "$1" cputimes
}

collect_target_pids() {
  local root_pid="$1"
  local discovered=""
  local pid_csv
  local fallback

  if [[ "${USE_SETSID:-0}" == "1" ]] && command -v pgrep >/dev/null 2>&1; then
    discovered="$(pgrep -g "$root_pid" 2>/dev/null || true)"
  fi

  pid_csv="$(
    {
      printf '%s\n' "$root_pid"
      if [[ -n "$discovered" ]]; then
        printf '%s\n' "$discovered"
      fi
    } | awk 'NF > 0 && $1 ~ /^[0-9]+$/ { print $1 }' | sort -u | paste -sd, -
  )"
  if [[ -n "$pid_csv" ]]; then
    printf '%s\n' "$pid_csv"
    return 0
  fi

  fallback="$(ps -o pid= -p "$root_pid" --ppid "$root_pid" 2>/dev/null || true)"
  pid_csv="$(
    printf '%s\n' "$fallback" \
      | awk 'NF > 0 && $1 ~ /^[0-9]+$/ { print $1 }' \
      | sort -u \
      | paste -sd, -
  )"
  printf '%s\n' "$pid_csv"
}

sum_metric_for_target() {
  local root_pid="$1"
  local field="$2"
  local pid_csv total

  pid_csv="$(collect_target_pids "$root_pid")"
  if [[ -z "$pid_csv" ]]; then
    printf '0\n'
    return 0
  fi

  total="$(ps -o "${field}"= -p "$pid_csv" 2>/dev/null | awk '{s+=$1} END {print s+0}')"
  if [[ "$total" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$total"
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

mkdir -p "$LOG_DIR" "$PROGRESS_DIR"

echo "Building pg-regress binary..."
CARGO_BUILD_JOBS=1 cargo build --quiet --manifest-path "$ROOT_DIR/.pg-regress/Cargo.toml" --bin pg-regress

if ! RUNNER="$(resolve_runner)"; then
  RUNNER=""
fi
if [[ -z "$RUNNER" ]]; then
  echo "Runner not found or not executable after build" >&2
  exit 1
fi

mapfile -t REMAINING < <(
  comm -23 \
    <(
      find "$ROOT_DIR/.pg-regress/sql" -maxdepth 1 -type f -name '*.sql' -printf '%f\n' \
        | sed 's/\.sql$//' \
        | sort
    ) \
    <(tail -n +2 "$PREV_PROGRESS_CSV" | cut -d, -f1 | sort)
)

TOTAL="${#REMAINING[@]}"
if (( TOTAL == 0 )); then
  echo "No remaining files to run from checkpoint $PREV_ID"
  exit 0
fi

printf '%s\n' "${REMAINING[@]}" > "$RUN_DIR/remaining_files.txt"

cat > "$RUN_DIR/launch_info.txt" <<EOF
resumed_from=$PREV_ID
started_at=$(date --iso-8601=seconds)
root_dir=$ROOT_DIR
run_dir=$RUN_DIR
runner=$RUNNER
memory_rss_limit_kb=$RSS_LIMIT_KB
time_limit_sec=$TIME_LIMIT_SEC
max_result_rows=$MAX_RESULT_ROWS
max_result_mb=$MAX_RESULT_MB
max_memory_mb=$MAX_MEMORY_MB
max_temp_mb=$MAX_TEMP_MB
rss_breach_count=$RSS_BREACH_COUNT
monitor_interval_sec=$MONITOR_INTERVAL_SEC
stall_limit_sec=$STALL_LIMIT_SEC
prlimit_as_kb=$PRLIMIT_AS_KB
prlimit_as_kb_min=$PRLIMIT_AS_KB_MIN
prlimit_as_kb_adjusted_from=$PRLIMIT_AS_KB_ADJUSTED_FROM
prlimit_enabled=$PRLIMIT_ENABLED
rss_scope=$(if command -v setsid >/dev/null 2>&1; then echo "process_group"; else echo "pid_plus_children"; fi)
remaining_count=$TOTAL
first_remaining=${REMAINING[0]}
strategy=continue_remaining_files_with_memory_guard
EOF

printf 'file,matched,total,rate_pct,exit_status,guard_reason,max_rss_kb\n' \
  > "$RUN_DIR/per_file_progress.csv"
: > "$RUN_DIR/resume_console.log"
printf 'status=running\nprocessed_files=0\ntotal_files=%s\nupdated_at=%s\n' \
  "$TOTAL" "$(date --iso-8601=seconds)" > "$PROGRESS_DIR/status.txt"

echo "Resume run: $RUN_DIR"
echo "Remaining files: $TOTAL"
echo "Runner: $RUNNER"
if (( PRLIMIT_ENABLED == 1 )); then
  echo "Hard virtual memory cap: ${PRLIMIT_AS_KB}kB (via prlimit)"
  if (( PRLIMIT_AS_KB_ADJUSTED_FROM > 0 )); then
    echo "Requested PG_REGRESS_PRLIMIT_AS_KB=${PRLIMIT_AS_KB_ADJUSTED_FROM} adjusted to safe minimum ${PRLIMIT_AS_KB_MIN}kB"
  fi
fi

USE_SETSID=0
if command -v setsid >/dev/null 2>&1; then
  USE_SETSID=1
fi
PRLIMIT_CMD=()
if (( PRLIMIT_ENABLED == 1 )); then
  PRLIMIT_CMD=(prlimit --as="$PRLIMIT_AS_BYTES")
fi

cleanup_runner() {
  if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
    terminate_target "$pid"
  fi
}
trap cleanup_runner EXIT

for (( INDEX = 0; INDEX < TOTAL; INDEX++ )); do
  file="${REMAINING[$INDEX]}"
  stdout_file="$LOG_DIR/$file.stdout.txt"
  stderr_file="$LOG_DIR/$file.stderr.txt"
  start_ts="$(date +%s)"
  max_rss_kb=0
  guard_reason=""
  rss_breach_hits=0
  rss_breach_streak_max=0
  max_idle_sec=0
  max_cpu_sec=0

  printf '[%s] START %s (%s/%s)\n' \
    "$(date --iso-8601=seconds)" "$file" "$((INDEX + 1))" "$TOTAL" >> "$RUN_DIR/resume_console.log"
  echo "[$((INDEX + 1))/$TOTAL] $file"

  if command -v ionice >/dev/null 2>&1; then
    if [[ "$USE_SETSID" == "1" ]]; then
      setsid env \
        PG_REGRESS_FILE="$file" \
        PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
        PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
        PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
        PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
        "${PRLIMIT_CMD[@]}" \
        ionice -c3 nice -n 10 "$RUNNER" >"$stdout_file" 2>"$stderr_file" &
    else
      env \
        PG_REGRESS_FILE="$file" \
        PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
        PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
        PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
        PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
        "${PRLIMIT_CMD[@]}" \
        ionice -c3 nice -n 10 "$RUNNER" >"$stdout_file" 2>"$stderr_file" &
    fi
  else
    if [[ "$USE_SETSID" == "1" ]]; then
      setsid env \
        PG_REGRESS_FILE="$file" \
        PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
        PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
        PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
        PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
        "${PRLIMIT_CMD[@]}" \
        nice -n 10 "$RUNNER" >"$stdout_file" 2>"$stderr_file" &
    else
      env \
        PG_REGRESS_FILE="$file" \
        PG_REGRESS_MAX_RESULT_ROWS="$MAX_RESULT_ROWS" \
        PG_REGRESS_MAX_RESULT_MB="$MAX_RESULT_MB" \
        PG_REGRESS_MAX_MEMORY_MB="$MAX_MEMORY_MB" \
        PG_REGRESS_MAX_TEMP_MB="$MAX_TEMP_MB" \
        "${PRLIMIT_CMD[@]}" \
        nice -n 10 "$RUNNER" >"$stdout_file" 2>"$stderr_file" &
    fi
  fi
  pid=$!
  last_progress_ts="$start_ts"
  last_cpu_sec=0

  while kill -0 "$pid" 2>/dev/null; do
    rss_raw="$(sum_rss_kb "$pid" || true)"
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
    cpu_raw="$(sum_cpu_sec "$pid" || true)"
    cpu_sec="${cpu_raw:-0}"
    if [[ "$cpu_sec" =~ ^[0-9]+$ ]] && (( cpu_sec > max_cpu_sec )); then
      max_cpu_sec="$cpu_sec"
    fi

    now_ts="$(date +%s)"
    if [[ "$cpu_sec" =~ ^[0-9]+$ ]] && (( cpu_sec > last_cpu_sec )); then
      last_progress_ts="$now_ts"
      last_cpu_sec="$cpu_sec"
    fi
    activity_ts="$(latest_activity_ts "$stdout_file" "$stderr_file")"
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
      terminate_target "$pid"
      break
    fi
    if (( STALL_LIMIT_SEC > 0 )); then
      if (( idle_sec > STALL_LIMIT_SEC )); then
        guard_reason="stall_limit"
        terminate_target "$pid"
        break
      fi
    fi
    if (( elapsed > TIME_LIMIT_SEC )); then
      guard_reason="time_limit"
      terminate_target "$pid"
      break
    fi
    sleep "$MONITOR_INTERVAL_SEC"
  done

  set +e
  wait "$pid"
  status=$?
  set -e

  end_ts="$(date +%s)"
  elapsed="$((end_ts - start_ts))"
  # Trust only runner control lines from stderr (runner emits RESULT/SKIP via eprintln!).
  result_line="$(grep -h '^RESULT|' "$stderr_file" 2>/dev/null | tail -n 1 || true)"
  skip_line="$(grep -h '^SKIP|' "$stderr_file" 2>/dev/null | tail -n 1 || true)"

  if [[ -n "$result_line" ]]; then
    IFS='|' read -r _ _matched_name _matched _total <<< "$result_line"
    if [[ ! "$_matched" =~ ^[0-9]+$ || ! "$_total" =~ ^[0-9]+$ || "$_matched" -gt "$_total" ]]; then
      result_line=""
      skip_line=""
    fi
  fi

  if [[ -n "$result_line" ]]; then
    IFS='|' read -r _ name matched total_cases <<< "$result_line"
    rate="$(
      awk -v m="$matched" -v t="$total_cases" \
        'BEGIN { if (t == 0) printf "0.00"; else printf "%.2f", (m / t) * 100 }'
    )"
    printf '%s,%s,%s,%s,%s,%s,%s\n' \
      "$name" "$matched" "$total_cases" "$rate" "$status" "$guard_reason" "$max_rss_kb" \
      >> "$RUN_DIR/per_file_progress.csv"
    printf '[%s] DONE %s %s/%s (%s%%) exit=%s guard=%s max_rss_kb=%s rss_streak_max=%s max_idle_sec=%s max_cpu_sec=%s elapsed=%ss\n' \
      "$(date --iso-8601=seconds)" "$name" "$matched" "$total_cases" "$rate" "$status" \
      "${guard_reason:-none}" "$max_rss_kb" "$rss_breach_streak_max" "$max_idle_sec" "$max_cpu_sec" "$elapsed" >> "$RUN_DIR/resume_console.log"
    echo "[$((INDEX + 1))/$TOTAL] $name -> $matched/$total_cases (${rate}%), exit=$status, guard=${guard_reason:-none}, max_rss=${max_rss_kb}kB, rss_streak_max=${rss_breach_streak_max}, max_idle=${max_idle_sec}s, max_cpu=${max_cpu_sec}s, ${elapsed}s"
  elif [[ -n "$skip_line" ]]; then
    IFS='|' read -r _ name reason <<< "$skip_line"
    printf '%s,0,0,0.00,%s,%s,%s\n' \
      "$name" "$status" "${guard_reason:-skip:$reason}" "$max_rss_kb" \
      >> "$RUN_DIR/per_file_progress.csv"
    printf '[%s] SKIP %s reason=%s exit=%s guard=%s max_rss_kb=%s rss_streak_max=%s max_idle_sec=%s max_cpu_sec=%s elapsed=%ss\n' \
      "$(date --iso-8601=seconds)" "$name" "$reason" "$status" "${guard_reason:-none}" \
      "$max_rss_kb" "$rss_breach_streak_max" "$max_idle_sec" "$max_cpu_sec" "$elapsed" >> "$RUN_DIR/resume_console.log"
    echo "[$((INDEX + 1))/$TOTAL] $name -> skipped ($reason), exit=$status, guard=${guard_reason:-none}, max_rss=${max_rss_kb}kB, rss_streak_max=${rss_breach_streak_max}, max_idle=${max_idle_sec}s, max_cpu=${max_cpu_sec}s, ${elapsed}s"
  else
    printf '%s,,,,%s,%s,%s\n' \
      "$file" "$status" "${guard_reason:-no_result}" "$max_rss_kb" \
      >> "$RUN_DIR/per_file_progress.csv"
    printf '[%s] NORESULT %s exit=%s guard=%s max_rss_kb=%s rss_streak_max=%s max_idle_sec=%s max_cpu_sec=%s elapsed=%ss\n' \
      "$(date --iso-8601=seconds)" "$file" "$status" "${guard_reason:-none}" \
      "$max_rss_kb" "$rss_breach_streak_max" "$max_idle_sec" "$max_cpu_sec" "$elapsed" >> "$RUN_DIR/resume_console.log"
    echo "[$((INDEX + 1))/$TOTAL] $file -> no RESULT line, exit=$status, guard=${guard_reason:-none}, max_rss=${max_rss_kb}kB, rss_streak_max=${rss_breach_streak_max}, max_idle=${max_idle_sec}s, max_cpu=${max_cpu_sec}s, ${elapsed}s"
  fi

  last_line="$(tail -n 1 "$RUN_DIR/per_file_progress.csv")"
  printf 'status=running\nprocessed_files=%s\ntotal_files=%s\nlast_file=%s\nlast_csv=%s\nupdated_at=%s\n' \
    "$((INDEX + 1))" "$TOTAL" "$file" "$last_line" "$(date --iso-8601=seconds)" \
    > "$PROGRESS_DIR/status.txt"
done

printf 'status=completed\nprocessed_files=%s\ntotal_files=%s\nupdated_at=%s\n' \
  "$TOTAL" "$TOTAL" "$(date --iso-8601=seconds)" > "$PROGRESS_DIR/status.txt"
echo "Resume run completed: $RUN_DIR"
