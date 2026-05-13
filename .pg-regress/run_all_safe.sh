#!/usr/bin/env bash
# Run each pg-regress test file individually so crashes are isolated,
# reported, and treated as hard failures.
set -uo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RESULTS_FILE="/tmp/pg_regress_all_results.txt"
LOG_FILE="/tmp/pg_regress_all_log.txt"
> "$RESULTS_FILE"
> "$LOG_FILE"

# Get all sql files sorted
INCLUDE_LOCAL_PROBES_RAW="${PG_REGRESS_INCLUDE_LOCAL_PROBES:-0}"
INCLUDE_LOCAL_PROBES=0
case "${INCLUDE_LOCAL_PROBES_RAW,,}" in
    1|true|yes|on) INCLUDE_LOCAL_PROBES=1 ;;
esac

ALLOW_CRASHES_RAW="${PG_REGRESS_ALLOW_CRASHES:-0}"
ALLOW_CRASHES=0
case "${ALLOW_CRASHES_RAW,,}" in
    1|true|yes|on) ALLOW_CRASHES=1 ;;
esac

trim() {
    local s="$1"
    s="${s#"${s%%[![:space:]]*}"}"
    s="${s%"${s##*[![:space:]]}"}"
    printf '%s' "$s"
}

parse_csv() {
    local raw="$1"
    local -n out_ref="$2"
    local item
    out_ref=()
    IFS=',' read -r -a raw_items <<< "$raw"
    for item in "${raw_items[@]}"; do
        item="$(trim "$item")"
        [ -n "$item" ] && out_ref+=("$item")
    done
}

is_hard_excluded() {
    case "$1" in
        test_setup|infinite_recurse|lock) return 0 ;;
        *) return 1 ;;
    esac
}

LOCAL_PROBE_SUITES=(
    with_probe
    tsdicts_probe
    probe_join_lateral_values
    probe_join_lateral_values_debug
    probe_join_lateral_values_two_rows
    probe_join_prepare_foo_true
    probe_join_left_three_keys_count
)

