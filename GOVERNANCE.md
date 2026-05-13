# AionDB Governance

AionDB v0.1 is an alpha project with a small maintainer surface. The governance goal for this release line is not bureaucracy; it is making project decisions, product claims, and release evidence inspectable.

## Maintainer Responsibility

Maintainers are responsible for:

- keeping the default branch buildable;
- keeping release claims aligned with tests and documentation;
- reviewing changes that affect storage compatibility, security, licensing, or public APIs;
- requiring reproduction steps for compatibility and benchmark claims;
- refusing broad product claims that are not backed by repository artifacts.

## Decision Rules

Use the smallest process that leaves an audit trail:

- ordinary bug fixes and docs changes can be reviewed in pull requests;
- storage format, license, security, public protocol, and release-process changes need an explicit rationale in the PR description or an ADR;
- benchmark claims need raw output, hardware context, commit hash, and command lines;
- compatibility claims need a reduced test or documented smoke path.

## Release Evidence

A release candidate should provide:

- passing CI gates;
- static documentation build;
- verified local archive from `make package-verify`;
- release notes with limitations;
- checksum and manifest for binary archives;
- explicit statement of worktree cleanliness.

## Security and Compatibility

Security issues follow [SECURITY.md](SECURITY.md). PostgreSQL compatibility and ecosystem support should be treated as evidence-based claims: a driver or tool is supported only when its smoke path is reproducible.

## Commercial Licensing

AionDB core is licensed under BUSL-1.1 with an Apache License 2.0 change
license and a separate commercial license path. Governance changes do not
change the license terms; license changes must be explicit release artifacts.
