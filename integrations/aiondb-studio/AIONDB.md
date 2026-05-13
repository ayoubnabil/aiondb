# AionDB Studio

AionDB Studio is the AionDB dashboard fork based on `pgweb`.

The old Rust `aiondb-dashboard` is now legacy. This directory is the product UI:
it talks to AionDB through PostgreSQL wire protocol and keeps the mature pgweb
features instead of rebuilding a database dashboard from scratch.

## What changed from upstream pgweb

- Branding changed from `pgweb` to `AionDB Studio`.
- Default connection form targets local AionDB pgwire:
  - host `127.0.0.1`
  - port `5432`
  - user `dev`
  - database `default`
  - SSL `disable`
- External GitHub release check is disabled.
- Environment prefix is `AIONDB_STUDIO_`, with `PGWEB_` still accepted.
- Query toolbar includes SQL/Cypher mode, AionDB snippets, and Graph Preview.
- Graph Preview renders query rows with `source_id`/`target_id`,
  `from`/`to`, or two scalar columns as edges.

## Run

Start AionDB pgwire separately, then run:

```sh
make dashboard-studio
```

Open:

```text
http://127.0.0.1:8082/
```

If AionDB pgwire is not running yet, the UI still loads and shows the connection
screen.

## License

The upstream pgweb code is MIT licensed. Keep `LICENSE` and upstream attribution
when distributing AionDB Studio.

AionDB-specific changes are distributed under AionDB's project license:
`BUSL-1.1` with the Apache License 2.0 change license, or a separate
commercial license.
