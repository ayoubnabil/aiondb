---
title: Release Notes
order: 90
---

# Release Notes

> New in v0.2: for the product-facing delta instead of the raw alpha release framing, start with [What's New in v0.2](/documentation/project/whats-new-v0-2.html).

## v0.1 alpha

AionDB v0.1 is the first public alpha line. It is intended for evaluation, inspection, local benchmarking, and early feedback.

The release should be judged as a technical preview. It is useful if a reader can understand the model, run the database locally, connect through a PostgreSQL client, and reproduce the examples. It should not be presented as a mature production database.

## Included surfaces

- PostgreSQL wire server.
- Embedded Rust API.
- SQL parser, planner, optimizer, executor, catalog, storage, transaction, and WAL path.
- Vector column type and distance functions.
- HNSW index DDL path for vector columns.
- Graph node and edge label DDL over relational tables.
- Local benchmark harnesses.
- Static documentation site.
- Local container profile with pgwire exposed and observability kept on loopback.
- Verified local archive with checksum and manifest.
- Offline storage doctor and upgrade command.
- Canonical SQL dump/restore command path.
- Observability endpoints: `/livez`, `/healthz`, `/readyz`, `/metrics`, `/info`.
- Structured query explain output through `EXPLAIN (FORMAT JSON)` and `EXPLAIN (ANALYZE, FORMAT JSON)`, including graph summary and graph detail payloads.
- Experimental Neo4j Query API compatibility wrapper with grouped `neo4j-http-p1` smoke evidence.
- Experimental Neo4j-oriented Bolt compatibility listener with grouped `neo4j-p0` review evidence.
- Security and governance policy documents.

## Status by surface

| Surface | v0.1 status |
| --- | --- |
| Server startup | Available for local evaluation. |
| Pgwire clients | Usable for supported SQL paths; driver behavior must be tested. |
| Embedded Rust API | Available for in-process evaluation. |
| SQL | Practical subset, not full PostgreSQL. |
| Graph labels | Available for evaluation; validate exact workload behavior. |
| Vector functions | Available for fixed-dimension vectors. |
| Vector indexes | HNSW DDL path available; benchmark exact workload. |
| Durable storage | Available for local evaluation; alpha format. |
| Storage compatibility | Storage v1 manifest, doctor, and upgrade tooling are available. |
| Logical backup | SQL dump/restore path is available for v0.1 evaluation. |
| Observability | Local HTTP health, readiness, metrics, and product info endpoints are available. |
| Explain JSON | Available for AionDB-native tooling and evaluation; treat the payload as versioned AionDB-specific contract, not a cross-database format. |
| Neo4j Query API wrapper | Experimental HTTP compatibility surface with grouped smoke evidence; not a Bolt or official Neo4j driver claim. |
| Neo4j Bolt compatibility | Experimental read-only compatibility listener with grouped review evidence; the current P0 tool wave can pass end-to-end when the local JavaScript, Java, and `cypher-shell` clients are provisioned, and `make product-smoke` will run that optional wave automatically in that environment. |
| Neo4j Browser preflight | Browser-oriented Bolt procedure preflight evidence is available as a separate grouped smoke; treat it as server-side readiness evidence, not Browser UI validation. |
| Packaging | Local archive, checksum, manifest, prebuilt GHCR images, Dockerfile, compose profile, Kubernetes profile, and systemd template are available. |
| HA/distributed operation | Not a public production contract. |

## Suggested first run

Start with the Docker quickstart:

```bash
cp .env.example .env
$EDITOR .env
docker compose --profile studio up
```

Open AionDB Studio at `http://127.0.0.1:8082`, then connect with `psql` if you
also want terminal access:

```bash
source .env
PGPASSWORD="$AIONDB_BOOTSTRAP_PASSWORD" \
psql "host=127.0.0.1 port=${AIONDB_PGWIRE_PORT:-5432} dbname=default user=$AIONDB_BOOTSTRAP_USER sslmode=disable"
```

Run the smoke SQL file first, then try the tutorial schema, one SQL join, one graph label example, and one vector distance query.

## License

AionDB core is licensed under BUSL-1.1 with an Apache License 2.0 change
license and a separate commercial license path.
Third-party components keep their original licenses and notices.

## Known limits

- Alpha on-disk format.
- Incomplete PostgreSQL compatibility.
- Graph and vector workloads still need workload-specific validation.
- No production high-availability contract.
- No online binary backup, point-in-time recovery, or managed backup contract.
- Performance characteristics still moving quickly.

Internal testing, fuzzing, and compatibility validation are encouraging. That is not yet the same thing as a public production-ready claim.

AionDB will only claim production readiness after at least one month of continuous testing and fuzzing on the release line being shipped.

## Compatibility notes

The release is PostgreSQL-facing because it speaks pgwire and implements PostgreSQL-compatible behavior where supported. It is not PostgreSQL-complete. Test application drivers, prepared statements, generated ORM SQL, type mapping, and catalog introspection before making compatibility claims.

## Benchmark notes

Any public number should include commit hash, build command, benchmark command, dataset size, hardware, durability mode, protocol path, and raw output. Numbers without those details should be treated as local observations only.

## Upgrade policy

Storage v1 directories should be inspected with:

```bash
aiondb doctor --data-dir ./data/aiondb
```

If doctor reports an upgrade path, run:

```bash
aiondb upgrade --data-dir ./data/aiondb
```

Keep test data reproducible from SQL or fixture files. Before production-like testing, also keep a canonical SQL export:

```bash
aiondb dump --data-dir ./data/aiondb --output pre-upgrade.sql
```

Binary online backup and point-in-time recovery are not v0.1 release claims.

## Release artifact checks

For a local release candidate:

```bash
make product-smoke
```

This checks formatting, workspace compilation, storage compatibility, CLI dump/restore, observability routes, the grouped `neo4j-http-p1` Query API compatibility smoke, documentation links, package contents, package checksum/manifest, and the static deployment profiles.

Bolt-oriented Neo4j ecosystem evidence is still reviewed separately through
`target/compat/neo4j-p0-smoke.json`. At the current maturity level, that report
should reach `passing` when the local JavaScript driver, Java driver, and
`cypher-shell` provisioning inputs are present. If some of those local clients
are intentionally absent, a clearly explained `partial` report is still
acceptable review evidence.

The local archive manifest records:

- archive name;
- `aiondb --version`;
- git commit when available;
- archive path;
- checksum file path;
- inline SHA256;
- worktree dirty status.

## Feedback wanted

The most valuable feedback for v0.1 is concrete:

- first-run failures;
- driver compatibility issues;
- SQL scripts that should work but do not;
- graph or vector examples that are unclear;
- benchmark commands that cannot be reproduced;
- documentation pages that overclaim or under-explain behavior.

Reports should include commit hash, command, SQL text, expected result, actual result, and client driver when relevant.

## Recommended release review

Before presenting v0.1 publicly, check:

- quickstart works from a clean checkout;
- tutorial runs on the current binary;
- `make product-smoke` succeeds;
- `target/compat/neo4j-http-p1-smoke.json` shows `group_status = "passing"` for the current release candidate;
- `target/compat/neo4j-p0-smoke.json` is reviewed as evidence, with no tool-level overclaim beyond the suites that actually passed;
- license page matches repository license;
- security and governance policies are current;
- benchmark page does not overclaim;
- limitations page is current;
- release notes describe alpha status plainly.
