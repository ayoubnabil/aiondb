---
title: aiondb-config
order: 11
---

# aiondb-config

Runtime configuration model used by every server entry point. Defines the typed `RuntimeConfig` tree (storage, pgwire, security, limits, replication, HA, distributed, sharding) and the loaders that populate it from `AIONDB_*` environment variables or a `KEY=VALUE` config file. All fallible operations return `aiondb_core::DbResult<T>`.

## cargo

```toml
[dependencies]
aiondb-config = { path = "../aiondb-config" }
```

## modules

| module | purpose |
|---|---|
| `env` | Helpers around `std::env::var` (`env_string`, `env_u16`, `env_bool`, `env_optional_string`). |
| `ha` | High-availability and failover settings (`HaConfig`). |
| `loader` | `load_from_env` and `load_from_file` plus validation. |
| `pgwire` | Pgwire listener and TLS settings (`PgWireConfig`, `TlsMode`). |
| `product` | Public product contract metadata (`ProductConstraints`, `ProductSupportLevel`). |
| `replication` | WAL streaming settings (`ReplicationConfig`, `ReplicationRole`, `WriteConcern`, `WalCompression`, `WalLsnMode`). |
| `runtime` | Top-level `RuntimeConfig`, `LimitsConfig`, `EnginePoolConfig`, `DistributedConfig`, `RemoteNodeConfig`, `RemoteSnapshotMode`. |
| `security` | Security profile and password/lockout policy (`SecurityConfig`, `SecurityProfile`). |
| `storage` | Storage backend selection and pool sizing (`StorageConfig`, `StorageBackend`, `DurableWalCommitPolicy`). |
| `sys` | `total_system_memory()` for auto-tuning on Linux. |

## key types

- `RuntimeConfig` - aggregate config consumed by the server. Owns `storage`, `pgwire`, `security`, `limits`, `replication`, `ha`, `distributed`, and the `native_cypher` flag.
- `StorageConfig` / `StorageBackend` - selects between `InMemory`, `Durable`, `Disk`, `PageEngine`, `Lsm`, with page size, pool frame counts, and WAL commit policy.
- `PgWireConfig` / `TlsMode` - listen address, connection caps, TLS material paths, idle and startup timeouts, embedded `EnginePoolConfig`.
- `EnginePoolConfig` - worker thread count and queue depth for the pgwire engine pool.
- `LimitsConfig` - per-statement timeouts and result/memory ceilings.
- `ReplicationConfig` / `ReplicationRole` / `WriteConcern` / `WalCompression` / `WalLsnMode` - primary/replica/standalone topology, sync commit, write concern, WAL compression and LSN mode.
- `HaConfig` - failover toggle, node id, cluster member list, election and health-check timeouts.
- `DistributedConfig` / `RemoteNodeConfig` / `RemoteSnapshotMode` - remote fragment transport: nodes, mTLS material, circuit breaker, snapshot mode, embedded `aiondb_shard::ShardingConfig`.
- `SecurityConfig` / `SecurityProfile` - presets `Development`, `Staging`, `Production` plus password, lockout, audit, and session settings.
- `ProductConstraints` / `ProductSupportLevel` - declared topology, clustering, encryption-at-rest, and backup support level for the v0.1 release line.

## loading

`load_from_env` reads `AIONDB_*` keys from the process environment. `load_from_file(path)` reads the same keys from a `KEY=VALUE` file (lines starting with `#` are comments). Both call the same validation pass and return `DbResult<RuntimeConfig>`. Setting `AIONDB_CONFIG_STRICT=true` makes unknown `AIONDB_*` keys fail loading; otherwise unknown keys are logged at `warn` and ignored.

## example

```rust
use aiondb_config::{
    load_from_env, RuntimeConfig, SecurityProfile, StorageBackend, TlsMode,
};

let config: RuntimeConfig = load_from_env().expect("config from env");

assert_eq!(config.storage.backend, StorageBackend::Durable);
assert_eq!(config.security.profile, SecurityProfile::Development);
assert!(matches!(
    config.pgwire.tls_mode,
    TlsMode::Disable | TlsMode::Prefer | TlsMode::Require
));
```

Building a config in-process without env vars uses `Default`:

```rust
use aiondb_config::{RuntimeConfig, ReplicationRole};

let mut config = RuntimeConfig::default();
config.replication.role = ReplicationRole::Standalone;
config.pgwire.max_connections = 64;
```

## product constraints

```rust
use aiondb_config::{ProductSupportLevel, V0_1_PRODUCT_CONSTRAINTS};

assert_eq!(V0_1_PRODUCT_CONSTRAINTS.release_line, "0.1");
assert_eq!(V0_1_PRODUCT_CONSTRAINTS.topology, "single-node");
assert_eq!(
    V0_1_PRODUCT_CONSTRAINTS.clustering,
    ProductSupportLevel::Unsupported
);
```
