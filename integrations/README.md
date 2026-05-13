# AionDB Integrations

The integration strategy is PostgreSQL wire first. AionDB-specific graph and vector workflows should be additive, not a reason ordinary SQL tools fail.

## Current Integrations

| Integration | Path | Purpose |
| --- | --- | --- |
| psql | `integrations/psql-smoke.sql` | minimal SQL, insert, select, and transaction smoke over pgwire |
| pgAdmin 4 | `integrations/pgadmin/` | SQL administration over pgwire |

Run the psql smoke after starting AionDB:

```bash
psql "host=127.0.0.1 port=5432 dbname=default user=dev password=DevPassword42! sslmode=disable" \
  -f integrations/psql-smoke.sql
```

## Adding an Integration

Every integration should include:

- a README with the exact server command;
- dependency versions;
- connection string shape without secrets;
- simple query smoke test;
- prepared query smoke test when applicable;
- transaction error test when applicable;
- known limitations.

Do not claim support until the integration has a reproducible smoke test.
