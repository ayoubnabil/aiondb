---
title: Limitations
order: 80
---

# Limitations

AionDB v0.1 is an alpha. This page is part of the product contract: it is better to state limits clearly than to let users discover them through failed migrations.

## Not a PostgreSQL replacement

AionDB speaks PostgreSQL wire protocol and implements PostgreSQL-compatible behavior where supported. It does not implement the full PostgreSQL language, system catalog, extension ecosystem, planner behavior, or operational maturity.

Validate exact behavior before using PostgreSQL tooling that depends on advanced protocol or catalog details. ORMs, migration frameworks, connection pools, and database administration tools often issue SQL that application developers never write manually.

Examples to test explicitly:

- prepared statements and extended query flow;
- generated migrations;
- transaction error recovery;
- type mapping for timestamps, arrays, JSON-like values, numeric values, and vectors;
- catalog introspection queries used by the driver or framework.

## Not production-ready by default

The v0.1 release is for evaluation. Treat production usage as experimental unless you have validated your own workload, durability expectations, backup strategy, failure behavior, and driver stack.

Do not use v0.1 as the only copy of important data. Keep reproducible imports, fixtures, or upstream source data so a test environment can be rebuilt from scratch.

Production readiness is not one feature. It requires a complete story for installation, upgrades, backups, monitoring, security, operational incidents, and long-term compatibility. v0.1 does not claim that full story.

Internal testing, fuzzing, and compatibility validation are progressing, but that is still not enough for a public production-ready claim.

AionDB will only claim production readiness after at least one month of continuous testing and fuzzing with the release line under evaluation.

## On-disk format may change

The storage format and catalog format may change before a stable release. Do not assume long-term upgrade compatibility for alpha data directories.

For alpha evaluations, prefer one of these workflows:

- ephemeral mode for demos and compatibility tests;
- disposable persistent directories for benchmark runs;
- migration scripts that can recreate schema and data after a clean checkout.

## Graph and vector need workload validation

Graph labels, edge label mapping, vector operators, vector indexing, and hybrid planning should be validated on the exact workload you care about. Syntax and planner behavior may still change across alpha releases.

The safest way to use graph and vector features in v0.1 is to keep an equivalent relational SQL query nearby. That gives you a correctness reference when graph syntax, traversal planning, or vector index behavior changes.

Deep traversals, variable-length paths, filtered vector search, and mixed graph/vector planning should be treated as areas to benchmark and validate carefully. They are exactly the areas where a multi-model engine needs the most optimizer work.

## Distributed and HA work is not the v0.1 contract

Some internal modules exist for clustering, transport, high availability, or distributed execution. Their presence does not mean the public product is a production distributed database in v0.1.

Do not build a public availability claim from module names. A production HA claim needs documented replication mode, failure behavior, recovery procedure, monitoring, and tests.

## Benchmark claims must be reproducible

Performance varies by workload. Any comparison to another database should include the exact commands, dataset, commit hash, hardware, and configuration.

Avoid claims such as "faster than database X" without disclosing protocol path and durability settings. A local embedded call, pgwire TCP query, HTTP query, WebSocket query, and batched prepared statement are different measurements.

## Good v0.1 use cases

Use v0.1 for:

- evaluating the SQL/graph/vector model;
- testing PostgreSQL driver compatibility;
- reproducing benchmark claims locally;
- inspecting the architecture;
- building demos where data can be recreated.

Avoid v0.1 for:

- primary production storage;
- compliance-sensitive workloads;
- long-lived alpha data directories;
- unbounded public multi-tenant workloads;
- workloads that require full PostgreSQL behavior.

## Unsupported does not mean impossible

Some limitations are temporary engineering work; others are deliberate scope decisions. When reporting a limitation, include the workload impact. A missing feature that blocks a real driver or tutorial path should be prioritized differently from an obscure PostgreSQL corner case.
