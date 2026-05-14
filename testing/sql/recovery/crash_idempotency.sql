-- Test: Crash recovery idempotency
-- Validates that replaying WAL multiple times produces the same
-- result, and that interleaved transactions are handled correctly.

-- Setup: create table and insert data across multiple transactions
CREATE TABLE recovery_test (id INT, value TEXT);

-- Transaction 1: committed
BEGIN;
INSERT INTO recovery_test VALUES (1, 'committed_1');
COMMIT;

-- Transaction 2: committed
BEGIN;
INSERT INTO recovery_test VALUES (2, 'committed_2');
INSERT INTO recovery_test VALUES (3, 'committed_3');
COMMIT;

-- Transaction 3: NOT committed (will be in-flight at crash)
BEGIN;
INSERT INTO recovery_test VALUES (4, 'uncommitted');
-- (crash here without COMMIT)

-- After recovery:
SELECT id, value FROM recovery_test ORDER BY id;
-- EXPECT: 3 rows (only committed data)
-- EXPECT: row 1: 1, 'committed_1'
-- EXPECT: row 2: 2, 'committed_2'
-- EXPECT: row 3: 3, 'committed_3'
-- EXPECT: row 4 (id=4) should NOT be present

-- After second recovery (idempotency check):
-- Same query should return identical results
SELECT id, value FROM recovery_test ORDER BY id;
-- EXPECT: same 3 rows (replay is idempotent)
