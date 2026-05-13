#!/usr/bin/env bash
# benchmarks/tpcds/setup.sh — fetch tpcds-kit, build dsdgen + dsqgen, generate
# TPC-DS SF=$TPCDS_SCALE data.
#
# tpcds-kit is the community fork of the TPC-DS tools with build fixes for
# modern compilers. SF=1 produces ~1 GB, SF=10 produces ~10 GB. Keep SF low
# unless you know what you're doing.

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd git
require_cmd make
require_cmd gcc
if ! command -v yacc >/dev/null 2>&1 && ! command -v bison >/dev/null 2>&1; then
    die "missing parser generator: install 'bison' (or 'yacc') to build tpcds-kit tools"
fi

TPCDS_SCALE="${TPCDS_SCALE:-1}"
TPCDS_REPO_URL="${TPCDS_REPO_URL:-https://github.com/gregrahn/tpcds-kit.git}"
TPCDS_TOOLS_DIR="$TOOLS_DIR_BASE/tpcds-kit"
TPCDS_DATA_DIR="$DATA_DIR_BASE/tpcds-sf${TPCDS_SCALE}"
TPCDS_QUERIES_DIR="$DATA_DIR_BASE/tpcds-queries"

if [[ ! -d "$TPCDS_TOOLS_DIR/.git" ]]; then
    log "cloning tpcds-kit into $TPCDS_TOOLS_DIR"
    git clone --depth 1 "$TPCDS_REPO_URL" "$TPCDS_TOOLS_DIR"
fi

TOOL_SRC="$TPCDS_TOOLS_DIR/tools"
[[ -d "$TOOL_SRC" ]] || die "tpcds-kit layout unexpected — missing $TOOL_SRC"

if [[ ! -x "$TOOL_SRC/dsdgen" || ! -x "$TOOL_SRC/dsqgen" ]]; then
    log "building dsdgen / dsqgen"
    # Upstream tpcds-kit has duplicate common symbols across translation units
    # on newer toolchains. Keep upstream compile flags and relax linker checks.
    (cd "$TOOL_SRC" && make -s clean >/dev/null 2>&1 || true)
    (cd "$TOOL_SRC" && make -s OS=LINUX CC=gcc LDFLAGS='-Wl,--allow-multiple-definition') >&2
fi
[[ -x "$TOOL_SRC/dsdgen" ]] || die "dsdgen build failed"
[[ -x "$TOOL_SRC/dsqgen" ]] || die "dsqgen build failed"

if [[ ! -f "$TPCDS_DATA_DIR/store_sales.dat" ]]; then
    log "generating TPC-DS data SF=$TPCDS_SCALE into $TPCDS_DATA_DIR"
    mkdir -p "$TPCDS_DATA_DIR"
    (cd "$TOOL_SRC" && ./dsdgen -DIR "$TPCDS_DATA_DIR" -SCALE "$TPCDS_SCALE" -TERMINATE N -FORCE) >&2
fi

if [[ ! -f "$TPCDS_QUERIES_DIR/query1.sql" ]]; then
    log "materializing TPC-DS queries via dsqgen into $TPCDS_QUERIES_DIR"
    mkdir -p "$TPCDS_QUERIES_DIR"
    # Generate all 99 standard queries in one batch (DIALECT=netezza is closest
    # to generic PostgreSQL-style SQL).
    (cd "$TOOL_SRC" \
        && ./dsqgen -DIRECTORY "$TOOL_SRC/../query_templates" \
                    -INPUT "$TOOL_SRC/../query_templates/templates.lst" \
                    -VERBOSE Y -QUALIFY Y -SCALE "$TPCDS_SCALE" \
                    -DIALECT netezza -OUTPUT_DIR "$TPCDS_QUERIES_DIR") >&2 \
    || warn "dsqgen produced partial output — check $TPCDS_QUERIES_DIR"
fi

cat <<EOF
TPCDS_TOOLS_DIR=$TPCDS_TOOLS_DIR
TPCDS_DATA_DIR=$TPCDS_DATA_DIR
TPCDS_SCHEMA_FILE=$TPCDS_TOOLS_DIR/tools/tpcds.sql
TPCDS_QUERIES_DIR=$TPCDS_QUERIES_DIR
EOF
