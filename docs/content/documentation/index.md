---
title: Documentation
order: 3
---

# Documentation

AionDB is a PostgreSQL-wire database engine that keeps relational tables, graph labels, and vector search in one local system. These pages focus on using and evaluating the product. Implementation notes live separately.

> New in v0.2: graph observability is now much stronger, `EXPLAIN (FORMAT JSON)` is a supported versioned surface, and Neo4j-oriented compatibility now has real grouped smoke evidence. Start with [What's New in v0.2](/documentation/project/whats-new-v0-2.html).

Crate-by-crate implementation notes live in [Advanced Specification](/specification-avancee/). They are useful for contributors, but they are not the recommended starting point for users evaluating the database.

## Choose a path

| Goal | Start here |
| --- | --- |
| Install or run AionDB for the first time | [Installation](/documentation/start/installation.html), [Getting Started](/documentation/start/getting-started.html), then [Tutorial](/documentation/start/tutorial.html) |
| Understand the model | [Core Concepts](/documentation/learn/core-concepts.html), then [Architecture](/documentation/learn/architecture.html) |
| Test SQL behavior | [SQL](/documentation/query/sql.html), [SQL Statements](/documentation/query/sql-statements.html), [Data Types](/documentation/query/data-types.html) |
| Test graph/vector features | [Graph and Vector](/documentation/query/graph-and-vector.html), [Graph Reference](/documentation/query/graph-reference.html), [Vector Reference](/documentation/query/vector-reference.html) |
| Connect an app | [Interfaces](/documentation/connect/interfaces.html), [Client Drivers](/documentation/connect/client-drivers.html), [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html), [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html) |
| Evaluate seriously | [Evaluation Checklist](/documentation/evaluate/evaluation-checklist.html), [Product Hardening Plan](/documentation/evaluate/product-hardening-plan.html), [Limitations](/documentation/evaluate/limitations.html), [Benchmarks](/documentation/evaluate/benchmarks.html) |
| Review the v0.2 delta | [What's New in v0.2](/documentation/project/whats-new-v0-2.html), then [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html) |

## Status model

AionDB v0.1 is an alpha. The documentation uses conservative language on purpose:

- examples are intended to be runnable;
- unsupported behavior should be stated clearly;
- benchmark claims need reproduction details;
- PostgreSQL compatibility is feature-by-feature;
- graph and vector planning are still evolving.

If a page sounds too broad, prefer the narrower interpretation and validate with a small script.

## Start

- [Installation](/documentation/start/installation.html): build from source, create a local archive, run the container profile, and review the Kubernetes and systemd templates.
- [Getting Started](/documentation/start/getting-started.html): build the server, create a local user, connect with `psql`, and run the first query.
- [Tutorial](/documentation/start/tutorial.html): one small dataset using SQL, graph labels, and vector scoring.
- [Example Workloads](/documentation/start/example-workloads.html): practical schemas for product, support, and knowledge-base evaluations.

## Learn

- [Core Concepts](/documentation/learn/core-concepts.html): the mental model behind tables, labels, vectors, catalog state, and alpha boundaries.
- [Architecture](/documentation/learn/architecture.html): how the server, engine, catalog, storage, and WAL fit together.
- [Storage Format](/documentation/learn/storage-format.html) and [WAL Contract](/documentation/learn/wal-contract.html): the v0.2 storage and write-ahead-log contracts.
- [Query Lifecycle](/documentation/learn/query-lifecycle.html): what happens between a client query and execution.
- [Tradeoffs](/documentation/learn/tradeoffs.html): workloads where AionDB is a good fit, and workloads where it is not.

## Build

- [SQL](/documentation/query/sql.html), [SQL Statements](/documentation/query/sql-statements.html), [Data Types](/documentation/query/data-types.html), and [Functions](/documentation/query/functions.html): the relational surface.
- [Graph and Vector](/documentation/query/graph-and-vector.html), [Graph Reference](/documentation/query/graph-reference.html), and [Vector Reference](/documentation/query/vector-reference.html): hybrid query features over ordinary tables.
- [Indexes and Constraints](/documentation/query/indexes-and-constraints.html), [Transactions](/documentation/query/transactions.html), and [System Catalogs](/documentation/query/system-catalogs.html): behavior that affects application correctness.
- [Interfaces](/documentation/connect/interfaces.html), [Client Drivers](/documentation/connect/client-drivers.html), [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html), and [Ecosystem Integrations](/documentation/connect/ecosystem-integrations.html): connecting through pgwire, integrating normal SQL tools, or embedding the engine.

## Manage

- [Configuration](/documentation/manage/configuration.html): command-line flags, environment variables, and local data directories.
- [Administration](/documentation/manage/administration.html), [Control Plane](/documentation/manage/control-plane.html), [Operations](/documentation/manage/operations.html), and [Security](/documentation/manage/security.html): operating the server during evaluation.
- [Observability](/documentation/manage/observability.html), [Explain JSON](/documentation/manage/explain-json.html), [Storage Compatibility](/documentation/manage/storage-compatibility.html), [Backup and Recovery](/documentation/manage/backup-and-recovery.html), and [Troubleshooting](/documentation/manage/troubleshooting.html): diagnosing and recovering local deployments.

## Evaluate

- [Benchmarks](/documentation/evaluate/benchmarks.html), [Benchmark Results](/documentation/evaluate/benchmark-results.html), [Benchmark Reproducibility](/documentation/evaluate/benchmark-reproducibility.html), and [Performance Tuning](/documentation/evaluate/performance-tuning.html): running fair local comparisons and reading visual result snapshots.
- [v0.2 Evidence](/documentation/evaluate/v0-2-evidence.html): the storage, WAL, driver, type, and baseline benchmark evidence expected before broad v0.2 claims.
- [Testing](/documentation/evaluate/testing.html), [Evaluation Checklist](/documentation/evaluate/evaluation-checklist.html), [Product Hardening Plan](/documentation/evaluate/product-hardening-plan.html), and [Migration Guide](/documentation/evaluate/migration-guide.html): deciding whether a workload is ready to try.
- [Limitations](/documentation/evaluate/limitations.html), [Error Reference](/documentation/evaluate/error-reference.html), [FAQ](/documentation/evaluate/faq.html), and [Glossary](/documentation/evaluate/glossary.html): boundaries and terminology.

## Project

- [What's New in v0.2](/documentation/project/whats-new-v0-2.html), [Roadmap](/documentation/project/roadmap.html), [Roadmap to v1](/documentation/project/roadmap-v1.html), [Governance](/documentation/project/governance.html), [GTM Evidence](/documentation/project/gtm-evidence.html), [Release Notes](/documentation/project/release-notes.html), and [Release Process](/documentation/project/release-process.html): where the project is going, how decisions are made, what evidence supports claims, and how releases are described.
- [Contributing](/documentation/project/contributing.html): how to work on the codebase without starting from crate-level internals.

## Reading order for reviewers

For a fast technical review, read:

1. [Core Concepts](/documentation/learn/core-concepts.html)
2. [Tradeoffs](/documentation/learn/tradeoffs.html)
3. [Limitations](/documentation/evaluate/limitations.html)
4. [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html)
5. [Benchmarks](/documentation/evaluate/benchmarks.html)

That path answers the main credibility questions before going deep into reference pages.
