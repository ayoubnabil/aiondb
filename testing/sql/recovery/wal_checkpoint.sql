-- Test: WAL and checkpoint operations
-- Validates that write-ahead log captures changes, checkpoints
-- flush state, and crash recovery restores committed data.
--
-- NOTE: WAL/recovery tests require engine restart and cannot be
-- fully expressed in pure SQL. These statements represent the
-- logical scenario; the test harness handles engine lifecycle.

-- Step 1: Create data before checkpoint
CREATE TABLE wal_test (id INT, name TEXT);
INSERT INTO wal_test VALUES (1, 'before_checkpoint');
-- EXPECT: data written to WAL

-- Step 2: Force a checkpoint
-- (Engine API: checkpoint())
-- EXPECT: checkpoint flushes all dirty pages

-- Step 3: Insert more data after checkpoint
INSERT INTO wal_test VALUES (2, 'after_checkpoint');

-- Step 4: Simulate crash and restart
-- (Engine rebuilds from WAL segments)

-- Step 5: Verify all committed data is recovered
SELECT id, name FROM wal_test ORDER BY id;
-- EXPECT: row 1: 1, 'before_checkpoint'
-- EXPECT: row 2: 2, 'after_checkpoint'

-- Step 6: Verify uncommitted transactions are rolled back
-- (Any data inserted within an uncommitted transaction at crash
--  time should NOT appear after recovery)
