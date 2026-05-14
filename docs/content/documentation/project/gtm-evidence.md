---
title: GTM Evidence
order: 92
---

# GTM Evidence

This page lists the evidence a technical buyer, investor, or evaluator should be able to inspect before taking AionDB seriously as a product.

## Minimum Public Evidence

| Evidence | Current artifact |
| --- | --- |
| One-command local start | `aiondb --ephemeral`, Docker compose |
| Standard client path | PostgreSQL wire protocol and `psql` smoke test |
| Integration story | pgAdmin profile and driver checklist |
| Honest limitations | Limitations, Tradeoffs, Roadmap |
| Operational story | `/livez`, `/readyz`, metrics, doctor, dump/restore |
| Distribution story | Dockerfile, compose profile, Kubernetes profile, verified local archive, systemd template |
| Benchmark discipline | benchmark reproducibility docs |
| Governance signal | license, contributing guide, governance, security policy, release process |

## What Still Needs Proof

- public releases with signed checksums and provenance;
- compatibility matrix generated from CI;
- example apps in more than one language;
- repeatable benchmark reports with raw output;
- public issue labels for compatibility, storage, docs, and benchmarks;
- signed release notes that separate engine features from product claims.

## Messaging Constraint

Use this positioning:

> AionDB is a source-available, PostgreSQL-wire, SQL-first database engine for evaluating unified relational, graph, and vector workloads on a single-node alpha.

Avoid these claims until the repository has evidence:

- managed cloud database;
- PostgreSQL replacement;
- production HA;
- complete graph database;
- complete vector database;
- production disaster recovery.
