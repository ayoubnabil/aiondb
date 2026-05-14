---
title: aiondb-extension
order: 42
---

# aiondb-extension

Trait-based framework for compiled-in extensions. Each extension is a Rust struct that implements `Extension`, declares metadata, and registers scalar functions through an `ExtensionRegistrar`. The `ExtensionRegistry` tracks which extensions are available and which have been installed via `CREATE EXTENSION`.

## cargo

```toml
[dependencies]
aiondb-extension = { path = "../aiondb-extension" }
```

## modules

| module | purpose |
|---|---|
| `registry` | `ExtensionRegistry`, `ExtensionDescriptor`, `ExtensionFunction`, `InstalledExtension`. |
| `builtin::uuid_ossp` | the `uuid-ossp` built-in extension. |
| `builtin::pgcrypto` | the `pgcrypto` built-in extension. |
| `builtin::vector` | the `vector` built-in extension, exposing the distance functions used by similarity search. |

## key types

| item | description |
|---|---|
| `Extension` trait | implemented by every extension; exposes `name`, `version`, `description`, `dependencies`, `install`, `upgrade`. |
| `ExtensionRegistrar` trait | callback passed to `install`, with `register_function`. |
| `ExtensionEvalFn` | `fn(&[Value]) -> DbResult<Value>` used for native scalar evaluation. |
| `ExtensionFunction` | name, return type, arity bounds, native eval function. |
| `ExtensionDescriptor` | name, default version, description, dependencies. |
| `InstalledExtension` | OID, name, version, relocatable flag for `pg_extension`. |
| `ExtensionRegistry` | `list_available`, `list_installed`, `is_installed`, `lookup_function`, `installed_version`, `install_extension`, `drop_extension`, `alter_extension_update`. |
| `UuidOsspExtension` | built-in `uuid-ossp` (`uuid_generate_v1`, `uuid_generate_v4`, `uuid_nil`, namespace UUIDs). |
| `PgcryptoExtension` | built-in `pgcrypto`. |
| `VectorExtension` | built-in `vector` -- pgvector compatibility marker. AionDB exposes the vector type, distance functions (`l2_distance`, `cosine_distance`, `inner_product`, `manhattan_distance`), and ANN index support as engine built-ins; this extension only writes a `pg_extension` row so `CREATE EXTENSION vector` succeeds for pgvector migrations. The reported extension version is `0.8.2`. |

## example

```rust
use aiondb_extension::ExtensionRegistry;

let registry = ExtensionRegistry::new();
assert!(!registry.is_installed("uuid-ossp"));

registry
    .install_extension("uuid-ossp", false)
    .expect("install uuid-ossp");

assert!(registry.is_installed("uuid-ossp"));
let _ = registry.lookup_function("uuid_generate_v4");
```
