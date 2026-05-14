#!/usr/bin/env bash
# benchmarks/job/setup.sh — fetch the Join Order Benchmark (schema + queries)
# plus the IMDb CSV dump.
#
# IMDb dump (~3.6 GB compressed) comes from the canonical URL used in the
# original JOB paper (Leis et al. 2015). Set IMDB_URL to a local mirror if
# you've cached it. The harness will NOT re-download if the target files
# already exist.
#
# Layout:
#   benchmarks/.tools/join-order-benchmark/{schema.sql,fkindexes.sql,*.sql}
#   benchmarks/.data/job/{title,name,cast_info,...}.csv

set -euo pipefail
# shellcheck source=../common.sh
source "$(dirname "$0")/../common.sh"

require_cmd git
require_cmd curl
require_cmd tar

JOB_REPO_URL="${JOB_REPO_URL:-https://github.com/gregrahn/join-order-benchmark.git}"
JOB_REPO="$TOOLS_DIR_BASE/join-order-benchmark"
JOB_DATA="$DATA_DIR_BASE/job"
IMDB_URL="${IMDB_URL:-https://event.cwi.nl/da/job/imdb.tgz}"

if [[ ! -d "$JOB_REPO/.git" ]]; then
    log "cloning join-order-benchmark into $JOB_REPO"
    git clone --depth 1 "$JOB_REPO_URL" "$JOB_REPO"
fi

mkdir -p "$JOB_DATA"

# The canonical IMDb dump ships as imdb.tgz containing ~21 CSV files.
# Sentinel file: title.csv (the largest and most commonly missing).
if [[ ! -f "$JOB_DATA/title.csv" ]]; then
    if [[ -f "$JOB_DATA/imdb.tgz" ]]; then
        log "IMDb tarball already present, extracting only"
    else
        log "downloading IMDb dump (~3.6 GB) from $IMDB_URL"
        log "  (set IMDB_URL to a local mirror to avoid this fetch)"
        curl -fL --progress-bar "$IMDB_URL" -o "$JOB_DATA/imdb.tgz"
    fi
    log "extracting imdb.tgz"
    (cd "$JOB_DATA" && tar xzf imdb.tgz && rm -f imdb.tgz)
fi

cat <<EOF
JOB_REPO=$JOB_REPO
JOB_DATA=$JOB_DATA
JOB_SCHEMA=$JOB_REPO/schema.sql
EOF
