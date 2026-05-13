---
title: aiondb-server
order: 54
---

# aiondb-server

The standalone `aiondb` server binary. Boots `aiondb-engine`, the pgwire listener, the fragment transport listener, and an HTTP observability endpoint, then waits for shutdown signals. Single-node only in the v0.1 contract; persistent backends require LUKS/dm-crypt encryption unless `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true` is set.

## cargo

```toml
[dependencies]
# Used as a binary, not a library:
aiondb-server = { path = "../aiondb-server" }
```

## binary

| Binary | Path |
|---|---|
| `aiondb` | `crates/aiondb-server/src/main.rs` |

## cli flags

The CLI is hand-parsed in `parse_cli_args`. Unknown arguments exit with an error.

| Flag | Description |
|---|---|
| `--help`, `-h` | Print the help text and exit. |
| `--version`, `-V` | Print the server version and exit. |
| `--ephemeral` | Run fully in memory. Equivalent to `--storage-backend in_memory`. Test/development only. |
| `--listen-addr <host:port>` | Pgwire listen address. Overrides `AIONDB_PGWIRE_LISTEN_ADDR`. Default `127.0.0.1:5432`. |
| `--data-dir <path>` | Directory for persistent storage state. Default `./data/aiondb`, or `AIONDB_STORAGE_DATA_DIR` when set. Ignored under `--ephemeral` or an in-memory backend. |
| `--storage-backend <backend>` | Select the storage backend. Values: `in_memory`, `durable`, `disk`, `page_engine`, `lsm`. Default `durable` (or `AIONDB_STORAGE_BACKEND`). |
| `--bootstrap-user <name>` / `--bootstrap-password <pwd>` | Provision a local superuser at startup. Both flags must be passed together. Dev / CI / benchmark only. The password is validated against the security baseline (minimum 12 chars, mixed case, digit, symbol). |
| `--allow-unencrypted-storage` | Allow a persistent backend on a non-encrypted filesystem. Same effect as `AIONDB_ALLOW_UNENCRYPTED_STORAGE=true`. |
| `--observability-bind <host>` / `--observability-port <port>` | Override the HTTP observability endpoint bind/port. |
| `--no-observability` | Disable the HTTP observability endpoint entirely. |

### subcommands

| Command | Effect |
|---|---|
| `aiondb doctor --data-dir <path>` | Inspect a data directory without opening it for writes. Prints storage-format version, corruption findings, WAL/snapshot/page status, and whether `upgrade` is possible. Read-only. |
| `aiondb upgrade --data-dir <path>` | Create an idempotent storage-format v1 manifest. Refuses ambiguous or corrupt state and creates a backup before modifying the data directory. |
| `aiondb dump --data-dir <path> --output <relative.sql>` | Canonical SQL dump. The output path is resolved relative to the data dir. |
| `aiondb restore --data-dir <path> --input <relative.sql>` | Canonical SQL restore. Input path resolved relative to the data dir. |

## environment variables

The full set of recognised keys lives in `crates/aiondb-config/src/loader.rs` (over 140 keys spanning storage, pgwire, security, limits, replication, HA, distributed control plane, and engine pool tuning). Setting `AIONDB_CONFIG_STRICT=true` makes unknown `AIONDB_*` keys fail loading; otherwise they are logged at `warn` and ignored.

The variables most commonly set on the server entry point:

