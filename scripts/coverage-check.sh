#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────
# AionDB — Coverage Threshold Checker
#
# Parses an LCOV file and enforces coverage thresholds:
# - per-crate overrides in CRATE_THRESHOLDS
# - a default global threshold for crates not explicitly listed
# Returns exit code 1 if any crate is below its effective threshold.
#
# Usage:
#   ./scripts/coverage-check.sh                         # use defaults
#   ./scripts/coverage-check.sh --lcov path/to/lcov.info
#   ./scripts/coverage-check.sh --threshold 60          # global min %
#   ./scripts/coverage-check.sh --threshold 0           # disable global gate (overrides still apply)
#
# Prerequisites:
#   An LCOV file produced by:  ./scripts/coverage.sh --lcov
# ─────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Defaults ────────────────────────────────────────────────────────
LCOV_FILE="$PROJECT_ROOT/coverage/lcov.info"
GLOBAL_THRESHOLD=25  # default global gate for crates without explicit override

# Per-crate minimum thresholds (line coverage %).
# Crates listed here override the global threshold.
declare -A CRATE_THRESHOLDS=(
  ["aiondb-engine"]=55
  ["aiondb-storage-engine"]=45
  ["aiondb-executor"]=45
  ["aiondb-planner"]=35
  ["aiondb-optimizer"]=35
  ["aiondb-eval"]=35
  ["aiondb-plan"]=35
  ["aiondb-pg-syntax"]=16
  ["aiondb-pgwire"]=25
  ["other"]=20
)

# ── Parse arguments ─────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --lcov)
      LCOV_FILE="$2"
      shift 2
      ;;
    --threshold)
      GLOBAL_THRESHOLD="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '3,15p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

# ── Validate ────────────────────────────────────────────────────────
if [[ ! -f "$LCOV_FILE" ]]; then
  echo "ERROR: LCOV file not found: $LCOV_FILE" >&2
  echo "" >&2
  echo "Generate it first with:" >&2
  echo "  ./scripts/coverage.sh --lcov" >&2
  exit 1
fi

# ── Parse LCOV into per-crate stats ────────────────────────────────
# Produce tab-separated: crate\tlines\thit\tpct
CRATE_STATS=$(awk '
  /^SF:/ {
    file = $0
    sub(/^SF:/, "", file)
    if (match(file, /crates\/([^/]+)\//, m)) {
      crate = m[1]
    } else {
      crate = "other"
    }
  }
  /^LF:/ { lf = $0; sub(/^LF:/, "", lf); lines[crate] += lf + 0 }
  /^LH:/ { lh = $0; sub(/^LH:/, "", lh); hit[crate] += lh + 0 }
  END {
    n = asorti(lines, sorted)
    for (i = 1; i <= n; i++) {
      c = sorted[i]
      if (lines[c] > 0) {
        pct = (hit[c] / lines[c]) * 100
      } else {
        pct = 0
      }
      printf "%s\t%d\t%d\t%.1f\n", c, lines[c], hit[c], pct
    }
  }
' "$LCOV_FILE")

# ── Display table ──────────────────────────────────────────────────
echo "================================================"
echo "  AionDB Coverage Threshold Check"
echo "================================================"
echo ""
printf "  %-35s %8s %8s %9s %9s %s\n" \
  "CRATE" "LINES" "HIT" "COVERAGE" "REQUIRED" "STATUS"
printf "  %-35s %8s %8s %9s %9s %s\n" \
  "───────────────────────────────────" "────────" "────────" "─────────" "─────────" "──────"

FAILURES=0
TOTAL_LINES=0
TOTAL_HIT=0

while IFS=$'\t' read -r crate lines hit pct; do
  TOTAL_LINES=$((TOTAL_LINES + lines))
  TOTAL_HIT=$((TOTAL_HIT + hit))

  # Determine required threshold for this crate
  required="${CRATE_THRESHOLDS[$crate]:-$GLOBAL_THRESHOLD}"

  # Check threshold
  if [[ "$required" -gt 0 ]]; then
    # Compare using integer arithmetic (pct is like "42.3")
    pct_int="${pct%%.*}"
    if [[ "$pct_int" -lt "$required" ]]; then
      status="FAIL"
      FAILURES=$((FAILURES + 1))
    else
      status="OK"
    fi
    req_str="${required}%"
  else
    status="-"
    req_str="-"
  fi

  printf "  %-35s %8d %8d %8.1f%% %8s  %s\n" \
    "$crate" "$lines" "$hit" "$pct" "$req_str" "$status"

done <<< "$CRATE_STATS"

# ── Totals ─────────────────────────────────────────────────────────
if [[ "$TOTAL_LINES" -gt 0 ]]; then
  TOTAL_PCT=$(awk "BEGIN { printf \"%.1f\", ($TOTAL_HIT / $TOTAL_LINES) * 100 }")
else
  TOTAL_PCT="0.0"
fi

printf "  %-35s %8s %8s %9s %9s %s\n" \
  "───────────────────────────────────" "────────" "────────" "─────────" "─────────" "──────"
printf "  %-35s %8d %8d %8s%%\n" \
  "TOTAL" "$TOTAL_LINES" "$TOTAL_HIT" "$TOTAL_PCT"

echo ""

# ── Verdict ────────────────────────────────────────────────────────
if [[ "$FAILURES" -gt 0 ]]; then
  echo "RESULT: FAILED — $FAILURES crate(s) below threshold."
  echo ""
  echo "To adjust thresholds, edit GLOBAL_THRESHOLD and CRATE_THRESHOLDS in:"
  echo "  $0"
  exit 1
else
  echo "RESULT: PASSED — all crates meet coverage thresholds."
  exit 0
fi
