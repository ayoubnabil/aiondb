---
title: Configuration
order: 57
---

# Configuration

AionDB can be configured through CLI flags and environment variables. CLI flags should be used for local clarity; environment variables are useful for scripts, benchmark harnesses, and deployments.

## CLI flags

```bash
aiondb --help
aiondb --version
aiondb --ephemeral
aiondb --data-dir ./data/aiondb
aiondb --storage-backend durable
```

Storage backend values:

- `in_memory`
- `durable`
- `disk`
- `page_engine`
- `lsm`

For v0.1, use `--ephemeral` for demos and `durable` for persistent local evaluation unless you are testing a specific backend. The storage v1 compatibility promise covers durable SQL table/WAL/index files; `lsm` remains an experimental backend format.

## Configuration precedence

Prefer CLI flags for values a human should see immediately, such as `--ephemeral` or `--data-dir`. Prefer environment variables for scripts and benchmark harnesses where the same command needs to run across machines.

When documenting a benchmark or bug, include both:

- the command line;
- relevant `AIONDB_*` environment variables.

That prevents hidden environment state from changing the result.

## Common environment variables

| Variable | Purpose |
| --- | --- |
| `AIONDB_PGWIRE_LISTEN_ADDR` | PostgreSQL wire listen address. Default: `127.0.0.1:5432`. |
| `AIONDB_STORAGE_BACKEND` | Storage backend. Same values as `--storage-backend`. |
| `AIONDB_STORAGE_DATA_DIR` | Persistent storage directory. |
| `AIONDB_IN_MEMORY` | Run with in-memory storage when set to `true`. |
| `AIONDB_OBSERVABILITY_BIND` | Observability HTTP bind address. Default: `127.0.0.1`. |
| `AIONDB_OBSERVABILITY_PORT` | Observability HTTP port. Default: `9187`. |
| `AIONDB_PGWIRE_TLS_MODE` | `disable`, `prefer`, or `require`. |
| `AIONDB_BOOTSTRAP_USER` | Development bootstrap role. |
| `AIONDB_BOOTSTRAP_PASSWORD` | Development bootstrap password. |
| `AIONDB_ALLOW_UNENCRYPTED_STORAGE` | Development override for persistent storage on an unencrypted filesystem. |

The systemd packaging profile includes `packaging/systemd/aiondb.env.example`.
Copy it to `/etc/aiondb/aiondb.env`, restrict permissions, and replace all
commented placeholders before enabling the service. The example keeps the
bootstrap user disabled, requires TLS, and leaves the unencrypted-storage
override commented so production-style evaluations do not inherit tutorial
defaults.

## Subsystem variables

Variables that belong to a single subsystem are grouped by prefix. The loader rejects unknown `AIONDB_*` names when `AIONDB_CONFIG_STRICT=true`.

### Security

```bash
AIONDB_SECURITY_PROFILE=development|staging|production
AIONDB_SECURITY_ALLOW_ANONYMOUS_LOCAL=false
AIONDB_SECURITY_ALLOW_EPHEMERAL_USERS=false
AIONDB_SECURITY_REQUIRE_TLS_FOR_PASSWORD=true
AIONDB_SECURITY_PASSWORD_MIN_LENGTH=12
AIONDB_SECURITY_PASSWORD_REQUIRE_LOWERCASE=true
AIONDB_SECURITY_PASSWORD_REQUIRE_UPPERCASE=true
AIONDB_SECURITY_PASSWORD_REQUIRE_DIGIT=true
AIONDB_SECURITY_PASSWORD_REQUIRE_SYMBOL=true
AIONDB_SECURITY_REJECT_ROLE_NAME_AS_PASSWORD=true
AIONDB_SECURITY_MAX_AUTH_FAILURES=5
AIONDB_SECURITY_AUTH_LOCKOUT_WINDOW_MS=600000
AIONDB_SECURITY_DURABLE_AUTH_LOCKOUT=false
AIONDB_SECURITY_AUTH_LOCKOUT_STATE_PATH=/var/lib/aiondb/auth-lockout.state
AIONDB_SECURITY_DURABLE_AUTH_AUDIT=false
AIONDB_SECURITY_AUTH_AUDIT_LOG_PATH=/var/log/aiondb/auth-audit.log
AIONDB_SECURITY_AUTH_AUDIT_MAX_FILE_SIZE_BYTES=10485760
AIONDB_SECURITY_AUTH_AUDIT_MAX_ROTATED_FILES=5
AIONDB_SECURITY_DDL_AUDIT_ENABLED=false
AIONDB_SECURITY_MAX_CONCURRENT_SESSIONS_PER_ROLE=100
AIONDB_SECURITY_MAX_SESSION_IDLE_TIMEOUT_MS=0
AIONDB_SECURITY_MAX_SESSION_LIFETIME_MS=0
AIONDB_SECURITY_MAX_TRANSACTION_IDLE_TIMEOUT_MS=0
```

