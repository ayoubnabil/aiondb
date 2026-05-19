#!/usr/bin/env bash
# benchmarks/ultra-compare/run.sh — composite long-run tri-engine orchestrator.

set -euo pipefail

BENCH_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

exec python3 "$BENCH_ROOT/ultra-compare/run.py" "$@"
