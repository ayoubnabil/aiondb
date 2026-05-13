#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

MANIFEST_PATH="$PROJECT_ROOT/.cypher-tck/Cargo.toml"
FEATURE_BASE="testing/neo4j-compat/features"
MODE="smoke"
declare -a FILE_FILTERS=()

usage() {
  cat <<'EOF'
Usage: bash ./scripts/neo4j-compat-check.sh [OPTIONS]

Run the in-repo Neo4j compatibility feature suite.

Options:
  --mode <smoke|full>       Execution profile (default: smoke)
  --file <FILTER>           Additional --file filter (repeatable, smoke mode)
  -h, --help                Show this help

Defaults:
  smoke mode file filters: DbMetadata1, PathAndGraphFunctions1, OptionalMatch1, Collections1
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      MODE="$2"
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

if [[ "$MODE" != "smoke" && "$MODE" != "full" ]]; then
  echo "ERROR: unsupported mode '$MODE' (expected smoke|full)" >&2
  exit 1
fi

if [[ "$MODE" == "smoke" && ${#FILE_FILTERS[@]} -eq 0 ]]; then
  FILE_FILTERS=("DbMetadata1" "PathAndGraphFunctions1" "OptionalMatch1" "Collections1")
fi

cd "$PROJECT_ROOT"
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"

run_case() {
  local label="$1"
  shift
  echo "=== Neo4j compat: $label ==="
  "$@"
}

if [[ "$MODE" == "full" ]]; then
  run_case \
    "full" \
    cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin cypher-tck -- --base-dir "$FEATURE_BASE"
else
  for file_filter in "${FILE_FILTERS[@]}"; do
    run_case \
      "file=$file_filter" \
      cargo run --quiet --manifest-path "$MANIFEST_PATH" --bin cypher-tck -- --base-dir "$FEATURE_BASE" --file "$file_filter"
  done
fi
