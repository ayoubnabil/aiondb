---
title: What's New in v0.2
order: 89
---

# What's New in v0.2

v0.2 is an evaluation-line release. It widens the graph engine, ships structured graph observability, freezes a versioned `EXPLAIN (FORMAT JSON)` contract, adds reproducible Neo4j-oriented compatibility evidence, and ties release evidence to concrete smoke artifacts.

> New in v0.2: experimental Neo4j-oriented Bolt compatibility evidence across the official Python, JavaScript, and Java drivers, plus `cypher-shell`, and a versioned `EXPLAIN JSON` contract for graph tooling.

For the current benchmark-facing snapshot, use
[v0.2 Performance Snapshot](/documentation/evaluate/v0-2-performance.html).

## Summary

Five changes:

1. graph execution and Cypher support are broader and better tested;
2. graph `EXPLAIN` output is now useful for diagnosis instead of raw plan text;
3. `EXPLAIN (FORMAT JSON)` and `EXPLAIN (ANALYZE, FORMAT JSON)` are explicit supported surfaces;
4. Neo4j-oriented compatibility moved from intent to reproducible evidence;
5. release evidence is tied to concrete smoke artifacts.

## Graph query surface

The graph surface is wider than the earlier alpha line.

Key improvements:

- broader native `CALL { ... }` support, including correlated forms and `UNION`;
- `EXISTS { ... }` lowering in the native pipeline;
- pattern comprehension support in the native path;
- better coverage for list comprehension, quantifier functions, map projection, and graph introspection functions;
- runtime fixes around graph binding compaction, property access, and path materialization.

This is not full Cypher. The supported subset is wider and less fragile.

Use:

- [Graph Reference](/documentation/query/graph-reference.html)
- [Graph and Vector](/documentation/query/graph-and-vector.html)
- [Limitations](/documentation/evaluate/limitations.html)

to see the exact supported surface and boundaries.

## Graph observability

Human-readable `EXPLAIN` output now includes:

- graph query summaries;
- clause-level and pattern-level access lines;
- seed and pivot selection details;
- join shape and correlated fanout risk;
- estimate drift and warning lines;
- graph summary severity and machine-readable summary metrics.

It distinguishes runtime-observed signals, plan-inferred signals, and mixed summaries. Use that during graph plan triage.

Use:

- [Observability](/documentation/manage/observability.html)
- [Explain JSON](/documentation/manage/explain-json.html)

to inspect the new contract and examples.

## `EXPLAIN (FORMAT JSON)`

The versioned explain payload is the headline v0.2 addition.

Supported forms:

```sql
EXPLAIN (FORMAT JSON) ...
EXPLAIN (ANALYZE, FORMAT JSON) ...
```

The payload now includes:

- a versioned top-level contract;
- `plan_overview`;
- `graph_summary`;
- `graph_detail`;
- `execution_summary`;
- explicit provenance fields such as `*_source`;
- stable helper APIs on the engine side.

This is not a cross-database format. It is an AionDB-native tooling contract. Use it for local tooling, UI, evaluation, and future planner feedback loops. Do not advertise it as PostgreSQL or Neo4j interop JSON.

See:

- [Explain JSON](/documentation/manage/explain-json.html)

for the field-level contract.

## Neo4j-oriented compatibility

v0.2 is the first release with concrete Neo4j-oriented evidence.

### Bolt compatibility

Reproducible grouped evidence covers the read-only Bolt compatibility surface for:

- Neo4j Python driver;
- Neo4j JavaScript driver;
- Neo4j Java driver;
- `cypher-shell`.

The grouped report is:

- `target/compat/neo4j-p0-smoke.json`

and the grouped wave is:

```bash
cargo run -q -p xtask -- ecosystem-compat --group neo4j-p0 --no-history --report target/compat/neo4j-p0-smoke.json
```

When the local provisioning inputs are present, that grouped wave passes end-to-end.

Current posture: experimental, read-only, tool-by-tool evidence. Not a broad "Neo4j ecosystem compatible" claim.

### Query API compatibility wrapper

The HTTP Query API compatibility wrapper has grouped evidence at `target/compat/neo4j-http-p1-smoke.json`. The grouped wave is part of the local smoke gate.

### Browser preflight

Browser-oriented Bolt preflight evidence lives at `target/compat/neo4j-browser-p0-smoke.json`. It proves the server-side preflight procedures a Browser-like client expects:

- `dbms.components`
- `db.labels`
- `db.relationshipTypes`
- `db.propertyKeys`

including projected `YIELD ... RETURN ...` forms.

This is not Browser UI validation. It is preflight evidence.

For the current matrix, limits, and commands, see [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html).

## Release evidence and smoke gates

The release process distinguishes hard local smoke gates, optional compatibility waves that depend on provisioned external tools, and grouped JSON artifacts reviewed as release evidence.

Important artifacts:

- `target/compat/neo4j-http-p1-smoke.json`
- `target/compat/neo4j-p0-smoke.json`
- `target/compat/neo4j-browser-p0-smoke.json`

`make product-smoke` now:

- always runs the HTTP compatibility wave;
- runs the Bolt P0 wave when the local Neo4j clients are provisioned;
- runs the Browser preflight wave when `AIONDB_CYPHER_SHELL` is provisioned.

The release evidence chain is now explicit.

See:

- [Release Process](/documentation/project/release-process.html)
- [Release Notes](/documentation/project/release-notes.html)
- [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html)

## What v0.2 still does not claim

v0.2 is not:

- full PostgreSQL compatibility;
- full Cypher compatibility;
- Neo4j Browser validation;
- Neo4j Bloom validation;
- a write-capable Neo4j Bolt compatibility claim;
- a production HA contract.

Frame it as: wider graph engine, wider graph observability, versioned explain contract, tool-by-tool compatibility evidence, conservative product claims.

## Suggested reading order

To review v0.2 quickly:

1. [What's New in v0.2](/documentation/project/whats-new-v0-2.html)
2. [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html)
3. [Explain JSON](/documentation/manage/explain-json.html)
4. [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html)
5. [Release Process](/documentation/project/release-process.html)