### pgwire

```bash
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432
AIONDB_PGWIRE_TLS_MODE=disable|prefer|require
AIONDB_PGWIRE_TLS_CERT_PATH=/path/server.crt
AIONDB_PGWIRE_TLS_KEY_PATH=/path/server.key
AIONDB_PGWIRE_TLS_CLIENT_CA_PATH=/path/client-ca.crt
AIONDB_PGWIRE_STARTUP_TIMEOUT_MS=10000
AIONDB_PGWIRE_IDLE_TIMEOUT_MS=0
AIONDB_PGWIRE_AUTH_FAILURE_BACKOFF_MS=500
AIONDB_PGWIRE_MAX_CONNECTIONS=200
AIONDB_PGWIRE_MAX_CONNECTIONS_PER_IP=50
AIONDB_PGWIRE_COPY_IN_MAX_BUFFER=8388608
AIONDB_PGWIRE_COPY_IN_TOTAL_TIMEOUT_MS=60000
```

### Engine pool

```bash
AIONDB_ENGINE_POOL_WORKER_THREADS=4
AIONDB_ENGINE_POOL_QUEUE_DEPTH=64
AIONDB_ENGINE_DISABLE_PARSED_SQL_FINGERPRINT_CACHE=false
```

### Replication

```bash
AIONDB_REPLICATION_ROLE=primary|replica
AIONDB_REPLICATION_PRIMARY_CONNINFO="host=primary.example.com port=5432 user=replicator application_name=node-b"
AIONDB_REPLICATION_MAX_WAL_SENDERS=8
AIONDB_REPLICATION_FACTOR=1
AIONDB_REPLICATION_WRITE_CONCERN=local|majority|all|factor:N
AIONDB_REPLICATION_SYNCHRONOUS_COMMIT=true
AIONDB_REPLICATION_SYNC_COMMIT_TIMEOUT_MS=10000
AIONDB_REPLICATION_STATUS_INTERVAL_MS=1000
AIONDB_REPLICATION_WAL_KEEP_SEGMENTS=16
AIONDB_REPLICATION_WAL_COMPRESSION=none|lz4|zstd
AIONDB_REPLICATION_WAL_LSN_MODE=logical|byte_offset
AIONDB_REPLICATION_PROMOTE_ON_START=false
```

For distributed replica repair, set `application_name` in
`AIONDB_REPLICATION_PRIMARY_CONNINFO` to the same value as the node's
distributed `NodeId`. The primary records that name in replication
progress, so staged learners can be promoted only after the matching
replica reports `apply_lsn` catch-up.

`AIONDB_REPLICATION_WRITE_CONCERN` also accepts the legacy aliases
`async` for `local` and `sync` for `all`.
For `factor:N`, `N` is a replica-ack count, must be at least `1`, and
must not exceed `AIONDB_REPLICATION_FACTOR - 1`.
`AIONDB_REPLICATION_SYNCHRONOUS_COMMIT=true` is a legacy compatibility
flag; it upgrades the default write concern to `majority` only when
`AIONDB_REPLICATION_WRITE_CONCERN` is not set.

