-- Test: Concurrent DDL catalog conflicts
-- Validates that concurrent CREATE TABLE with the same name
-- results in a serialization failure for the second committer.
--
-- NOTE: These tests require two concurrent sessions. Comments
-- indicate which session executes each statement.

-- [Session 1] BEGIN;
-- [Session 2] BEGIN;

-- [Session 1] CREATE TABLE users (id INT);
-- [Session 2] CREATE TABLE users (id INT);

-- [Session 1] COMMIT;
-- EXPECT: success (first to commit wins)

-- [Session 2] COMMIT;
-- EXPECT ERROR: SerializationFailure (SQLSTATE 40001)

-- After conflict, Session 2 should fall back to committed state
-- [Session 2] SELECT id FROM users;
-- EXPECT: 0 rows (sees the table committed by Session 1)

-- Session 2 should remain usable
-- [Session 2] BEGIN;
-- [Session 2] ROLLBACK;
-- EXPECT: both succeed
