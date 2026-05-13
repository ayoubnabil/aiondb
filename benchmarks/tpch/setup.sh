#!/usr/bin/env bash
# benchmarks/tpch/setup.sh — fetch tpch-kit and generate TPC-H data.
#
# Idempotent: skips each step if already done. Data goes to
# benchmarks/.data/tpch-sf${TPCH_SCALE}/.
#
# Required: git, make, gcc. The tpch-kit repo is a standard source for dbgen
# adapted for modern compilers — we use the gregrahn fork which has PG-ready
# queries under dbgen/queries.

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd git
require_cmd make
require_cmd gcc

TPCH_SCALE="${TPCH_SCALE:-1}"
TPCH_QGEN_SEED="${TPCH_QGEN_SEED:-1}"
TPCH_REPO_URL="${TPCH_REPO_URL:-https://github.com/gregrahn/tpch-kit.git}"
TPCH_TOOLS_DIR="$TOOLS_DIR_BASE/tpch-kit"
TPCH_DATA_DIR="$DATA_DIR_BASE/tpch-sf${TPCH_SCALE}"

if [[ ! -d "$TPCH_TOOLS_DIR/.git" ]]; then
    log "cloning tpch-kit into $TPCH_TOOLS_DIR"
    git clone --depth 1 "$TPCH_REPO_URL" "$TPCH_TOOLS_DIR"
fi

DBGEN_DIR="$TPCH_TOOLS_DIR/dbgen"
[[ -d "$DBGEN_DIR" ]] || die "tpch-kit layout unexpected — missing $DBGEN_DIR"

if [[ ! -x "$DBGEN_DIR/dbgen" ]]; then
    log "building dbgen (MACHINE=LINUX DATABASE=POSTGRESQL)"
    (cd "$DBGEN_DIR" && make -s MACHINE=LINUX DATABASE=POSTGRESQL CC=gcc) >&2
fi
[[ -x "$DBGEN_DIR/dbgen" ]] || die "dbgen build failed"

if [[ ! -f "$TPCH_DATA_DIR/lineitem.tbl" ]]; then
    log "generating TPC-H data SF=$TPCH_SCALE into $TPCH_DATA_DIR"
    mkdir -p "$TPCH_DATA_DIR"
    (cd "$DBGEN_DIR" && DSS_PATH="$TPCH_DATA_DIR" ./dbgen -vf -s "$TPCH_SCALE" -b "$DBGEN_DIR/dists.dss") >&2
fi

# tpch-kit ships with template queries at dbgen/queries/{1..22}.sql containing
# ':n' placeholders. `qgen` materializes them with default parameters. We
# stash the resulting queries under benchmarks/.data/tpch-queries/.
QUERIES_DIR="$DATA_DIR_BASE/tpch-queries"
if [[ ! -f "$QUERIES_DIR/1.sql" ]]; then
    log "materializing TPC-H queries via qgen into $QUERIES_DIR"
    mkdir -p "$QUERIES_DIR"
    for q in $(seq 1 22); do
        (cd "$DBGEN_DIR" && DSS_QUERY="$DBGEN_DIR/queries" DSS_PATH="$TPCH_DATA_DIR" \
            ./qgen -r "$TPCH_QGEN_SEED" -d "$q" > "$QUERIES_DIR/$q.sql") 2>/dev/null \
        || warn "qgen failed for query $q"
    done
fi

# Print locations for the run script.
cat <<EOF
TPCH_TOOLS_DIR=$TPCH_TOOLS_DIR
TPCH_DATA_DIR=$TPCH_DATA_DIR
TPCH_SCHEMA_FILE=$DBGEN_DIR/dss.ddl
TPCH_QUERIES_DIR=$QUERIES_DIR
EOF
