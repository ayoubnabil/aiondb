#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────
# AionDB — Code Coverage Report Generator
#
# Uses cargo-llvm-cov to instrument the workspace test suite and
# produce coverage reports (HTML, LCOV, or both).
#
# Usage:
#   ./scripts/coverage.sh              # HTML + LCOV + summary
#   ./scripts/coverage.sh --html       # HTML report only
#   ./scripts/coverage.sh --lcov       # LCOV file only
#   ./scripts/coverage.sh --open       # HTML report + open in browser
#   ./scripts/coverage.sh --json       # JSON export (machine-readable)
#
# Prerequisites:
#   cargo install cargo-llvm-cov
# ─────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COVERAGE_DIR="$PROJECT_ROOT/coverage"

# ── Defaults ────────────────────────────────────────────────────────
WANT_HTML=false
WANT_LCOV=false
WANT_OPEN=false
WANT_JSON=false
WANT_ALL=true   # when no flag is given, produce HTML + LCOV + summary

# ── Parse arguments ─────────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --html) WANT_HTML=true; WANT_ALL=false ;;
    --lcov) WANT_LCOV=true; WANT_ALL=false ;;
    --open) WANT_OPEN=true; WANT_HTML=true; WANT_ALL=false ;;
    --json) WANT_JSON=true; WANT_ALL=false ;;
    -h|--help)
      sed -n '3,14p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      echo "Usage: $0 [--html] [--lcov] [--open] [--json]" >&2
      exit 1
      ;;
  esac
done

if $WANT_ALL; then
  WANT_HTML=true
  WANT_LCOV=true
fi

# ── Pre-flight checks ──────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
  echo "ERROR: cargo is not installed or not in PATH." >&2
  exit 1
fi

if ! cargo llvm-cov --help &>/dev/null; then
  echo "ERROR: cargo-llvm-cov is not installed." >&2
  echo "Install it with:  cargo install cargo-llvm-cov" >&2
  exit 1
fi

# ── Prepare output directory ───────────────────────────────────────
mkdir -p "$COVERAGE_DIR"

cd "$PROJECT_ROOT"

echo "================================================"
echo "  AionDB Code Coverage"
echo "================================================"
echo ""

# ── Clean previous instrumentation data ────────────────────────────
echo "[1/4] Cleaning previous coverage data..."
cargo llvm-cov clean --workspace 2>/dev/null || true
echo ""

# ── Run tests with coverage instrumentation ────────────────────────
echo "[2/4] Running tests with coverage instrumentation..."
echo "      (this may take a while)"
echo ""

# Generate HTML report
if $WANT_HTML; then
  echo "  -> Generating HTML report..."
  cargo llvm-cov --workspace \
    --html \
    --output-dir "$COVERAGE_DIR" \
    2>&1
  echo "     HTML report: $COVERAGE_DIR/html/index.html"
  echo ""
fi

# Generate LCOV file
if $WANT_LCOV; then
  echo "  -> Generating LCOV file..."
  cargo llvm-cov --workspace \
    --lcov \
    --output-path "$COVERAGE_DIR/lcov.info" \
    2>&1
  echo "     LCOV file:   $COVERAGE_DIR/lcov.info"
  echo ""
fi

# Generate JSON export
if $WANT_JSON; then
  echo "  -> Generating JSON export..."
  cargo llvm-cov --workspace \
    --json \
    --output-path "$COVERAGE_DIR/coverage.json" \
    2>&1
  echo "     JSON file:   $COVERAGE_DIR/coverage.json"
  echo ""
fi

# ── Print summary ──────────────────────────────────────────────────
echo "[3/4] Generating summary..."
echo ""

# Text summary to stdout and file
cargo llvm-cov --workspace --no-run 2>/dev/null \
  | tee "$COVERAGE_DIR/summary.txt" \
  || {
    # If --no-run fails (no previous run data), generate fresh
    cargo llvm-cov --workspace 2>&1 \
      | tee "$COVERAGE_DIR/summary.txt"
  }

echo ""
echo "================================================"
echo "  Coverage artifacts written to: $COVERAGE_DIR/"
echo "================================================"
echo ""

# ── Per-crate breakdown from LCOV ─────────────────────────────────
if [[ -f "$COVERAGE_DIR/lcov.info" ]]; then
  echo "[4/4] Per-crate coverage breakdown:"
  echo ""
  printf "  %-40s %8s %8s %8s\n" "CRATE" "LINES" "HIT" "COVERAGE"
  printf "  %-40s %8s %8s %8s\n" "────────────────────────────────────────" "────────" "────────" "────────"

  # Parse LCOV to extract per-crate stats
  awk '
    /^SF:/ {
      file = $0
      sub(/^SF:/, "", file)
      # Extract crate name from path like crates/<name>/src/...
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
      total_lines = 0
      total_hit = 0
      for (i = 1; i <= n; i++) {
        c = sorted[i]
        total_lines += lines[c]
        total_hit += hit[c]
        if (lines[c] > 0) {
          pct = (hit[c] / lines[c]) * 100
        } else {
          pct = 0
        }
        printf "  %-40s %8d %8d %7.1f%%\n", c, lines[c], hit[c], pct
      }
      printf "  %-40s %8s %8s %8s\n", "────────────────────────────────────────", "────────", "────────", "────────"
      if (total_lines > 0) {
        total_pct = (total_hit / total_lines) * 100
      } else {
        total_pct = 0
      }
      printf "  %-40s %8d %8d %7.1f%%\n", "TOTAL", total_lines, total_hit, total_pct
    }
  ' "$COVERAGE_DIR/lcov.info"

  echo ""
else
  echo "[4/4] Skipping per-crate breakdown (no lcov.info generated)."
  echo ""
fi

# ── Open in browser ────────────────────────────────────────────────
if $WANT_OPEN; then
  HTML_INDEX="$COVERAGE_DIR/html/index.html"
  if [[ -f "$HTML_INDEX" ]]; then
    echo "Opening coverage report in browser..."
    if command -v xdg-open &>/dev/null; then
      xdg-open "$HTML_INDEX"
    elif command -v open &>/dev/null; then
      open "$HTML_INDEX"
    else
      echo "Could not detect browser opener. Open manually:"
      echo "  $HTML_INDEX"
    fi
  fi
fi

echo "Done."
