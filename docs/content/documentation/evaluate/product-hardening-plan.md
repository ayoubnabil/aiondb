---
title: Product Hardening Plan
order: 83
---

# Product Hardening Plan

This plan converts broad evaluation concerns into repository-level work. It is intentionally strict: a database project should be easy to build, easy to start, honest about limits, and reproducible under review.

## Done in v0.1 Surface

| Concern | Repository artifact |
| --- | --- |
| First-run server | `README.md`, `aiondb --help`, `--ephemeral` |
| PostgreSQL-facing access | pgwire server, `psql` smoke path, compatibility docs |
| Storage compatibility | `aiondb.storage`, `aiondb doctor`, `aiondb upgrade` |
| Logical recovery | `aiondb dump`, `aiondb restore`, backup docs |
| Local dashboard | `aiondb-dashboard`, product info API |
| pgAdmin integration | `integrations/pgadmin/` and `make dashboard-pgadmin` |
| Container packaging | root `Dockerfile` and `docker-compose.yml` |
| Linux service template | `packaging/systemd/aiondb.service` |
| Benchmark policy | benchmark reproducibility docs and `benchmarks/run.sh` |
| Product-surface CI | GitHub Actions job for static docs and verified local archive |
| Local product smoke | `make product-smoke` for docs, release bundle, storage, observability, CLI dump/restore, deployment profiles, format, and check |

## Remaining Hardening Axes

| Axis | Next concrete artifact |
| --- | --- |
| Release provenance | signed archives, signed container images, and published provenance attestations |
| Driver confidence | CI smoke tests for Rust, Python, Node, and Java PostgreSQL drivers |
| ORM confidence | reduced Diesel, SQLAlchemy, Prisma, and ActiveRecord introspection suites |
| Cloud readiness | readiness endpoint and operator API before any hosted claims |
| Upgrade confidence | populated historical storage fixtures for every release line |
| Product trust | public compatibility matrix generated from tests |
| Benchmark trust | benchmark reports published with raw output and commit hash |

## Review Rule

If a claim cannot point to a command, a test, a doc page, or a fixture, treat it as roadmap language. This keeps marketing, engineering, and investor-facing material aligned with the actual product surface.
