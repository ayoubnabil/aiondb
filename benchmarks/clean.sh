#!/usr/bin/env bash
# benchmarks/clean.sh — cleanup helper for benchmark artifacts.
#
# Safe-by-default behavior:
# - cleans only benchmarks/.state transient artifacts (logs/query outputs)
# - keeps report TSV files and a configurable number of newest run artifacts
# - keeps benchmarks/.data and benchmarks/.tools
#
# Options:
#   --max-state-mb N   Target max size for .state (default: 1024)
#   --keep-logs N      Keep newest N .log files (default: 30)
#   --keep-outs N      Keep newest N .out files (default: 30)
#   --all              Also delete benchmarks/.data and benchmarks/.tools (destructive)
#   --dry-run          Print what would be deleted

set -euo pipefail

BENCH_ROOT="$(cd "$(dirname "$0")" && pwd)"
STATE_DIR="${STATE_DIR:-$BENCH_ROOT/.state}"
DATA_DIR_BASE="${DATA_DIR_BASE:-$BENCH_ROOT/.data}"
TOOLS_DIR_BASE="${TOOLS_DIR_BASE:-$BENCH_ROOT/.tools}"

MAX_STATE_MB="${BENCH_MAX_STATE_MB:-1024}"
KEEP_LOGS="${BENCH_KEEP_LOGS:-30}"
KEEP_OUTS="${BENCH_KEEP_OUTS:-30}"
DO_ALL=0
DRY_RUN=0

log() { printf '[bench-clean] %s\n' "$*" >&2; }

usage() {
  cat <<USAGE
Usage: benchmarks/clean.sh [options]
  --max-state-mb N   Target maximum size for .state (default: ${MAX_STATE_MB})
  --keep-logs N      Keep newest N .log files (default: ${KEEP_LOGS})
  --keep-outs N      Keep newest N .out files (default: ${KEEP_OUTS})
  --all              Also delete .data and .tools (destructive)
  --dry-run          Show actions only
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --max-state-mb)
      MAX_STATE_MB="$2"; shift 2 ;;
    --keep-logs)
      KEEP_LOGS="$2"; shift 2 ;;
    --keep-outs)
      KEEP_OUTS="$2"; shift 2 ;;
    --all)
      DO_ALL=1; shift ;;
    --dry-run)
      DRY_RUN=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      printf 'unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2 ;;
  esac
 done

[[ -d "$STATE_DIR" ]] || { log "state dir not found: $STATE_DIR"; exit 0; }

rm_safe() {
  local path="$1"
  [[ -e "$path" ]] || return 0
  if (( DRY_RUN == 1 )); then
    printf 'DRY-RUN rm -f %q\n' "$path"
  else
    rm -f -- "$path"
  fi
}

rm_tree_safe() {
  local path="$1"
  [[ -e "$path" ]] || return 0
  if (( DRY_RUN == 1 )); then
    printf 'DRY-RUN rm -rf %q\n' "$path"
  else
    rm -rf -- "$path"
  fi
}

managed_state_size_bytes() {
  # Size budget concerns top-level transient artifacts only.
  # Engine state dirs (e.g. pg-local) are intentionally excluded.
  find "$STATE_DIR" -maxdepth 1 -type f -printf '%s\n' 2>/dev/null \
    | awk '{s+=$1} END{print s+0}'
}

initial_bytes="$(managed_state_size_bytes)"
log "initial .state size: $(( initial_bytes / 1024 / 1024 )) MB"

# 1) Keep report TSV files; prune old logs except newest KEEP_LOGS.
mapfile -t logs < <(find "$STATE_DIR" -maxdepth 1 -type f -name '*.log' -printf '%T@ %p\n' | sort -nr | awk '{ $1=""; sub(/^ /,""); print }')
if (( ${#logs[@]} > KEEP_LOGS )); then
  for ((i=KEEP_LOGS; i<${#logs[@]}; i++)); do
    rm_safe "${logs[$i]}"
  done
fi

# 2) Prune old query outputs except newest KEEP_OUTS.
mapfile -t outs < <(find "$STATE_DIR" -maxdepth 1 -type f -name '*.out' -printf '%T@ %p\n' | sort -nr | awk '{ $1=""; sub(/^ /,""); print }')
if (( ${#outs[@]} > KEEP_OUTS )); then
  for ((i=KEEP_OUTS; i<${#outs[@]}; i++)); do
    rm_safe "${outs[$i]}"
  done
fi

# 3) If still above threshold, trim oldest non-report files first.
max_bytes=$(( MAX_STATE_MB * 1024 * 1024 ))
current_bytes="$(managed_state_size_bytes)"
if (( current_bytes > max_bytes )); then
  log ".state above threshold (${MAX_STATE_MB} MB), trimming oldest non-report files"
  while IFS= read -r f; do
    [[ -z "$f" ]] && continue
    # keep reports
    [[ "$f" == *.tsv ]] && continue
    rm_safe "$f"
    current_bytes="$(managed_state_size_bytes)"
    (( current_bytes <= max_bytes )) && break
  done < <(find "$STATE_DIR" -maxdepth 1 -type f -printf '%T@ %p\n' | sort -n | awk '{ $1=""; sub(/^ /,""); print }')
fi

# 4) Optional destructive cleanup for heavy datasets/tooling.
if (( DO_ALL == 1 )); then
  log "--all enabled: removing .data, .tools and heavyweight engine state"
  rm_tree_safe "$STATE_DIR/pg-local"
  rm_tree_safe "$STATE_DIR/aiondb-data"
  rm_safe "$STATE_DIR/pg-local.log"
  rm_safe "$STATE_DIR/aiondb.log"
  rm_tree_safe "$DATA_DIR_BASE"
  rm_tree_safe "$TOOLS_DIR_BASE"
fi

final_bytes="$(managed_state_size_bytes)"
freed=$(( initial_bytes - final_bytes ))
log "final .state size: $(( final_bytes / 1024 / 1024 )) MB"
log "freed: $(( freed / 1024 / 1024 )) MB"
