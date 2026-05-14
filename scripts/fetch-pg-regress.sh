#!/usr/bin/env bash
# Fetch external regression fixtures into .pg-regress/.
# Downloads:
#   - PostgreSQL pg_regress suite (sql/, expected/, data/) from postgres/postgres
#   - SQLLogicTest suite (index/, random/, select*.test, evidence/) from
#     gregrahn/sqllogictest mirror
#
# The .pg-regress/ tree (minus the runner crate) is git-ignored. Re-run this
# script whenever you need to recreate the fixtures locally or in CI.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="$ROOT_DIR/.pg-regress"

PG_VERSION="${PG_REGRESS_PG_VERSION:-REL_16_STABLE}"
PG_REPO="${PG_REGRESS_PG_REPO:-https://github.com/postgres/postgres.git}"
SLT_REPO="${PG_REGRESS_SLT_REPO:-https://github.com/gregrahn/sqllogictest.git}"
SLT_REF="${PG_REGRESS_SLT_REF:-master}"

CACHE_DIR="${PG_REGRESS_CACHE_DIR:-$DEST_DIR/.cache}"
PG_SRC="$CACHE_DIR/postgres"
SLT_SRC="$CACHE_DIR/sqllogictest"

log() { printf '[fetch-pg-regress] %s\n' "$*"; }
die() { printf '[fetch-pg-regress] error: %s\n' "$*" >&2; exit 1; }

command -v git >/dev/null 2>&1 || die "git not found"

mkdir -p "$CACHE_DIR" "$DEST_DIR"

clone_or_update() {
  local repo="$1" ref="$2" dest="$3"
  if [[ -d "$dest/.git" ]]; then
    log "updating $(basename "$dest") ($ref)"
    git -C "$dest" fetch --depth=1 origin "$ref"
    git -C "$dest" checkout -q FETCH_HEAD
  else
    log "cloning $repo @ $ref -> $dest"
    git clone --depth=1 --branch "$ref" "$repo" "$dest" 2>/dev/null \
      || git clone --depth=1 "$repo" "$dest"
  fi
}

sync_dir() {
  local src="$1" dst="$2"
  [[ -d "$src" ]] || die "missing source: $src"
  mkdir -p "$dst"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a --delete "$src/" "$dst/"
  else
    rm -rf "$dst"
    cp -a "$src" "$dst"
  fi
}

log "destination: $DEST_DIR"
log "postgres ref: $PG_VERSION"
log "sqllogictest ref: $SLT_REF"

clone_or_update "$PG_REPO" "$PG_VERSION" "$PG_SRC"
clone_or_update "$SLT_REPO" "$SLT_REF" "$SLT_SRC"

PG_REGRESS_SRC="$PG_SRC/src/test/regress"
[[ -d "$PG_REGRESS_SRC" ]] || die "pg regress dir missing in clone: $PG_REGRESS_SRC"

log "syncing pg_regress sql/"
sync_dir "$PG_REGRESS_SRC/sql" "$DEST_DIR/sql"

log "syncing pg_regress expected/"
sync_dir "$PG_REGRESS_SRC/expected" "$DEST_DIR/expected"

log "syncing pg_regress data (regress/data)/"
if [[ -d "$PG_REGRESS_SRC/data" ]]; then
  sync_dir "$PG_REGRESS_SRC/data" "$DEST_DIR/regress/data"
fi
sync_dir "$PG_REGRESS_SRC/sql" "$DEST_DIR/regress/sql"
sync_dir "$PG_REGRESS_SRC/expected" "$DEST_DIR/regress/expected"

log "syncing sqllogictest test/"
SLT_TEST="$SLT_SRC/test"
[[ -d "$SLT_TEST" ]] || die "sqllogictest test dir missing: $SLT_TEST"
sync_dir "$SLT_TEST" "$DEST_DIR/sqllogictest"

log "done"
log "fixtures size: $(du -sh "$DEST_DIR" | awk '{print $1}')"
log "to skip re-cloning, keep $CACHE_DIR; to purge: rm -rf $CACHE_DIR"
