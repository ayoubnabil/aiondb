---
title: Operations
order: 60
---

# Operations

This page documents the operational posture of AionDB v0.1.

## Storage modes

AionDB supports in-memory and durable local profiles.

- `--ephemeral` uses in-memory storage and loses data on exit.
- `--data-dir <path>` uses persistent local state.
- `AIONDB_STORAGE_BACKEND` can select the backend used by the server.

Persistent storage should be placed on encrypted storage. The server refuses unsafe persistent setups unless an explicit development override is provided.

## Starting locally

For a disposable local run:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
cargo run -p aiondb-server --bin aiondb -- --ephemeral
```

For a persistent evaluation directory:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
cargo run -p aiondb-server --bin aiondb -- --data-dir ./data/aiondb
```

Keep persistent alpha directories disposable. The safer workflow is to keep schema and import scripts under version control so the directory can be rebuilt.

## Observability

The server exposes an HTTP observability endpoint for health and metrics.

Default shape:

```bash
AIONDB_OBSERVABILITY_BIND=127.0.0.1
AIONDB_OBSERVABILITY_PORT=9187
```

Endpoints include health and metrics surfaces. Keep observability bound to localhost unless you have explicitly secured the environment.

## Process supervision

For v0.1 evaluation, run AionDB under a simple supervisor only after direct command-line runs are understood. The supervisor should capture stdout, stderr, exit status, environment variables, and the exact binary path.

The repository ships a systemd starting point at
`packaging/systemd/aiondb.service` and an environment template at
`packaging/systemd/aiondb.env.example`. It also ships a single-node Kubernetes
evaluation profile at `packaging/kubernetes/aiondb.yaml`. The local package
archive includes these files. `make deployment-validate` checks that the
deployment profiles use explicit image tags, durable storage, readiness and
liveness probes, and conservative observability exposure. `make docker-validate`
remains a compatibility alias.

Operational reports should include:

- server command;
- environment variables that affect storage, pgwire, auth, and observability;
- data directory path;
- commit hash;
- client command;
- logs around failure time.

## Authentication bootstrap

For development and benchmark harnesses, the server can provision a bootstrap user:

```bash
AIONDB_BOOTSTRAP_USER=bench
AIONDB_BOOTSTRAP_PASSWORD='BenchAion42!'
```

Passwords must satisfy the server security baseline. Do not use benchmark bootstrap credentials for production-like environments.

## Upgrade posture

AionDB v0.1 keeps a separate storage format contract. Persistent SQL storage uses storage v1; graph, vector, LSM, distributed, and HA artifacts are not part of the stable storage v1 promise.

Before testing a newer binary against an older data directory:

```bash
aiondb doctor --data-dir ./data/aiondb
aiondb upgrade --data-dir ./data/aiondb
```

The server refuses to open a stable data directory that has no storage manifest. Upgrade creates a backup before writing and refuses ambiguous or corrupt state.

## Incident checklist

When a local evaluation fails:

1. save the server logs;
2. save the exact SQL or client operation;
3. record whether the server process exited;
4. check `/healthz` and `/info` if observability is still running;
5. preserve the data directory if the issue involves recovery or persistence;
6. reduce the failure to a smaller script.

## Current production posture

AionDB v0.1 does not claim a mature production operations story. Canonical SQL dump/restore is the supported safety path, but binary online backup, point-in-time recovery, high availability, online upgrades, long-running migration safety, and disaster recovery need workload-specific validation before use.

See [Storage Compatibility](/documentation/manage/storage-compatibility.html) and [Backup and Recovery](/documentation/manage/backup-and-recovery.html) for the current recovery guidance.
