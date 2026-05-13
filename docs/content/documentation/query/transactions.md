---
title: Transactions
order: 58
---

# Transactions

AionDB supports explicit transaction control for implemented statement paths.

## Basic transaction flow

```sql
BEGIN;

INSERT INTO accounts VALUES (1, 100);
UPDATE accounts SET balance = balance - 10 WHERE id = 1;

COMMIT;
```

Rollback:

```sql
BEGIN;
INSERT INTO accounts VALUES (2, 50);
ROLLBACK;
```

Use explicit transactions when multiple statements must be treated as one unit. For single-statement tests, autocommit behavior is usually simpler and easier to debug.

## Savepoints

Savepoint behavior is implemented on supported paths:

```sql
BEGIN;
INSERT INTO accounts VALUES (3, 25);
SAVEPOINT before_adjustment;
UPDATE accounts SET balance = balance + 5 WHERE id = 3;
ROLLBACK TO SAVEPOINT before_adjustment;
COMMIT;
```

Savepoints are useful when testing driver behavior because many ORMs use them internally for nested transaction-like flows. If an ORM fails, check whether it emitted `SAVEPOINT`, `RELEASE SAVEPOINT`, or `ROLLBACK TO SAVEPOINT`.

## Driver expectations

PostgreSQL drivers usually track transaction state strictly. After an error, some clients expect the session to remain in a failed transaction state until rollback. Others reset connections through pool-specific logic.

When validating a driver, test:

- successful commit;
- explicit rollback;
- error inside transaction followed by rollback;
- savepoint rollback;
- prepared statement inside transaction;
- connection reuse after transaction failure.

## Practical guidance

- Keep transactions short during alpha evaluation.
- Avoid holding idle transactions open.
- Set statement and lock timeout limits when running untrusted or exploratory workloads.
- Treat isolation behavior as something to validate against your workload rather than as a blanket PostgreSQL equivalence claim.

## Isolation levels

The engine recognises three isolation levels, returned by the transaction manager and threaded through the executor's MVCC visibility check:

| Level | Behaviour |
| --- | --- |
| `READ COMMITTED` | Each statement sees a fresh snapshot taken at its start. The default isolation used when `BEGIN` is issued without an explicit level. |
| `SNAPSHOT ISOLATION` (parser also accepts `REPEATABLE READ`) | Snapshot taken at `BEGIN`. Every statement in the transaction sees the same `(xmin, xmax, active)` view, so subsequent reads in the same transaction are repeatable. |
| `SERIALIZABLE` | Snapshot isolation plus a per-transaction read-set / write-set tracker. The serializable coordinator validates the conflict graph at commit time and returns `40001` (serialization failure) when an interleaving that violates serial-equivalent semantics is detected. |

Lock handling is shared across all three levels: the wait-graph lock manager rejects cycles with `40P01` (deadlock detected) before parking, and `lock_timeout` cuts the wait short with `55P03` (lock not available).

## Isolation guidance

Do not assume full PostgreSQL isolation behavior unless the exact scenario is documented and tested. For application evaluation, write small concurrent tests for the anomalies you care about:

- dirty reads;
- non-repeatable reads;
- lost updates;
- write skew;
- DDL while another session is querying;
- read behavior after rollback.

Keep these tests deterministic where possible. Sleep-based concurrency tests are hard to trust unless they are only used as smoke tests.

## Failure behavior

If a statement fails inside a transaction, test the exact behavior expected by your client or ORM. PostgreSQL drivers often have strict assumptions about transaction error state and recovery.

## Operational rule

During v0.1 evaluation, keep transactions short and explicit. Long idle transactions make debugging storage, locking, and recovery behavior harder because they leave the system in a state that is difficult to reproduce.