declare -A KNOWN_SUITES=()
ALL_SUITES=()
for f in sql/*.sql; do
    [ -e "$f" ] || continue
    name=$(basename "$f" .sql)
    ALL_SUITES+=("$name")
    KNOWN_SUITES["$name"]=1
done

parse_csv "${PG_REGRESS_EXCLUDE_SUITES:-}" EXTRA_EXCLUDES
parse_csv "${PG_REGRESS_INCLUDE_SUITES:-}" EXTRA_INCLUDES

declare -A EXCLUDED_SET=()
for name in test_setup infinite_recurse lock; do
    EXCLUDED_SET["$name"]=1
done

if [ "$INCLUDE_LOCAL_PROBES" -eq 0 ]; then
    for name in "${LOCAL_PROBE_SUITES[@]}"; do
        if [ -n "${KNOWN_SUITES[$name]+x}" ]; then
            EXCLUDED_SET["$name"]=1
        fi
    done
fi

for name in "${EXTRA_EXCLUDES[@]}"; do
    if [ -z "${KNOWN_SUITES[$name]+x}" ]; then
        echo "WARN|config|PG_REGRESS_EXCLUDE_SUITES references unknown suite '$name'" >&2
        continue
    fi
    EXCLUDED_SET["$name"]=1
done

for name in "${EXTRA_INCLUDES[@]}"; do
    if [ -z "${KNOWN_SUITES[$name]+x}" ]; then
        echo "WARN|config|PG_REGRESS_INCLUDE_SUITES references unknown suite '$name'" >&2
        continue
    fi
    if is_hard_excluded "$name"; then
        echo "WARN|config|suite '$name' is hard-excluded and cannot be re-included" >&2
        continue
    fi
    unset "EXCLUDED_SET[$name]"
done

FILES=()
EXCLUDED_NAMES=()
for name in "${ALL_SUITES[@]}"; do
    if [ -n "${EXCLUDED_SET[$name]+x}" ]; then
        EXCLUDED_NAMES+=("$name")
    else
        FILES+=("$name")
    fi
done

TOTAL=${#FILES[@]}
DONE=0
PASSED_100=0
CRASHED=0
TOTAL_MATCHED=0
TOTAL_STMTS=0
SKIPPED=0

echo "=== AionDB2 PostgreSQL Regression Tests ==="
echo "Total fichiers: $TOTAL"
if [ "$INCLUDE_LOCAL_PROBES" -eq 1 ]; then
    echo "Mode suites: all (local probes inclus)"
else
    echo "Mode suites: official (local probes exclus)"
fi
echo "Config excludes (PG_REGRESS_EXCLUDE_SUITES): ${PG_REGRESS_EXCLUDE_SUITES:-<none>}"
echo "Config includes (PG_REGRESS_INCLUDE_SUITES): ${PG_REGRESS_INCLUDE_SUITES:-<none>}"
echo "Config allow crashes (PG_REGRESS_ALLOW_CRASHES): $ALLOW_CRASHES_RAW"
echo "Config exclusions appliquees: ${#EXCLUDED_NAMES[@]}"
echo "Debut: $(date '+%H:%M:%S')"
echo "==========================================="
echo ""

if [ "$TOTAL" -eq 0 ]; then
    echo "Aucun fichier a executer apres exclusions de config."
    echo "Log complet: $LOG_FILE"
    echo "Resultats:   $RESULTS_FILE"
    exit 1
fi

for name in "${FILES[@]}"; do
    DONE=$((DONE + 1))
    PCT=$((DONE * 100 / TOTAL))

    # Progress line
    FILLED=$((PCT / 2))
    BAR=""
    for ((i=0; i<FILLED; i++)); do BAR+="#"; done
    for ((i=FILLED; i<50; i++)); do BAR+="."; done

    printf "\r  [%s] %3d%% (%d/%d) %-40s" "$BAR" "$PCT" "$DONE" "$TOTAL" "$name"

    # Run single file with timeout
    OUTPUT=$(timeout 120 env PG_REGRESS_FILE="$name" ./target/release/pg-regress 2>&1) || true

    echo "$OUTPUT" >> "$LOG_FILE"

    # Parse RESULT line
    RESULT_LINE=$(echo "$OUTPUT" | grep "^RESULT|" | head -1)
    SKIP_LINE=$(echo "$OUTPUT" | grep "^SKIP|" | head -1)

    if [ -n "$SKIP_LINE" ]; then
        SKIPPED=$((SKIPPED + 1))
        echo "$SKIP_LINE" >> "$RESULTS_FILE"
    elif [ -n "$RESULT_LINE" ]; then
        matched=$(echo "$RESULT_LINE" | cut -d'|' -f3)
        total=$(echo "$RESULT_LINE" | cut -d'|' -f4)
        TOTAL_MATCHED=$((TOTAL_MATCHED + matched))
        TOTAL_STMTS=$((TOTAL_STMTS + total))
        if [ "$matched" -eq "$total" ] && [ "$total" -gt 0 ]; then
            PASSED_100=$((PASSED_100 + 1))
        fi
        echo "$RESULT_LINE" >> "$RESULTS_FILE"
    else
        # Crash or timeout
        CRASHED=$((CRASHED + 1))
        echo "CRASH|$name|0|0" >> "$RESULTS_FILE"
    fi
done

echo ""
echo ""
echo "==========================================="
echo "=== RESULTATS FINAUX ==="
echo "==========================================="
echo ""
echo "  Fichiers testes:   $DONE / $TOTAL"
echo "  Pass 100%:         $PASSED_100"
echo "  Skip:              $SKIPPED"
echo "  Crash/Timeout:     $CRASHED"
echo ""
if [ "$TOTAL_STMTS" -gt 0 ]; then
    STMT_PCT=$((TOTAL_MATCHED * 100 / TOTAL_STMTS))
    echo "  Statements match:  $TOTAL_MATCHED / $TOTAL_STMTS ($STMT_PCT%)"
fi
echo ""
echo "  Fin: $(date '+%H:%M:%S')"
echo ""

# Top 20 best
echo "=== TOP 20 MEILLEURS ==="
grep "^RESULT|" "$RESULTS_FILE" | while IFS='|' read -r _ name matched total; do
    if [ "$total" -gt 0 ]; then
        pct=$((matched * 100 / total))
        printf "%3d%%  %4d/%4d  %s\n" "$pct" "$matched" "$total" "$name"
    fi
done | sort -rn | head -20
echo ""

# Top 20 worst
echo "=== TOP 20 PIRES ==="
grep "^RESULT|" "$RESULTS_FILE" | while IFS='|' read -r _ name matched total; do
    if [ "$total" -gt 0 ]; then
        pct=$((matched * 100 / total))
        printf "%3d%%  %4d/%4d  %s\n" "$pct" "$matched" "$total" "$name"
    fi
done | sort -n | head -20
echo ""

# Crashes
if [ "$CRASHED" -gt 0 ]; then
    echo "=== CRASHES/TIMEOUTS ==="
    grep "^CRASH|" "$RESULTS_FILE" | cut -d'|' -f2
    echo ""
fi

echo "Log complet: $LOG_FILE"
echo "Resultats:   $RESULTS_FILE"

if [ "$CRASHED" -gt 0 ] && [ "$ALLOW_CRASHES" -eq 0 ]; then
    echo "Echec: $CRASHED suite(s) ont crashe ou depasse le timeout."
    echo "Pour une exploration non bloquante uniquement: PG_REGRESS_ALLOW_CRASHES=1 $0"
    exit 1
fi
