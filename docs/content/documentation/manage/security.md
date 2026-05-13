---
title: Security
order: 59
---

# Security

AionDB v0.1 includes security controls for local evaluation and early server operation, but it is not yet presented as a production-hardened database.

## Security profiles

The configuration model has development, staging, and production-style profiles.

Development is permissive for local testing. Production-style settings require stronger password policy, TLS for password authentication, lockout, audit, session lifetime limits, and transaction idle limits.

Use development settings only for loopback evaluation. If the database is reachable from another machine, treat it as production-style from a security perspective even if the data is disposable.

| Context | Recommended posture |
| --- | --- |
| Local tutorial | loopback bind, bootstrap user, ephemeral storage |
| Driver test on one machine | loopback bind, explicit user/password, logs captured |
| Team evaluation network | TLS required, non-demo passwords, limited roles |
| Public internet | not recommended for v0.1 |

## Bootstrap user

For local development and benchmark harnesses:

```bash
AIONDB_BOOTSTRAP_USER=dev
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!'
aiondb --ephemeral
```

This creates a startup role for convenience. Do not use benchmark or demo credentials in long-running environments.

For repeatable tests, bootstrap an admin role, then create application roles through SQL. That separates startup convenience from the permissions your application actually uses.

## Password baseline

Production-style password policy expects:

- minimum length of 12;
- lowercase letter;
- uppercase letter;
- digit;
- symbol;
- password not equal to the role name.

Do not publish example passwords as real credentials. Documentation examples are intentionally disposable.

## TLS

Pgwire TLS mode:

```bash
AIONDB_PGWIRE_TLS_MODE=disable
AIONDB_PGWIRE_TLS_MODE=prefer
AIONDB_PGWIRE_TLS_MODE=require
```

TLS certificate variables:

```bash
AIONDB_PGWIRE_TLS_CERT_PATH=/path/server.crt
AIONDB_PGWIRE_TLS_KEY_PATH=/path/server.key
AIONDB_PGWIRE_TLS_CLIENT_CA_PATH=/path/client-ca.crt
```

Use `require` for password-based access outside local development.

The packaged systemd environment example sets `AIONDB_PGWIRE_TLS_MODE=require`
and leaves certificate paths commented until real files are installed. That is
intentional: a long-running service should fail closed rather than silently
reuse the permissive local compose profile.

If TLS setup fails, reduce the problem:

1. verify the server starts with TLS disabled on loopback;
2. verify certificate paths exist and are readable;
3. start with `AIONDB_PGWIRE_TLS_MODE=require`;
4. connect with a client `sslmode` that matches the setup.

## Storage encryption

AionDB does not claim native encryption at rest in v0.1. Persistent storage should be placed on encrypted storage such as LUKS or another filesystem-level encryption mechanism.

The override below is for development:

```bash
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true
```

## Public network exposure

Keep pgwire and observability endpoints bound to localhost for local evaluation. Do not expose an alpha database directly to the public internet.

Observability endpoints should be treated as internal operational endpoints. They may reveal process state, workload shape, or configuration details.

## Role model

Use separate roles for separate jobs:

```sql
CREATE ROLE app_reader LOGIN PASSWORD 'ReaderPass42!';
CREATE ROLE app_writer LOGIN PASSWORD 'WriterPass42!';
CREATE ROLE migration_user LOGIN PASSWORD 'MigrationPass42!';

GRANT SELECT ON items TO app_reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON items TO app_writer;
GRANT CREATE ON SCHEMA public TO migration_user;
```

Avoid running application traffic as a superuser. Superusers are for local administration and emergency debugging.

## Security evaluation checklist

Before a serious evaluation, check:

- pgwire bind address;
- observability bind address;
- TLS mode;
- bootstrap credentials;
- role privileges;
- storage encryption;
- password policy;
- audit/logging expectations;
- session and transaction timeout settings.

See [Administration](/documentation/manage/administration.html) for role and privilege examples.
