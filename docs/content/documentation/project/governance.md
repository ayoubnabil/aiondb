---
title: Governance
order: 91
---

# Governance

AionDB v0.1 uses lightweight governance. The goal is to make decisions and product claims inspectable without slowing down small fixes.

## Maintainer Responsibility

Maintainers are responsible for:

- keeping the default branch buildable;
- keeping release claims aligned with tests and documentation;
- reviewing changes that affect storage compatibility, security, licensing, or public APIs;
- requiring reproduction steps for compatibility and benchmark claims;
- refusing broad product claims that are not backed by repository artifacts.

## Evidence-Based Claims

Use this rule:

> If a claim cannot point to a command, test, doc page, fixture, release artifact, or reduced reproduction, treat it as roadmap language.

This applies to PostgreSQL compatibility, ecosystem integrations, benchmarks, storage upgrade safety, and operational readiness.

## Release Requirements

A release candidate should provide:

- passing CI gates;
- static documentation build;
- verified local archive from `make package-verify`;
- release notes with limitations;
- checksum and manifest for binary archives;
- explicit statement of worktree cleanliness.

## Security

Security issues follow the root `SECURITY.md` policy. Reports should include enough information to reproduce the issue: commit, server command, storage backend, data-dir state where relevant, SQL/client sequence, and expected impact.

## Commercial Licensing

AionDB core is licensed under BUSL-1.1 with an Apache License 2.0 change
license and a separate commercial license path. Governance changes do not
change license terms.
