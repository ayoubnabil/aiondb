# Ultra Compare

Composite benchmark orchestrator for:

- `neo4j-graph`: AionDB vs Neo4j on graph/Cypher traversal shapes.
- `surreal-graph`: AionDB vs SurrealDB on end-to-end graph traversal shapes.
- `surreal-suite`: AionDB vs SurrealDB vs PostgreSQL-stack on the existing CRUD / scan / graph / vector matrix.

The goal is not to fake a single universal winner number. The goal is to run a
long, reproducible comparison pass with one `run_id`, one report directory, and
explicit comparability boundaries between workload families.

## Smoke

```bash
python3 benchmarks/ultra-compare/run.py --dry-run
```

## Long run

```bash
benchmarks/run.sh ultra-compare
```

The consolidated report is written under:

```text
target/benchmarks/ultra-compare/<run-id>/
```

with:

- `report.json`
- per-component stdout/stderr logs
- nested artifact directories for the underlying benchmark families
