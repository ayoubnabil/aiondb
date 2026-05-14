---
title: Administration
order: 60
---

# Administration

This page covers common administrative SQL for local evaluation and early server operation.

Administration in v0.1 is intentionally scoped. The goal is to support local evaluation, driver tests, role behavior, and basic privilege checks without pretending the system has the full operational maturity of PostgreSQL.

## Roles

Create roles:

```sql
CREATE ROLE reader;
CREATE ROLE app_user WITH LOGIN;
CREATE ROLE secure_user LOGIN PASSWORD 'StrongPassword42!';
CREATE ROLE admin_user SUPERUSER LOGIN;
```

Drop roles:

```sql
DROP ROLE reader;
```

Role names are treated case-insensitively for duplicate detection in the implemented role lifecycle.

Recommended local pattern:

```sql
CREATE ROLE app_reader LOGIN PASSWORD 'ReaderPass42!';
CREATE ROLE app_writer LOGIN PASSWORD 'WriterPass42!';
CREATE ROLE app_admin SUPERUSER LOGIN PASSWORD 'AdminPass42!';
```

Use separate roles even in tests. It exposes authorization bugs earlier than running every query as a superuser.

## Table privileges

Grant privileges:

```sql
GRANT SELECT ON items TO reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON items TO app_user;
GRANT ALL PRIVILEGES ON items TO admin_user;
GRANT SELECT ON TABLE items TO reader;
```

Revoke privileges:

```sql
REVOKE SELECT ON items FROM reader;
REVOKE ALL PRIVILEGES ON items FROM app_user;
```

Granting the same privilege more than once is intended to be idempotent.

Privilege checks should be tested with real sessions. Creating a role and granting a privilege is not enough; reconnect as that role and run the expected query.

## Schema privileges

```sql
GRANT CREATE ON SCHEMA public TO app_user;
```

Schema privileges are useful when testing migration tools or application users that create tables.

Migration tools often need broader privileges than runtime application users. Keep those roles separate:

- migration role: creates and changes schema;
- application writer: reads and writes application tables;
- application reader: reads only;
- admin role: local evaluation and emergency debugging.

## Superusers

Superuser-style roles bypass ordinary table grants in the implemented authorization paths. Use them sparingly, even in local evaluation.

If a scenario works only as superuser, it has not validated the normal application permission model. Re-run the same scenario with the least-privileged role that should succeed.

## Practical admin workflow

1. Bootstrap a local admin role from environment variables.
2. Create application roles through SQL.
3. Grant only the privileges needed for the test.
4. Run the workload.
5. Revoke privileges and drop roles when the scenario is done.

## Troubleshooting permissions

When a query fails with an authorization error, record:

- current role;
- table or schema being accessed;
- SQLSTATE;
- grants that were expected to apply;
- whether a superuser can run the same query.

That separates missing privileges from parser, catalog, or execution bugs.

## What not to assume

Do not assume full PostgreSQL privilege coverage unless the exact object type and command are documented and tested. v0.1 should be evaluated feature by feature, especially for schema privileges, generated ORM migrations, and catalog introspection.
