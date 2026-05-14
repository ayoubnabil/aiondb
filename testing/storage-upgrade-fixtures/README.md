# Storage Upgrade Fixtures

This directory is the binary fixture root for storage compatibility tests.

Required fixture slots:

- `0.1/`
- `0.2/`
- `1.0/`
- `1.1/`

Each fixture directory must be a stopped AionDB data-dir, not a dump. Keep the
fixture small but complete:

- catalog with at least one schema and one SQL table;
- heap/page data with representative rows;
- at least one primary or ordered index;
- WAL requiring recovery on first open;
- no graph/vector/HA artifact unless the fixture is explicitly testing that an
  experimental artifact is detected and reported as non-stable.

CI upgrade checks should run:

```bash
cargo xtask storage-upgrade-matrix --strict-fixtures
```

The source fixture must never be modified in place by CI. Copy it to a scratch
directory before `upgrade`; the xtask command does this before running
`doctor -> upgrade -> doctor` for each fixture.
