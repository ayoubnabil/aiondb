---
title: Evaluation Checklist
order: 82
---

# Evaluation Checklist

Use this checklist before publishing performance claims, comparing AionDB to another database, or trying a serious prototype.

The checklist is meant to produce a decision, not just a pass/fail feeling. At the end, you should know whether the workload is ready to continue, blocked by a missing feature, or better suited to another database today.

## Basic server

- Build the release binary.
- Run `aiondb --help`.
- Start with `--ephemeral`.
- Connect with `psql`.
- Run basic DDL, INSERT, SELECT, UPDATE, DELETE.
- Restart in durable mode and verify data survives.

Decision point: if this section fails, do not continue to graph, vector, or benchmark work. Fix first-run behavior before deeper evaluation.

## SQL behavior

- Test your schema.
- Test your generated ORM SQL.
- Test transactions and rollback.
- Test prepared statements.
- Test the data types your application needs.
- Reduce every mismatch to a standalone SQL file.

Minimum artifact:

```text
schema.sql
seed.sql
queries.sql
expected-output.txt
```

Those four files make the evaluation portable.

## Graph and vector

- Create node labels over existing tables.
- Create edge labels over relationship tables.
- Run the same relationship query as SQL joins and as graph patterns where supported.
- Run one bounded variable-length path query and record the expected path count.
- Run `shortestPath` and `allShortestPaths` on a small fixture where the expected paths are obvious.
- Run at least one `CALL graph.*` procedure and pin the yielded columns.
- Save `EXPLAIN` output for graph `MATCH` and graph procedure calls.
- Insert vector columns with the intended dimension.
- Run brute-force vector distance queries.
- Add HNSW indexes only after correctness is clear.

Decision point: if graph/vector behavior is promising but incomplete, decide whether SQL fallbacks are acceptable. For alpha releases, a working SQL fallback is part of a credible adoption path.

## Driver compatibility

- Test connection startup.
- Test simple queries.
- Test prepared statements.
- Test transaction errors.
- Test connection pooling.
- Test binary and text result modes if the driver exposes them.

Record driver name and version. Driver behavior changes across versions, especially around prepared statements, type formats, and startup parameters.

## Benchmarks

- Record commit hash.
- Record hardware and OS.
- Record dataset size.
- Record durability settings.
- Record timeout and memory limits.
- Keep raw output.
- Compare correctness before comparing latency.

Minimum benchmark disclosure:

| Field | Required |
| --- | --- |
| Commit | yes |
| Build command | yes |
| Query text | yes |
| Dataset size | yes |
| Hardware | yes |
| Durability mode | yes |
| Protocol path | yes |
| Raw output | yes |

## Release readiness

- v0.2 evidence checklist has been reviewed.
- README explains the project in under one minute.
- Documentation has a working tutorial.
- Installation docs cover source, local archive, container, and service template paths.
- Limitations are public.
- License is clear.
- `cargo check --workspace` passes on a clean checkout.
- Benchmarks are reproducible.
- Storage doctor and dump/restore have a documented smoke test.
- PostgreSQL client and pgAdmin integration paths are documented.
- Control-plane claims are limited to health, metrics, info, doctor, upgrade, dump, and restore unless more is implemented.
- The codebase has no obvious generated/debug artifacts in the public surface.
- Roadmap and limitations are explicit.
- Release process has been followed before tagging.

## Evaluation outcomes

Use one of these outcomes:

- Continue: first-run path works, core workload runs, and limitations are acceptable.
- Continue with blockers: the model is interesting, but one or more missing features must be fixed before public claims.
- Stop for now: the workload depends on features AionDB does not implement yet.
- Use another database: a mature existing system is clearly better for this workload today.

An honest stop is useful. It prevents the project from being judged against a workload it is not ready to serve.
