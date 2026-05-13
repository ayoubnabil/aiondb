---
title: Advanced Specification
order: 4
---

# Advanced Specification

This section contains crate-level and implementation-oriented notes. It is intended for contributors and reviewers who need to understand how AionDB is organized internally.

The general product documentation lives in [Documentation](/documentation/). Start there if you want to run AionDB, connect to it, evaluate the query model, or run benchmarks.

## How to read this section

Each page describes one internal component. These pages are not a public stability promise. They are allowed to evolve as the implementation changes.

Use this section for:

- reviewing internal architecture;
- understanding ownership boundaries between crates;
- planning refactors;
- checking whether a feature belongs in parser, planner, optimizer, executor, storage, catalog, pgwire, or embedded API code.