| Variable | Effect |
|---|---|
| `AIONDB_IN_MEMORY=true` | Equivalent to `--ephemeral`. |
| `AIONDB_STORAGE_BACKEND` | Same as `--storage-backend`. |
| `AIONDB_STORAGE_DATA_DIR` | Same as `--data-dir`. |
| `AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY` | Durable WAL commit policy: `always`, `every:N`, `never`. |
| `AIONDB_PGWIRE_LISTEN_ADDR` | pgwire listen address (default `127.0.0.1:5432`). |
| `AIONDB_PGWIRE_MAX_CONNECTIONS`, `AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP` | Connection caps. Clamped by the memory-safety guard at startup. |
| `AIONDB_PGWIRE_STARTUP_TIMEOUT_MS`, `AIONDB_PGWIRE_IDLE_TIMEOUT_MS`, `AIONDB_PGWIRE_AUTH_FAILURE_BACKOFF_MS` | Per-connection timing knobs. |
| `AIONDB_PGWIRE_TLS_MODE` | TLS policy: `disable`, `prefer`, `require`. |
| `AIONDB_PGWIRE_TLS_CERT_PATH`, `AIONDB_PGWIRE_TLS_KEY_PATH` | PEM material for pgwire TLS. Must be set together. |
| `AIONDB_PGWIRE_TLS_CLIENT_CA_PATH` | Optional PEM CA for pgwire mTLS. Requires the cert/key pair above. |
| `AIONDB_OBSERVABILITY_BIND`, `AIONDB_OBSERVABILITY_PORT`, `AIONDB_OBSERVABILITY_FAIL_FAST` | Bind, port, and fail-fast policy for the observability HTTP server (defaults `127.0.0.1`, `9187`, soft-fail). |
| `AIONDB_REPLICATION_ROLE` | `standalone`, `primary`, or `replica`. |
| `AIONDB_REPLICATION_PRIMARY_CONNINFO` | libpq-style conninfo for `replica` role. Required when `role=replica`. |
| `AIONDB_REPLICATION_MAX_WAL_SENDERS`, `AIONDB_REPLICATION_WAL_KEEP_SEGMENTS`, `AIONDB_REPLICATION_STATUS_INTERVAL_MS` | Primary-side wal sender tuning. |
| `AIONDB_REPLICATION_SYNCHRONOUS_COMMIT`, `AIONDB_REPLICATION_WRITE_CONCERN`, `AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS` | Commit acknowledgement policy. |
| `AIONDB_REPLICATION_PROMOTE_ON_START` | Bump the timeline and promote on startup. |
| `AIONDB_HA_ENABLED`, `AIONDB_HA_NODE_ID`, `AIONDB_HA_PORT`, `AIONDB_HA_CLUSTER_NODES`, `AIONDB_HA_AUTH_TOKEN` | High-availability runtime (see [`aiondb-ha`](/specification-avancee/aiondb-ha.html)). |
| `AIONDB_HA_ELECTION_TIMEOUT_MS`, `AIONDB_HA_HEALTH_CHECK_INTERVAL_MS`, `AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS`, `AIONDB_HA_MAX_FAILOVER_LAG`, `AIONDB_HA_FENCING_TOKEN_PATH` | Failover orchestrator tuning. |
| `AIONDB_DISTRIBUTED_*` | Fragment transport peers, mTLS material, circuit breaker, retries, snapshot mode. See `aiondb-config/src/loader.rs` for the complete list. |
| `AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST` | Fail startup if the fragment transport listener cannot initialise. |
| `AIONDB_ENGINE_POOL_WORKER_THREADS`, `AIONDB_ENGINE_POOL_QUEUE_DEPTH` | Pgwire engine pool sizing. Both are clamped by the memory-safety guard. |
| `AIONDB_LIMITS_*` | Per-statement timeout, lock timeout, result rows/bytes, temp bytes, memory ceiling, prepared statements, portals, parallel workers, recursive rows/iterations. |
| `AIONDB_ALLOW_UNENCRYPTED_STORAGE` | Allow persistent storage on a non-encrypted filesystem. |
| `AIONDB_BOOTSTRAP_USER`, `AIONDB_BOOTSTRAP_PASSWORD` | Provision a local superuser at startup. Dev / CI / benchmark only. Password must satisfy the security baseline. |
| `AIONDB_BENCH_MODE` | Swap the session authorizer for `AllowAllAuthorizer` and relax some clamps. Bench harnesses only — never for any deployment that accepts untrusted connections. |
| `AIONDB_CONFIG_STRICT` | Fail loading on unknown `AIONDB_*` keys instead of warning. |
| `AIONDB_ALLOW_PUBLIC_OBSERVABILITY`, `AIONDB_DISABLE_MEMORY_GUARD` | Recognised but always rejected; logged with a warning so a previously-set value cannot silently relax the security baseline. |

The HTTP observability server exposes `/livez`, `/healthz`, `/readyz`, `/metrics`, and `/info`.

## example

Run an ephemeral server with a bootstrap superuser, then connect with `psql`:

```sh
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
aiondb --ephemeral

psql "host=127.0.0.1 port=5432 dbname=default user=dev password=DevPassword42! sslmode=disable"
```

Run a durable single-node server with WAL state under `./data/aiondb`:

```sh
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
aiondb --data-dir ./data/aiondb --storage-backend durable
```
