# Agent Rules

These rules are mandatory for any AI agent working on AionDB.

## Prime Directive

Do the smallest correct change.

Less code is better. Simple code is better. Existing patterns are better than
new abstractions. Do not make the project bigger unless the problem clearly
requires it.

## Before Writing Code

1. Read the relevant code first.
2. Identify the ownership boundary of the change.
3. Build a short mental map of the code path:
   - entry points;
   - core types;
   - state transitions;
   - error paths;
   - tests that already cover the area.
4. Check how similar problems are already solved inside AionDB.
5. For public behavior, compatibility, database semantics, protocols, storage,
   security, or UI conventions, check how mature competitors solve the same
   problem before designing the change.

Do not implement from vibes. Do not invent a new local style while an existing
one is present.

## Model Selection

Use the strongest practical model for non-trivial work.

Preferred default:

- `gpt-5.5` with reasoning effort `high`.

Acceptable alternative when the preferred model is unavailable:

- `Opus 4.7`.

Use cheaper or faster models only for narrow mechanical tasks, such as simple
formatting, small searches, or low-risk summaries. Architecture, storage,
query semantics, protocol compatibility, security, licensing, and product
decisions should use `gpt-5.5 high` unless there is a clear reason not to.

## Competitor Research Rule

When the task touches behavior that users can compare against another database,
tool, or ecosystem, inspect the comparable implementation or documentation
first.

Examples:

- PostgreSQL behavior for SQL, pgwire, catalog, error codes, transactions,
  privileges, COPY, prepared statements, data types, and planner behavior.
- Neo4j/openCypher behavior for Cypher and graph semantics.
- pgAdmin, pgweb, DBeaver, CloudBeaver, or psql behavior for dashboard and
  client UX.
- SQLite, RocksDB, FoundationDB, PostgreSQL, or DuckDB patterns for storage,
  recovery, WAL, durability, and testing.

Use primary sources when possible: upstream code, official docs, specs, tests,
or protocol references.

If network access is unavailable, use local source, vendored specs, tests,
existing docs, and clearly state the limitation.

## Code Quality Rules

- Prefer deletion over addition.
- Prefer a direct function over a framework.
- Prefer a small patch over a redesign.
- Prefer explicit errors over silent fallback.
- Prefer typed structures over ad hoc strings.
- Prefer existing helper APIs over new helpers.
- Avoid speculative abstractions.
- Avoid broad refactors while fixing a narrow bug.
- Avoid new dependencies unless they remove more risk than they add.
- Do not duplicate behavior that already exists elsewhere in the repository.
- Prefer losing performance over introducing `unsafe`.

Every new abstraction must pay rent immediately.

## Change Scope

Keep the write set tight.

Before editing, know which files you intend to touch and why. If you discover a
larger issue, stop and reassess instead of expanding the patch silently.

Never revert unrelated user work. The worktree may be dirty. Treat unrelated
changes as owned by someone else.

## Correctness Rules

Database code must be fail-fast and deterministic.

- Do not silently coerce invalid data.
- Do not turn correctness errors into `NULL`.
- Preserve SQLSTATE/error semantics where compatibility expects them.
- Preserve transaction, durability, and authorization boundaries.
- Make recovery paths explicit.
- Make limits explicit.
- Make unsupported behavior explicit.

Compatibility is a feature only when it is tested.

## Testing Rules

Add or update tests when behavior changes.

Use the narrowest test that proves the change:

- unit test for local pure logic;
- integration test for cross-module behavior;
- pgwire/ecosystem test for client-visible protocol behavior;
- recovery/WAL test for durable state;
- documentation smoke path for user-facing commands.

If a test cannot be run, say exactly why.

## Documentation Rules

Update docs only when public behavior, commands, compatibility, deployment,
licensing, or user expectations change.

Documentation must be concrete:

- exact command;
- exact file;
- exact limitation;
- exact expected behavior.

Avoid marketing copy in technical docs.

## UI Rules

For dashboards and tools, do not build toy UI when a mature open-source UI can
be adapted through pgwire.

Use existing PostgreSQL-compatible tooling as the base when possible, then add
AionDB-specific extensions for:

- Cypher;
- graph preview;
- vector/graph examples;
- AionDB capability metadata;
- compatibility diagnostics.

## Security Rules

Security-sensitive code requires extra restraint.

- Prefer slower safe Rust over faster `unsafe` code.
- `unsafe` is forbidden unless there is a documented, reviewed, and tested
  justification that no safe design can satisfy.
- No silent auth bypass.
- No debug secrets in logs.
- No broad CORS or remote bind by default.
- No unsafe defaults for production paths.
- No weakening crypto, session, or privilege checks for convenience.

When unsure, choose the safer behavior and document the tradeoff.

## Final Response Rules

Report only what matters:

- files changed;
- behavior changed;
- tests run;
- tests not run and why;
- remaining risk.

Do not over-explain. Do not pretend uncertainty is certainty.
