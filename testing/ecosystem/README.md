This directory holds live ecosystem compatibility harnesses and a few manual
comparison tools.

Primary harnesses kept on purpose:
- `python/sqlalchemy_orm_compat.py`
- `python/alembic_autogen_compat.py`
- `python/django_orm_compat.py`
- `python/psycopg_smoke.py`
- `prisma/prisma_orm_compat.mjs`
- `typeorm/typeorm_orm_compat.mjs`
- `typeorm/typeorm_migrations_compat.mjs`
- `typeorm/typeorm_schema_diff_compat.mjs`
- `sequelize/sequelize_orm_compat.mjs`
- `sequelize/sequelize_alter_compat.mjs`
- `knex/knex_orm_compat.mjs`
- `objection/objection_orm_compat.mjs`
- `mikroorm/mikroorm_compat.mjs`
- `node/node_postgres_smoke.mjs`
- `diesel-smoke/src/main.rs`
- `psql/smoke.sql`
- `psql/undefined_table.sql`

Currently wired directly into `xtask`:
- `python/psycopg_smoke.py`
- `python/sqlalchemy_orm_compat.py`
- `node/node_postgres_smoke.mjs`
- `diesel-smoke/src/main.rs`
- `psql/smoke.sql`
- `psql/undefined_table.sql`

Manual tools kept for ad hoc comparison or deeper investigation:
- `rust-postgres-binary/`
- `aiondb-turso-embedded-bench/`

Local generated outputs under this tree should stay untracked. In particular,
`tmp/`, `target/`, and `node_modules/` are disposable.
