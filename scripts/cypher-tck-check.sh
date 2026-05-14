#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

MANIFEST_PATH="$PROJECT_ROOT/.cypher-tck/Cargo.toml"
MODE="smoke"
MIN_PASS_RATE=""
declare -a FILE_FILTERS=()

usage() {
  cat <<'EOF'
Usage: ./scripts/cypher-tck-check.sh [OPTIONS]

Run Cypher TCK in CI and enforce a minimum pass-rate gate.

Options:
  --mode <smoke|full>         Execution profile (default: smoke)
  --min-pass-rate <float>     Minimum required pass-rate percentage
  --manifest-path <PATH>      Path to .cypher-tck Cargo.toml
  --file <FILTER>             Additional --file filter (repeatable, smoke mode)
  -h, --help                  Show this help

Defaults:
  smoke mode: minimum pass-rate = 8.0
  full mode:  minimum pass-rate = 12.0
  smoke file filters (when none are provided): Literals1, Null1, Comparison1, List1
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="$2"
      shift 2
      ;;
    --min-pass-rate)
      MIN_PASS_RATE="$2"
      shift 2
      ;;
    --manifest-path)
      MANIFEST_PATH="$2"
      shift 2
      ;;
    --file)
      FILE_FILTERS+=("$2")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

case "$MODE" in
  smoke|full) ;;
  *)
    echo "ERROR: unsupported mode '$MODE' (expected smoke|full)" >&2
    exit 1
    ;;
esac

if [[ -z "$MIN_PASS_RATE" ]]; then
  if [[ "$MODE" == "full" ]]; then
    MIN_PASS_RATE="12.0"
  else
    MIN_PASS_RATE="8.0"
  fi
fi

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "ERROR: missing manifest at $MANIFEST_PATH" >&2
  exit 1
fi

if [[ "$MODE" == "smoke" && ${#FILE_FILTERS[@]} -eq 0 ]]; then
  FILE_FILTERS=("Literals1" "Null1" "Comparison1" "List1")
fi

cd "$PROJECT_ROOT"
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"

TOTAL_PASS=0
TOTAL_SCENARIOS=0
RUN_COUNT=0

parse_pass_total_from_file() {
  local output_file="$1"
  python3 - "$output_file" <<'PY'
import pathlib
import re
import sys

output_path = pathlib.Path(sys.argv[1])
text = output_path.read_text(encoding="utf-8", errors="replace")
match = re.search(r"Pass rate:\s*([0-9]+(?:\.[0-9]+)?)%\s*\((\d+)\s*/\s*(\d+)\)", text)
if not match:
    print("ERROR: could not parse 'Pass rate' line from Cypher TCK output", file=sys.stderr)
    sys.exit(1)
print(match.group(2), match.group(3))
PY
}

run_tck_case() {
  local label="$1"
  shift

  local tmp_output
  tmp_output="$(mktemp)"
  trap 'rm -f "$tmp_output"' RETURN

  echo "=== Cypher TCK: $label ==="

  set +e
  "$@" 2>&1 | tee "$tmp_output"
  local cmd_status=${PIPESTATUS[0]}
  set -e

  if [[ "$cmd_status" -ne 0 ]]; then
    echo "ERROR: Cypher TCK command failed for '$label' (exit $cmd_status)" >&2
    exit "$cmd_status"
  fi

  read -r pass total < <(parse_pass_total_from_file "$tmp_output")
  TOTAL_PASS=$((TOTAL_PASS + pass))
  TOTAL_SCENARIOS=$((TOTAL_SCENARIOS + total))
  RUN_COUNT=$((RUN_COUNT + 1))

  echo "Parsed pass-rate for '$label': $pass/$total"
  rm -f "$tmp_output"
  trap - RETURN
}

if [[ "$MODE" == "full" ]]; then
  run_tck_case \
    "full" \
    cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin cypher-tck --
else
  for file_filter in "${FILE_FILTERS[@]}"; do
    run_tck_case \
      "file=$file_filter" \
      cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin cypher-tck -- --file "$file_filter"
  done
fi

if [[ "$TOTAL_SCENARIOS" -le 0 ]]; then
  echo "ERROR: Cypher TCK produced zero scenarios; refusing to pass gate." >&2
  exit 1
fi

OBSERVED_RATE=$(python3 - <<PY
total_pass = $TOTAL_PASS
total = $TOTAL_SCENARIOS
print(f"{(total_pass * 100.0 / total):.2f}")
PY
)

echo "=== Cypher TCK aggregate ==="
echo "runs: $RUN_COUNT"
echo "pass: $TOTAL_PASS"
echo "total: $TOTAL_SCENARIOS"
echo "pass-rate: $OBSERVED_RATE%"
echo "required: >= $MIN_PASS_RATE%"

python3 - "$OBSERVED_RATE" "$MIN_PASS_RATE" <<'PY'
import sys

observed = float(sys.argv[1])
required = float(sys.argv[2])
if observed + 1e-9 < required:
    print(
        f"ERROR: Cypher TCK gate failed (observed {observed:.2f}% < required {required:.2f}%)",
        file=sys.stderr,
    )
    sys.exit(1)
PY

echo "Cypher TCK gate passed."
