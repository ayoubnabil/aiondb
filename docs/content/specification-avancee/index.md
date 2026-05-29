---
title: Advanced Specification
order: 4
---

# Advanced Specification

Crate-level notes for contributors and reviewers. If you want to run AionDB, connect to it, or read benchmarks, go to [Documentation](/documentation/) instead.

## How to read this section

One page per internal component. None of it is a stability promise. Pages change when the code changes.

Use these notes to review internal architecture, check ownership boundaries between crates, plan refactors, or work out whether a change belongs in the parser, planner, optimizer, executor, storage, catalog, pgwire, or embedded API.
