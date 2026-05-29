---
title: Roadmap
order: 89
---

# Roadmap

This roadmap describes product direction, not a release guarantee.

## v0.1 focus

- Make the project understandable to external readers.
- Keep the PostgreSQL wire path usable for real clients.
- Keep the embedded API aligned with the server engine.
- Preserve reproducible benchmarks.
- Document limitations clearly.
- Improve graph and vector examples.

The v0.1 goal is credibility, not completeness. A useful v0.1 should let a technical reader clone the repo, run the server, connect with a normal client, read honest limitations, and understand why the architecture exists.

## Near-term engineering themes

- Better PostgreSQL compatibility coverage for real drivers and ORMs.
- More explicit graph DDL for relationships backed by existing relational columns.
- More predictable vector index planning.
- Cleaner error reporting and SQLSTATE coverage.
- Smaller, clearer public docs and examples.
- Benchmark runs that can be reproduced from a fresh checkout.

## Compatibility direction

PostgreSQL compatibility should grow from real client behavior:

- `psql` smoke tests;
- prepared statements from common drivers;
- ORM introspection queries;
- transaction error behavior;
- command tags and SQLSTATE coverage;
- type mapping for supported values.

The project should avoid claiming "PostgreSQL compatible" without a compatibility matrix and reduced test cases.

## Graph and vector direction

The graph and vector roadmap should prioritize features that reduce application duplication:

- edge labels over existing relational columns;
- clearer graph reference examples;
- predictable behavior for nullable endpoints;
- vector index planning that can be explained;
- hybrid query examples where SQL filters, graph relationships, and vector scoring operate on one dataset.

Deep graph algorithms, distributed graph traversal, and large-scale vector serving are later concerns. The first milestone is a coherent single-node model.

## Operations direction

Operational work should become credible in layers:

- stable configuration;
- explicit storage and WAL behavior;
- backup and restore procedure;
- crash recovery tests;
- observability with documented metrics;
- upgrade path for disk format changes.

High availability should not be marketed before the single-node operational story is solid.

## Product and distribution direction

What AionDB ships should be easy to evaluate without reading crate internals:

- release archives with checksums;
- a local container profile;
- Linux service templates;
- pgAdmin and standard PostgreSQL client workflows;
- driver and ORM compatibility reports generated from tests;
- a control-plane readiness endpoint before any managed-cloud positioning;
- public GTM evidence that separates implemented features from roadmap items.

The first distribution milestone is a credible local product. Managed cloud work should come after the local operator path is boring and reproducible.

## Longer-term themes

- Stronger cost model across SQL, graph, and vector operators.
- More complete backup and recovery story.
- Upgrade-safe storage format.
- Production-oriented observability and operations.
- High-availability work only after the single-node contract is credible.

## Non-goals for v0.1

- Claiming full PostgreSQL compatibility.
- Claiming production HA.
- Claiming best-in-class performance on every workload.
- Hiding alpha limitations behind vague language.

See [Tradeoffs](/documentation/learn/tradeoffs.html) for the current architectural tradeoff summary.

## How to read this roadmap

This page is not a promise that every item will land in order. It is a statement of priorities. If a benchmark, compatibility report, or user workload shows that a lower-level issue blocks real evaluation, that issue should move ahead of broad feature work.
