-- Test: SAVEPOINT, ROLLBACK TO SAVEPOINT, RELEASE SAVEPOINT
-- Validates partial rollback within a transaction using savepoints,
-- including nested savepoints and error cases.

CREATE TABLE t (id INT);

-- Basic savepoint
BEGIN;
SAVEPOINT sp1;
-- EXPECT: SAVEPOINT success

INSERT INTO t VALUES (1);
SAVEPOINT sp1;  -- redefine with same name is allowed within different scope

ROLLBACK TO SAVEPOINT sp1;
-- After rollback, insert should be undone... but sp1 was redefined.

COMMIT;

-- Rollback to savepoint undoes inserts after the savepoint
DELETE FROM t;
BEGIN;
INSERT INTO t VALUES (1);
SAVEPOINT sp1;
INSERT INTO t VALUES (2);

-- Both rows visible before rollback
SELECT id FROM t;
-- EXPECT: 2 rows: 1, 2

ROLLBACK TO SAVEPOINT sp1;

-- Only the first row remains
SELECT id FROM t;
-- EXPECT: 1 row: 1

COMMIT;

-- RELEASE SAVEPOINT preserves work
DELETE FROM t;
BEGIN;
SAVEPOINT sp1;
INSERT INTO t VALUES (1);
RELEASE SAVEPOINT sp1;
-- EXPECT: RELEASE success

-- After release, the insert is still visible
SELECT id FROM t;
-- EXPECT: 1 row: 1

COMMIT;

-- Released savepoint cannot be rolled back to
DELETE FROM t;
BEGIN;
SAVEPOINT sp1;
RELEASE SAVEPOINT sp1;
-- EXPECT ERROR: ROLLBACK TO SAVEPOINT sp1 => no such savepoint

COMMIT;

-- Nested savepoints
DELETE FROM t;
BEGIN;
INSERT INTO t VALUES (1);
SAVEPOINT sp1;
INSERT INTO t VALUES (2);
SAVEPOINT sp2;
INSERT INTO t VALUES (3);

-- Three rows visible
SELECT id FROM t;
-- EXPECT: 3 rows: 1, 2, 3

-- Rollback to sp2: removes row 3
ROLLBACK TO SAVEPOINT sp2;
SELECT id FROM t;
-- EXPECT: 2 rows: 1, 2

-- Rollback to sp1: removes row 2
ROLLBACK TO SAVEPOINT sp1;
SELECT id FROM t;
-- EXPECT: 1 row: 1

COMMIT;

-- Savepoint can be re-used after rollback
DELETE FROM t;
BEGIN;
SAVEPOINT sp1;

INSERT INTO t VALUES (1);
ROLLBACK TO SAVEPOINT sp1;
SELECT id FROM t;
-- EXPECT: 0 rows

INSERT INTO t VALUES (2);
ROLLBACK TO SAVEPOINT sp1;
SELECT id FROM t;
-- EXPECT: 0 rows

COMMIT;

-- COMMIT clears savepoints
DELETE FROM t;
BEGIN;
SAVEPOINT sp1;
INSERT INTO t VALUES (1);
COMMIT;

-- Old savepoint should not exist in a new transaction
BEGIN;
-- EXPECT ERROR: ROLLBACK TO SAVEPOINT sp1 => no such savepoint
COMMIT;

-- Savepoint with DDL rollback
BEGIN;
SAVEPOINT sp1;
CREATE TABLE new_table (id INT);
SELECT id FROM new_table;
-- EXPECT: 0 rows (table exists)

ROLLBACK TO SAVEPOINT sp1;
-- EXPECT ERROR: SELECT id FROM new_table => UndefinedTable (table rolled back)

COMMIT;
