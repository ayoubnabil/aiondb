#!/usr/bin/env bash
# scripts/security-baseline-check.sh
#
# Pre-deployment static check that the AionDB process is not about to start
# with any of the known-unsafe environment combinations. Read-only — does
# not modify the process or the host. Exits non-zero on any failure so it
# can be wired into CI / a pre-systemd-start ExecStartPre hook.
#
# Usage:
#   scripts/security-baseline-check.sh                 # check current shell env
#   env -i FOO=bar scripts/security-baseline-check.sh  # check a specific env

set -u

fail=0
warn() { printf '\033[33m[warn]\033[0m %s\n' "$*" >&2; fail=$((fail + 1)); }
info() { printf '\033[32m[ok]\033[0m %s\n' "$*" >&2; }
is_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }
uint_trim_leading_zeroes() {
    local value="$1"
    local prefix="${value%%[!0]*}"
    value="${value#"$prefix"}"
    if [[ -z "$value" ]]; then
        printf '0'
    else
        printf '%s' "$value"
    fi
}
uint_gt() {
    local left right
    left="$(uint_trim_leading_zeroes "$1")"
    right="$(uint_trim_leading_zeroes "$2")"
    if (( ${#left} != ${#right} )); then
        (( ${#left} > ${#right} ))
    else
        [[ "$left" > "$right" ]]
    fi
}
check_uint() {
    local name="$1"
    local value="$2"
    if ! is_uint "$value"; then
        warn "${name}=${value} — expected an unsigned integer"
        return 1
    fi
    return 0
}

# 1. Encryption-at-rest bypass should NOT be set in production.
if [[ "${AIONDB_ALLOW_UNENCRYPTED_STORAGE:-false}" == "true" ]]; then
    warn "AIONDB_ALLOW_UNENCRYPTED_STORAGE=true — persistent data will be written UNENCRYPTED"
    warn "  → use a LUKS-encrypted volume and unset this variable in production"
else
    info "encryption-at-rest bypass is OFF"
fi

if [[ -n "${AIONDB_BOOTSTRAP_USER:-}" ]]; then
    warn "AIONDB_BOOTSTRAP_USER=${AIONDB_BOOTSTRAP_USER:-} — startup will provision a bootstrap superuser role"
    warn "  → unset both AIONDB_BOOTSTRAP_USER and AIONDB_BOOTSTRAP_PASSWORD before going live"
else
    info "no bootstrap superuser env override"
fi

# 3. Statement timeout must be >0 in production.
statement_timeout="${AIONDB_LIMITS_STATEMENT_TIMEOUT_MS:-30000}"
if ! check_uint "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS" "$statement_timeout"; then
    warn "  → set a real budget in milliseconds (e.g. 30000 for 30 s)"
elif [[ "$(uint_trim_leading_zeroes "$statement_timeout")" == "0" ]]; then
    warn "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=0 — statements can run forever (DoS surface)"
    warn "  → set a real budget (e.g. 30000 for 30 s)"
else
    info "statement timeout = ${statement_timeout} ms"
fi

# 4. Bound at least one of the result-byte / result-row caps.
result_bytes="${AIONDB_LIMITS_MAX_RESULT_BYTES:-67108864}"
result_rows="${AIONDB_LIMITS_MAX_RESULT_ROWS:-2000000}"
if ! check_uint "AIONDB_LIMITS_MAX_RESULT_BYTES" "$result_bytes"; then
    warn "  → set a byte cap such as 67108864 (64 MiB)"
elif uint_gt "$result_bytes" 1073741824; then
    warn "AIONDB_LIMITS_MAX_RESULT_BYTES=${result_bytes} (> 1 GiB)"
    warn "  → a single SELECT can buffer up to that much per response"
else
    info "result byte cap = $((result_bytes / 1024 / 1024)) MiB"
fi
if ! check_uint "AIONDB_LIMITS_MAX_RESULT_ROWS" "$result_rows"; then
    warn "  → set a row cap such as 2000000"
elif uint_gt "$result_rows" 10000000; then
    warn "AIONDB_LIMITS_MAX_RESULT_ROWS=${result_rows} (> 10 M rows)"
else
    info "result row cap = ${result_rows}"
fi

listen="${AIONDB_PGWIRE_LISTEN_ADDR:-127.0.0.1:5432}"
case "$listen" in
    0.0.0.0|0.0.0.0:*|::|:::*|\[::\]|\[::\]:*|0:0:0:0:0:0:0:0|0:0:0:0:0:0:0:0:*|\[0:0:0:0:0:0:0:0\]|\[0:0:0:0:0:0:0:0\]:*)
        listen_is_wildcard=true
        ;;
    *)
        listen_is_wildcard=false
        ;;
esac
if [[ "$listen_is_wildcard" == true ]]; then
    tls_mode="${AIONDB_PGWIRE_TLS_MODE:-prefer}"
    tls_mode="${tls_mode,,}"
    if [[ "$tls_mode" != "require" ]]; then
        warn "listening on $listen with AIONDB_PGWIRE_TLS_MODE=${AIONDB_PGWIRE_TLS_MODE:-prefer}"
        warn "  → remote pgwire exposure requires AIONDB_PGWIRE_TLS_MODE=require"
    elif [[ -z "${AIONDB_PGWIRE_TLS_CERT_PATH:-}" || -z "${AIONDB_PGWIRE_TLS_KEY_PATH:-}" ]]; then
        warn "listening on $listen WITHOUT TLS cert — credentials would cross the network in plaintext"
        warn "  → either bind to 127.0.0.1 or set AIONDB_PGWIRE_TLS_CERT_PATH/KEY_PATH"
    else
        info "listening on $listen with TLS required and cert configured"
    fi
else
    info "listen address = $listen"
fi

# 6. Per-commit paged-state persistence is OK for OLTP but make sure the
#    operator knows what they have.
if [[ "${AIONDB_PERSIST_PAGED_STATE_ON_COMMIT:-1}" != "0" ]]; then
    warn "AIONDB_PERSIST_PAGED_STATE_ON_COMMIT is enabled"
    warn "  → expected for warehouse / append-only; OLTP throughput drops by ~3000×"
    warn "  → unset or set to 0 for queue-worker / row-update workloads"
else
    info "paged-state persist = lazy (OK for OLTP)"
fi

if (( fail > 0 )); then
    printf '\n\033[31m%d security-baseline finding(s) above\033[0m — review before deploying.\n' "$fail" >&2
    exit 1
fi
printf '\n\033[32mall baseline checks passed\033[0m\n' >&2
exit 0
