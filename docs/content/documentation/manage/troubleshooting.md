---
title: Troubleshooting
order: 85
---

# Troubleshooting

This page covers common local evaluation problems.

Start with the smallest failing path. A connection failure, parser error, planner issue, and storage recovery issue need different evidence.

## Quick triage

| Symptom | First check |
| --- | --- |
| Server does not start | command, environment, storage directory, logs |
| `psql` cannot connect | bind address, port, credentials, TLS mode |
| Login fails | bootstrap variables, password policy, role existence |
| Query syntax fails | parser support and reduced SQL |
| Query is slow | release build, indexes, limits, query shape |
| Data missing after restart | storage mode and data directory |
| Driver fails but `psql` works | extended protocol, prepared statements, type mapping |

## The server refuses persistent storage

Persistent storage requires filesystem-level encryption unless you explicitly opt into unencrypted development storage.

For local evaluation:

```bash
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
aiondb --data-dir ./data/aiondb
```

For production-like testing, put the data directory on encrypted storage instead of using the override.

Also check that `--ephemeral` is not being passed accidentally. Ephemeral mode intentionally ignores persistent recovery expectations.

## psql cannot connect

Check the server bind address:

```bash
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:5432
```

Then connect:

```bash
psql "host=127.0.0.1 port=5432 dbname=default user=dev password=ReplaceWithLongUniquePassword42! sslmode=disable"
```

If TLS mode is `prefer` or `require`, make sure the client `sslmode` matches your certificate setup.

If the port is already in use, change the listen address:

```bash
AIONDB_PGWIRE_LISTEN_ADDR=127.0.0.1:15432
```

Then update the `psql` connection string to use the same port.

## Bootstrap login fails

Make sure both variables are set:

```bash
AIONDB_BOOTSTRAP_USER=dev
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!'
```

The password must satisfy the server baseline in stricter modes.

If a role already exists, confirm whether the server leaves it unchanged. Do not assume changing the environment password resets an existing role unless that behavior is explicitly documented and tested.

## A query returns a limit error

Development defaults include resource limits. For controlled evaluation, increase the relevant limit:

```bash
AIONDB_LIMITS_MAX_RESULT_ROWS=100000
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=0
```

Do this intentionally. Limits protect the alpha server from accidental runaway workloads.

Record any changed limit with benchmark output. A query that only works after raising limits may still need query or index work.

## A PostgreSQL feature is missing

AionDB v0.1 is not PostgreSQL-complete. Reduce the failure to the smallest SQL script possible and keep the expected PostgreSQL behavior beside it.

Useful report format:

```text
AionDB commit:
Client:
SQLSTATE:
Expected PostgreSQL behavior:
Actual AionDB behavior:
Reduced SQL:
```

## A graph or vector query does not plan as expected

Graph and vector planning are active alpha areas. Try the equivalent SQL join or brute-force vector query first, then compare results and plan shape.

## Data is missing after restart

Check whether the server was started with `--ephemeral`. If so, data loss after shutdown is expected.

For durable mode, record:

- data directory path;
- storage backend;
- server shutdown method;
- restart command;
- validation query.

Do not delete the data directory until the issue has been reduced or intentionally discarded.

## Driver fails but psql works

This usually means the driver uses a path that `psql` did not exercise:

- prepared statements;
- extended protocol;
- binary result formats;
- startup parameters;
- connection reset SQL;
- transaction state expectations.

Capture driver logs or wire-level query text when possible, then reduce the issue to SQL or protocol behavior.
