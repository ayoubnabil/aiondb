#!/usr/bin/env bash
# Run pg-regress with a live progress bar
set -euo pipefail

TOTAL_FILES=218
DONE=0
PASSED=0
TOTAL_STMTS=0
MATCHED_STMTS=0
SKIPPED=0
CURRENT=""
BAR_WIDTH=50
LOG_FILE="/tmp/pg_regress_full_output.txt"
RESULTS_FILE="/tmp/pg_regress_results.txt"

> "$LOG_FILE"
> "$RESULTS_FILE"

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

draw_bar() {
    local pct=$1
    local filled=$(( pct * BAR_WIDTH / 100 ))
    local empty=$(( BAR_WIDTH - filled ))
    local bar=""
    for ((i=0; i<filled; i++)); do bar+="█"; done
    for ((i=0; i<empty; i++)); do bar+="░"; done
    echo -n "$bar"
}

render() {
    local pct=0
    if [ "$TOTAL_FILES" -gt 0 ]; then
        pct=$(( DONE * 100 / TOTAL_FILES ))
    fi
    local stmt_pct=0
    if [ "$TOTAL_STMTS" -gt 0 ]; then
        stmt_pct=$(( MATCHED_STMTS * 100 / TOTAL_STMTS ))
    fi

    # Clear screen area (6 lines)
    printf '\033[6A\033[J' 2>/dev/null || true

    echo ""
    printf "  ${BOLD}PostgreSQL Regression Tests - AionDB2${RESET}\n"
    echo ""
    printf "  ${CYAN}[$(draw_bar $pct)]${RESET}  ${BOLD}%3d%%${RESET}  (%d/%d fichiers)\n" "$pct" "$DONE" "$TOTAL_FILES"
    printf "  ${DIM}En cours: %-40s${RESET}  ${GREEN}Pass: %d${RESET} | ${YELLOW}Skip: %d${RESET} | ${BOLD}Stmts: %d/%d (%d%%)${RESET}\n" \
        "${CURRENT:-(attente)}" "$PASSED" "$SKIPPED" "$MATCHED_STMTS" "$TOTAL_STMTS" "$stmt_pct"
    echo ""
}

# Print initial blank lines for the render area
echo ""; echo ""; echo ""; echo ""; echo ""; echo ""

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Run pg-regress, capture stderr (progress) and stdout (final report)
./target/release/pg-regress 2>&1 | while IFS= read -r line; do
    # Log everything
    echo "$line" >> "$LOG_FILE"

    if [[ "$line" == FILE\|* ]]; then
        CURRENT="${line#FILE|}"
        render
    elif [[ "$line" == SKIP\|* ]]; then
        SKIPPED=$((SKIPPED + 1))
        DONE=$((DONE + 1))
        name=$(echo "$line" | cut -d'|' -f2)
        echo "$line" >> "$RESULTS_FILE"
        render
    elif [[ "$line" == RESULT\|* ]]; then
        DONE=$((DONE + 1))
        name=$(echo "$line" | cut -d'|' -f2)
        matched=$(echo "$line" | cut -d'|' -f3)
        total=$(echo "$line" | cut -d'|' -f4)
        MATCHED_STMTS=$((MATCHED_STMTS + matched))
        TOTAL_STMTS=$((TOTAL_STMTS + total))
        if [ "$matched" -eq "$total" ] && [ "$total" -gt 0 ]; then
            PASSED=$((PASSED + 1))
        fi
        echo "$line" >> "$RESULTS_FILE"
        render
    fi
done

# Final render
echo ""
printf "  ${BOLD}${GREEN}Termine !${RESET}\n"
echo ""
printf "  Fichiers: %d/%d  |  " "$DONE" "$TOTAL_FILES"
printf "Pass 100%%: ${GREEN}%d${RESET}  |  " "$PASSED"
printf "Skip: ${YELLOW}%d${RESET}\n" "$SKIPPED"
if [ "$TOTAL_STMTS" -gt 0 ]; then
    stmt_pct=$((MATCHED_STMTS * 100 / TOTAL_STMTS))
    printf "  Statements: ${BOLD}%d/%d (%d%%)${RESET}\n" "$MATCHED_STMTS" "$TOTAL_STMTS" "$stmt_pct"
fi
echo ""
printf "  ${DIM}Log complet: $LOG_FILE${RESET}\n"
printf "  ${DIM}Resultats:   $RESULTS_FILE${RESET}\n"
echo ""

# Show the final report from stdout if captured
if [ -f "$LOG_FILE" ]; then
    # Extract the summary table from the log
    grep -A 500 "^=\+$\|^FILE " "$LOG_FILE" 2>/dev/null | head -80 || true
fi
