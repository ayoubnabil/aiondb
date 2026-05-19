---
title: What's New in v0.2
order: 89
---

# What's New in v0.2

v0.2 is the release where AionDB stops looking like a narrow demo surface and starts looking like a serious evaluation target for hybrid SQL, graph, and tooling workflows.

The headline is simple:

- stronger graph query coverage;
- structured graph observability;
- versioned `EXPLAIN (FORMAT JSON)`;
- real Neo4j-oriented compatibility evidence;
- clearer release evidence and smoke gates.

This page is the product-facing summary of what changed in v0.2.

> New in v0.2: AionDB now has experimental Neo4j-oriented Bolt compatibility evidence across the official Python, JavaScript, and Java drivers, plus `cypher-shell`, and a versioned `EXPLAIN JSON` contract for graph tooling.

## Summary

v0.2 adds five major product changes:

1. graph execution and Cypher support are broader and better tested;
2. graph `EXPLAIN` output is now useful for diagnosis instead of only raw plan text;
3. `EXPLAIN (FORMAT JSON)` and `EXPLAIN (ANALYZE, FORMAT JSON)` are now explicit supported surfaces;
4. Neo4j-oriented compatibility moved from vague intent to reproducible evidence;
5. release evidence is now tied to concrete smoke artifacts.

## Graph query surface

The graph surface in v0.2 is materially stronger than the earlier alpha line.

Key improvements include:

- broader native `CALL { ... }` support, including correlated forms and `UNION`;
- `EXISTS { ... }` lowering in the native pipeline;
- pattern comprehension support in the native path;
- better coverage for list comprehension, quantifier functions, map projection, and graph introspection functions;
- runtime fixes around graph binding compaction, property access, and path materialization.

This does not mean "full Cypher." It means the supported subset is now wider, better exercised, and less fragile.

Use:

- [Graph Reference](/documentation/query/graph-reference.html)
- [Graph and Vector](/documentation/query/graph-and-vector.html)
- [Limitations](/documentation/evaluate/limitations.html)

to see the exact supported surface and boundaries.

## Graph observability

v0.2 introduces a much more serious graph observability layer.

Human-readable `EXPLAIN` output now includes:

- graph query summaries;
- clause-level and pattern-level access lines;
- seed and pivot selection details;
- join shape and correlated fanout risk;
- estimate drift and warning lines;
- graph summary severity and machine-readable summary metrics.

It now also distinguishes between:

- runtime-observed signals;
- plan-inferred signals;
- mixed summaries that combine both.

This makes graph plan triage much more practical during evaluation.

Use:

- [Observability](/documentation/manage/observability.html)
- [Explain JSON](/documentation/manage/explain-json.html)

to inspect the new contract and examples.

## `EXPLAIN (FORMAT JSON)`

One of the most important v0.2 additions is the versioned explain payload.

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

This is not presented as a cross-database format. It is an AionDB-native tooling contract.

That distinction matters:

- it is good enough for local tooling, UI, evaluation, and future planner feedback loops;
- it should not be advertised as PostgreSQL or Neo4j interop JSON.

See:

- [Explain JSON](/documentation/manage/explain-json.html)

for the field-level contract.

## Neo4j-oriented compatibility

v0.2 is the first point where the Neo4j-oriented story becomes concrete.

### Bolt compatibility

AionDB now has reproducible grouped evidence for the read-only Bolt compatibility surface across:

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

When the local provisioning inputs are present, that grouped wave now passes end-to-end.

Current posture:

- experimental;
- read-only;
- explicit tool-by-tool evidence;
- not a broad "Neo4j ecosystem compatible" claim.

### Query API compatibility wrapper

The HTTP Query API compatibility wrapper also has grouped evidence now:

- `target/compat/neo4j-http-p1-smoke.json`

This grouped wave is already part of the local smoke gate.

### Browser preflight

v0.2 also adds Browser-oriented Bolt preflight evidence:

- `target/compat/neo4j-browser-p0-smoke.json`

This currently proves the server-side preflight procedures that a Browser-like client is likely to expect:

- `dbms.components`
- `db.labels`
- `db.relationshipTypes`
- `db.propertyKeys`

including projected `YIELD ... RETURN ...` forms.

This is still not Browser UI validation. It is preflight evidence only.

For the current matrix, limits, and commands, use:

- [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html)

## Release evidence and smoke gates

v0.2 also tightens the release story.

The release process now distinguishes:

- hard local smoke gates;
- optional compatibility waves that depend on provisioned external tools;
- grouped JSON artifacts reviewed as release evidence.

Important artifacts now include:

- `target/compat/neo4j-http-p1-smoke.json`
- `target/compat/neo4j-p0-smoke.json`
- `target/compat/neo4j-browser-p0-smoke.json`

`make product-smoke` now:

- always runs the HTTP compatibility wave;
- runs the Bolt P0 wave when the local Neo4j clients are provisioned;
- runs the Browser preflight wave when `AIONDB_CYPHER_SHELL` is provisioned.

This makes the release evidence chain more explicit and less hand-wavy.

See:

- [Release Process](/documentation/project/release-process.html)
- [Release Notes](/documentation/project/release-notes.html)
- [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html)

## What v0.2 still does not claim

Even with these improvements, v0.2 should still avoid broad claims.

v0.2 is **not** yet:

- full PostgreSQL compatibility;
- full Cypher compatibility;
- Neo4j Browser validation;
- Neo4j Bloom validation;
- write-capable Neo4j Bolt compatibility claim;
- production HA contract.

The right message is:

- stronger graph engine;
- stronger graph observability;
- stronger explain contract;
- stronger tool-by-tool compatibility evidence;
- still conservative product claims.

## Suggested reading order

If you want to review v0.2 quickly:

1. [What's New in v0.2](/documentation/project/whats-new-v0-2.html)
2. [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html)
3. [Explain JSON](/documentation/manage/explain-json.html)
4. [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html)
5. [Release Process](/documentation/project/release-process.html)
