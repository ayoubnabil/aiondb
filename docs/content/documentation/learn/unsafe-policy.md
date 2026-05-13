---
title: Unsafe Policy
order: 81
---

# Unsafe Policy

AionDB currently prefers safe Rust by default, even in parts of the engine where a more aggressive database implementation might choose low-level `unsafe` techniques.

This is an intentional product decision, not an accident.

## Current policy

The workspace denies `unsafe` by default. Small exceptions exist only where the gain is obvious and the blast radius is narrow, such as selected SIMD kernels, prefetch hints, or tightly scoped platform interop.

For v0.1, the default position is:

- prefer the simpler safe design if both versions are credible;
- accept some extra copies, parsing overhead, or synchronization cost;
- keep binary layout code explicit instead of pointer-casting through raw memory;
- keep the storage and WAL paths auditable during the alpha phase.

This makes the codebase easier to review, test, and evolve while SQL, graph, vector, storage, and recovery behavior are still moving.

## What this choice costs

Avoiding broad `unsafe` use is not free. In some hot paths, AionDB may pay in:

- extra CPU work for byte-by-byte decoding and encoding;
- temporary allocations or owned buffers where a zero-copy path could exist;
- more conservative synchronization than a lock-free design;
- ordinary file I/O where a more specialized engine might use `mmap`, `O_DIRECT`, or deeper async I/O integration;
- a lower performance ceiling on some storage-engine paths.

For the current stage of the project, that trade is acceptable. The project is still optimizing for correctness, inspectability, and architectural stability first.

## Future direction

A future performance-oriented variant may intentionally introduce roughly 30 additional `unsafe` blocks in carefully chosen areas of the engine.

That does not mean "unsafe everywhere". It means targeted use in places where the performance upside is real and the invariants can be documented and tested.

The most likely candidates are:

- page decoding and page layout access in the storage engine;
- WAL encode/decode and checksum-adjacent hot paths;
- selected zero-copy read paths;
- tighter vector and scan kernels;
- carefully isolated syscall or platform-specific I/O fast paths.

If that happens, the rules should remain strict:

- each `unsafe` region stays small and isolated;
- the public API around it remains safe;
- each block carries a `SAFETY:` explanation;
- benchmarks justify the complexity;
- tests cover the invariants the `unsafe` code relies on.

## Why not do it now

Right now AionDB is still an alpha database, not a fully stabilized storage engine chasing every last percentage point.

At this stage, broadening `unsafe` too early would make it easier to hide correctness bugs in precisely the subsystems that are still being validated:

- crash recovery;
- WAL durability rules;
- page layout evolution;
- binary protocol handling;
- hybrid SQL, graph, and vector execution paths.

The current position is straightforward:

- today: prefer safety and explicitness;
- later: add `unsafe` only where profiling proves it matters and the invariants are mature.