The replica client does not negotiate TLS yet. `sslmode=require`,
`sslmode=verify-ca`, and `sslmode=verify-full` are rejected instead of
falling back to plaintext.

`AIONDB_REPLICATION_PRIMARY_CONNINFO` accepts libpq-style quoted values
for spaces and rejects embedded NUL bytes before building startup or
password frames.

### High availability

```bash
AIONDB_HA_ENABLED=false
AIONDB_HA_NODE_ID=1
AIONDB_HA_PORT=7600
AIONDB_HA_CLUSTER_NODES="host1:7600,host2:7600"
AIONDB_HA_AUTH_TOKEN=<shared-secret>
AIONDB_HA_HEALTH_CHECK_INTERVAL_MS=3000
AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS=10000
AIONDB_HA_ELECTION_TIMEOUT_MS=30000
AIONDB_HA_MAX_FAILOVER_LAG=1000
AIONDB_HA_FENCING_TOKEN_PATH=/var/lib/aiondb/fencing
```

`AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS` must be greater than `AIONDB_HA_HEALTH_CHECK_INTERVAL_MS`. `AIONDB_HA_ELECTION_TIMEOUT_MS` must be greater than `AIONDB_HA_HEALTH_CHECK_TIMEOUT_MS`. The loader rejects configurations that violate these ordering constraints.

`AIONDB_HA_NODE_ID` is one-based: `1` refers to the first address in
`AIONDB_HA_CLUSTER_NODES`, `2` to the second, and so on.

### Sharding

```bash
AIONDB_SHARDING_ENABLED=false
AIONDB_SHARDING_DEFAULT_SHARD_COUNT=16
AIONDB_SHARDING_VIRTUAL_NODES_PER_SHARD=128
AIONDB_SHARDING_REPLICATION_FACTOR=1
AIONDB_SHARDING_AUTO_REBALANCE=false
AIONDB_SHARDING_MAX_LEARNERS_PER_SHARD=1
AIONDB_SHARDING_MAX_LEARNERS_PER_NODE=64
AIONDB_SHARDING_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE=16
AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA=1
AIONDB_SHARDING_NODE_ATTRIBUTES="local:region=eu-west;zone=az-a,node-b:region=eu-north;zone=az-b"
AIONDB_SHARDING_PLACEMENT_REQUIRED_ATTRIBUTES="disk=ssd"
AIONDB_SHARDING_LEASE_PREFERENCE_ATTRIBUTES="region=eu-west"
AIONDB_SHARDING_PLACEMENT_SPREAD_ATTRIBUTES="region,zone"
```

Placement policy is applied to initial shard placement and configured automatic
repair. Required attributes filter candidate voters. Lease preferences choose
the initial leader/leaseholder when matching candidates exist. Spread
attributes try to keep voting replicas apart across failure domains such as
region and zone.
Leadership balancing is bounded per maintenance pass with
`AIONDB_SHARDING_LEADERSHIP_MAX_TRANSFERS_PER_MAINTENANCE`, and live-leader
balancing only moves leaders when the source is at least
`AIONDB_SHARDING_LEADERSHIP_MIN_LOAD_DELTA` hotter than the target.

### Distributed execution

```bash
AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_PORT=7700
AIONDB_DISTRIBUTED_FRAGMENT_TRANSPORT_FAIL_FAST=false
AIONDB_DISTRIBUTED_INTER_NODE_AUTH_TOKEN=<shared-secret>
AIONDB_DISTRIBUTED_REMOTE_NODES="node-a=host-a:7700,node-b=host-b:7700"
AIONDB_DISTRIBUTED_REMOTE_SNAPSHOT_MODE=latest_visible|coordinator
AIONDB_DISTRIBUTED_REMOTE_CONNECT_TIMEOUT_MS=5000
AIONDB_DISTRIBUTED_REMOTE_RETRY_BACKOFF_MS=250
AIONDB_DISTRIBUTED_REMOTE_MAX_RETRIES=3
AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_FAILURE_THRESHOLD=5
AIONDB_DISTRIBUTED_REMOTE_CIRCUIT_BREAKER_RESET_TIMEOUT_MS=30000
AIONDB_DISTRIBUTED_ALLOW_UNREGISTERED_LOOPBACK_NODES=false
AIONDB_DISTRIBUTED_LOOPBACK_NODES="node-test"
AIONDB_DISTRIBUTED_REQUIRE_TLS=true
AIONDB_DISTRIBUTED_TLS_CERT_PATH=/path/node.crt
AIONDB_DISTRIBUTED_TLS_KEY_PATH=/path/node.key
AIONDB_DISTRIBUTED_TLS_CA_CERT_PATH=/path/cluster-ca.crt
```

