-- Test: Basic transaction lifecycle
-- Validates BEGIN, COMMIT, ROLLBACK, and transaction isolation
-- between sessions.

-- Multiple transaction statements in one batch
BEGIN;
COMMIT;
-- EXPECT: BEGIN success, COMMIT success

-- Start a transaction with explicit isolation level
START TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION;
COMMIT;
-- EXPECT: transaction starts with SnapshotIsolation

-- Commit publishes inserts to other sessions
-- Session 1 (writer):
CREATE TABLE users (id INT, name TEXT);
BEGIN;
INSERT INTO users VALUES (1, 'alice');

-- Writer can see the row within the transaction
SELECT id, name FROM users;
-- EXPECT: 1 row: (1, 'alice')

-- Session 2 (reader) should NOT see the uncommitted row:
-- SELECT id, name FROM users;
-- EXPECT: 0 rows (data not yet committed)

COMMIT;

-- After COMMIT, both sessions should see the row:
SELECT id, name FROM users;
-- EXPECT: 1 row: (1, 'alice')

-- Rollback discards inserts
BEGIN;
INSERT INTO users VALUES (2, 'bob');

-- Writer sees the staged row
SELECT id, name FROM users;
-- EXPECT: 2 rows

ROLLBACK;

-- After ROLLBACK, the insert is discarded
SELECT id, name FROM users;
-- EXPECT: 1 row: (1, 'alice') only
