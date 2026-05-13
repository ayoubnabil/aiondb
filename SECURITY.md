# Security Policy

AionDB v0.1 is an alpha database engine. Treat it as evaluation software unless your team has separately reviewed and accepted the risks.

## Supported Version

| Version | Security support |
| --- | --- |
| `0.1.x` | best-effort fixes for reproducible issues |

Older unreleased snapshots and local branches are not security-supported release lines.

## Reporting a Vulnerability

Do not publish exploit details before maintainers have had time to investigate. Send a private report to the maintainer email listed in `Cargo.toml`, or use the repository's private security advisory channel when available.

Include:

- affected commit or release;
- build and run command;
- storage backend and data-dir state if relevant;
- proof-of-concept SQL or client sequence;
- expected impact;
- whether the issue requires network access, authenticated access, or local filesystem access.

## Scope

Security-relevant areas include:

- authentication and authorization;
- pgwire protocol handling;
- SQL parsing and execution crashes;
- privilege escalation across roles or databases;
- data-dir corruption or unsafe upgrade behavior;
- TLS configuration;
- backup/restore integrity;
- dashboard and observability exposure.

## v0.1 Boundaries

AionDB v0.1 does not claim:

- production hardening;
- managed cloud isolation;
- production high availability;
- built-in encryption at rest;
- public internet exposure of observability endpoints.

Keep observability on loopback or behind trusted infrastructure. Use encrypted filesystems for persistent data during production-like testing.

## Disclosure

Maintainers should acknowledge credible reports, reproduce the issue, prepare a fix or mitigation, and document the affected surface in release notes. Severe issues should get a narrow hotfix rather than waiting for broad feature work.
