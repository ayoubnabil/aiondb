---
title: Error Reference
order: 83
---

# Error Reference

AionDB maps database errors to PostgreSQL-style SQLSTATE codes where possible. Clients can inspect SQLSTATE instead of parsing message text.

## SQLSTATE codes

The full set of SQLSTATEs the engine emits today. Codes are grouped by class.

### Class 0A — feature not supported

| SQLSTATE | Meaning |
| --- | --- |
| `0A000` | Feature not supported. |

### Class 20 / 22 — data exceptions

| SQLSTATE | Meaning |
| --- | --- |
| `20000` | Case not found. |
| `22001` | String data right truncation. |
| `22003` | Numeric value out of range. |
| `22007` | Invalid datetime format. |
| `22008` | Datetime field overflow. |
| `22012` | Division by zero. |
| `22023` | Invalid parameter value. |
| `22P02` | Invalid text representation. |

### Class 23 — integrity constraint violation

| SQLSTATE | Meaning |
| --- | --- |
| `23502` | Not-null violation. |
| `23503` | Foreign-key violation. |
| `23505` | Unique violation. |
| `23514` | Check-constraint violation. |

### Class 24 / 34 — cursor

| SQLSTATE | Meaning |
| --- | --- |
| `24000` | Invalid cursor state. |
| `34000` | Invalid cursor name. |

### Class 25 / 2B — transaction state

| SQLSTATE | Meaning |
| --- | --- |
| `25P01` | No active SQL transaction. |
| `25P02` | In failed SQL transaction. |
| `25P03` | Idle-in-transaction session timeout. |
| `2BP01` | Dependent objects still exist. |

### Class 28 — authentication

| SQLSTATE | Meaning |
| --- | --- |
| `28000` | Invalid authorization specification. |
| `28P02` | Too many authentication failures. |

### Class 3B / 3D / 3F — catalog & savepoints

| SQLSTATE | Meaning |
| --- | --- |
| `3B001` | Invalid savepoint specification. |
| `3D000` | Invalid catalog name. |
| `3F000` | Invalid schema name. |

### Class 40 — transaction rollback

| SQLSTATE | Meaning |
| --- | --- |
| `40001` | Serialization failure. |
| `40P01` | Deadlock detected. |

### Class 42 — syntax & access rule

| SQLSTATE | Meaning |
| --- | --- |
| `42501` | Insufficient privilege. |
| `42601` | Syntax error. |
| `42701` | Duplicate column. |
| `42703` | Undefined column. |
| `42704` | Undefined object. |
| `42710` | Duplicate object. |
| `42725` | Ambiguous function. |
| `42803` | Grouping error. |
| `42804` | Datatype mismatch. |
| `42809` | Wrong object type. |
| `42883` | Undefined function. |
| `42P01` | Undefined table. |
| `42P02` | Undefined parameter. |
| `42P06` | Duplicate schema. |
| `42P10` | Invalid column reference. |
| `42P16` | Invalid table definition. |

### Class 53 / 54 — resource & program limit

| SQLSTATE | Meaning |
| --- | --- |
| `53300` | Too many connections. |
| `54000` | Program limit exceeded. |

### Class 55 — object not in prerequisite state

| SQLSTATE | Meaning |
| --- | --- |
| `55000` | Object not in prerequisite state. |
| `55P03` | Lock not available. |

### Class 57 — operator intervention

| SQLSTATE | Meaning |
| --- | --- |
| `57014` | Query canceled. |
| `57P01` | Admin shutdown. |
| `57P05` | Idle-session timeout. |

### Class P0 — PL/pgSQL

| SQLSTATE | Meaning |
| --- | --- |
| `P0001` | `RAISE EXCEPTION`. |
| `P0002` | No data found. |
| `P0003` | Too many rows. |
| `P0004` | Assert failure. |

### Class XX — internal

| SQLSTATE | Meaning |
| --- | --- |
| `XX000` | Internal error. |

## Error handling guidance

Applications should branch on SQLSTATE class or exact SQLSTATE where possible:

- `23xxx` for constraint violations;
- `25xxx` for transaction-state issues;
- `28xxx` for authentication issues;
- `40xxx` for retryable transaction conflicts;
- `42xxx` for syntax or object lookup problems;
- `57xxx` for cancellation and shutdown cases.

Message text is for humans. SQLSTATE is for programs. Error wording may become clearer over time, but applications should not depend on exact message strings.

## Examples

Undefined table:

```sql
SELECT * FROM missing_table;
```

Expected class: `42xxx`, with `42P01` when mapped precisely.

Unsupported feature:

```sql
CREATE EXTENSION pg_trgm;
```

Expected class: `0A000` when the feature is intentionally unsupported.

Constraint violation:

```sql
CREATE TABLE unique_demo (id INT PRIMARY KEY);
INSERT INTO unique_demo VALUES (1);
INSERT INTO unique_demo VALUES (1);
```

Expected class: `23xxx`, with `23505` for unique violation where implemented.

## Transaction errors

Errors inside a transaction can affect subsequent statements. Drivers often expect PostgreSQL-style transaction state handling, so test the exact client behavior:

```sql
BEGIN;
SELECT * FROM missing_table;
ROLLBACK;
```

Record whether the client can continue after rollback and which SQLSTATE was returned for the original error.

## Reporting errors

When reporting an error, include:

- SQL text;
- SQLSTATE;
- message;
- whether it happened over pgwire or embedded API;
- transaction state;
- AionDB commit hash.

If another database is the reference, include its SQLSTATE and message too. That makes compatibility work precise.
