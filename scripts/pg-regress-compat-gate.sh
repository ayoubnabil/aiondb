#!/usr/bin/env bash
set -euo pipefail

SUMMARY_PATH=".pg-regress/reports/compat-summary.json"
POLICY_PATH=".pg-regress/policy/compat-p1-gate.json"
MODE="pr"

usage() {
  cat <<'EOF'
Usage: ./scripts/pg-regress-compat-gate.sh [OPTIONS]

Enforce governance thresholds for priority P1 pg-regress suites.

Options:
  --summary <PATH>   Path to compat summary json
  --policy <PATH>    Path to P1 governance policy json
  --mode <pr|main>   Gate profile (default: pr)
  -h, --help         Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --summary)
      SUMMARY_PATH="$2"
      shift 2
      ;;
    --policy)
      POLICY_PATH="$2"
      shift 2
      ;;
    --mode)
      MODE="$2"
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

if [[ "$MODE" != "pr" && "$MODE" != "main" ]]; then
  echo "ERROR: --mode must be 'pr' or 'main'" >&2
  exit 1
fi

if [[ ! -f "$SUMMARY_PATH" ]]; then
  echo "ERROR: summary file not found: $SUMMARY_PATH" >&2
  exit 1
fi

if [[ ! -f "$POLICY_PATH" ]]; then
  echo "ERROR: policy file not found: $POLICY_PATH" >&2
  exit 1
fi

python3 - "$SUMMARY_PATH" "$POLICY_PATH" "$MODE" <<'PY'
import json
import sys
from pathlib import Path

summary_path = Path(sys.argv[1])
policy_path = Path(sys.argv[2])
mode = sys.argv[3]

summary = json.loads(summary_path.read_text(encoding="utf-8"))
policy = json.loads(policy_path.read_text(encoding="utf-8"))

overall_rate = float(summary.get("overall_rate", 0.0))
crash_count = int(summary.get("crash_count", 0))
low_rate_suites = summary.get("low_rate_suites", {})

missing_suite_lower_bound = float(policy.get("missing_suite_lower_bound_rate", 70.0))
max_drop_pp = float(policy["max_drop_pp"][mode])
overall_min = float(policy["overall_rate_min"][mode])

print("====================================================")
print(f"  pg_regress P1 governance gate (mode={mode})")
print("====================================================")
print(f"summary: {summary_path}")
print(f"policy : {policy_path}")
print("")

failures = []
warnings = []

p0_policy = policy.get("p0", {})
if not bool(p0_policy.get("enforce", False)):
    warn_limit = int(p0_policy.get("warn_if_crash_count_above", 0))
    if crash_count > warn_limit:
        warnings.append(
            f"P0 warning: crash_count={crash_count} exceeds warning limit {warn_limit}"
        )

print(f"overall_rate={overall_rate:.1f}% (required >= {overall_min:.1f}%)")
if overall_rate + 1e-9 < overall_min:
    failures.append(
        f"overall_rate {overall_rate:.1f}% is below required {overall_min:.1f}%"
    )

print("")
print("Suite                Obs%   Floor(PR) Target(main) Baseline MaxDrop  Status")
print("--------------------------------------------------------------------------")

for suite, cfg in policy["suites"].items():
    baseline = float(cfg["baseline_rate"])
    floor_pr = float(cfg["pr_floor"])
    target_main = float(cfg["main_target"])
    required = target_main if mode == "main" else floor_pr

    if suite in low_rate_suites:
        observed = float(low_rate_suites[suite]["rate"])
    else:
        observed = missing_suite_lower_bound

    status = "OK"
    if observed + 1e-9 < required:
        status = "FAIL:req"
        failures.append(
            f"{suite}: observed {observed:.1f}% < required {required:.1f}% (mode={mode})"
        )

    if observed + max_drop_pp + 1e-9 < baseline:
        status = "FAIL:reg"
        failures.append(
            f"{suite}: observed {observed:.1f}% regressed by more than {max_drop_pp:.1f}pp from baseline {baseline:.1f}%"
        )

    print(
        f"{suite:20} {observed:5.1f}   {floor_pr:7.1f}    {target_main:8.1f} {baseline:8.1f} {max_drop_pp:7.1f}  {status}"
    )

if warnings:
    print("")
    for warning in warnings:
        print(f"WARNING: {warning}")

if failures:
    print("")
    print("ERROR: P1 governance gate failed:")
    for failure in failures:
        print(f" - {failure}")
    sys.exit(1)

print("")
print("Gate PASSED.")
PY