### WAL integrity

```bash
AIONDB_WAL_LOCAL_HMAC_KEY=<32-byte-hex>
AIONDB_WAL_ARCHIVE_HMAC_KEY=<32-byte-hex>
```

When set, the WAL writer persists a `.auth` HMAC sidecar next to every active segment and recovery refuses records whose bytes were modified offline. The key must live outside the data directory.

### Per-session limits

```bash
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=30000
AIONDB_LIMITS_LOCK_TIMEOUT_MS=1000
AIONDB_LIMITS_MAX_RESULT_ROWS=10000
AIONDB_LIMITS_MAX_RESULT_BYTES=8388608
AIONDB_LIMITS_MAX_MEMORY_BYTES=67108864
AIONDB_LIMITS_MAX_TEMP_BYTES=268435456
AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY=1
AIONDB_LIMITS_MAX_PORTALS=64
AIONDB_LIMITS_MAX_PREPARED_STATEMENTS=128
AIONDB_LIMITS_MAX_RECURSIVE_ITERATIONS=10000
AIONDB_LIMITS_MAX_RECURSIVE_ROWS=1000000
```

### Operational toggles

```bash
AIONDB_CONFIG_STRICT=true
AIONDB_ALLOW_PLAINTEXT_PUBLIC=false
AIONDB_ALLOW_PUBLIC_OBSERVABILITY=false
AIONDB_DISABLE_MEMORY_GUARD=false
AIONDB_OBSERVABILITY_FAIL_FAST=false
```

`AIONDB_CONFIG_STRICT=true` makes the loader reject unknown `AIONDB_*` variables. The `ALLOW_PUBLIC_*` toggles must remain `false` unless the deployment owner has actively decided to expose the surface; they exist so that an accidental non-loopback bind fails closed instead of silently going public.

## Limits

AionDB includes explicit limits for result size, memory, temporary data, recursive execution, portals, prepared statements, and statement timeout. The defaults are conservative because v0.1 is an alpha.

Common limit variables:

```bash
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=30000
AIONDB_LIMITS_MAX_RESULT_ROWS=10000
AIONDB_LIMITS_MAX_RESULT_BYTES=8388608
AIONDB_LIMITS_MAX_MEMORY_BYTES=67108864
AIONDB_LIMITS_MAX_TEMP_BYTES=268435456
AIONDB_LIMITS_MAX_PARALLEL_WORKERS_PER_QUERY=1
```

Benchmark scripts may override these values so benchmark runs are not accidentally capped by small development defaults.

## Example local profiles

Ephemeral development:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
aiondb --ephemeral
```

Persistent local evaluation:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
aiondb --data-dir ./data/aiondb
```

Custom pgwire port:

```bash
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:15432 \
aiondb --ephemeral
```

## WAL durability

The durable backend defaults to fsync on commit. Development and benchmark runs may relax this with:

```bash
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=every:10
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=never
```

Use `always` for durability-sensitive evaluation. Non-`always` policies are for controlled benchmarks and development only.

## Configuration hygiene

Do not publish benchmark numbers without listing durability policy, storage backend, data directory type, and limits. Those settings can change performance by more than the query optimizer change being measured.

For support reports, redact secrets but keep variable names visible. For example, say `AIONDB_BOOTSTRAP_PASSWORD=<redacted>` instead of omitting the variable entirely.
