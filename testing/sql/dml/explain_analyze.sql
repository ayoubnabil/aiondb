-- Test: EXPLAIN ANALYZE for query plan inspection
-- Validates that EXPLAIN ANALYZE returns a plan description
-- with execution statistics.

-- EXPLAIN ANALYZE on a simple SELECT
EXPLAIN ANALYZE SELECT 1 AS one;
-- EXPECT: Query result with column "QUERY PLAN" (TEXT)
-- EXPECT: output contains "ProjectOnce (cols=1)"
-- EXPECT: output contains "Execution: Query"
-- EXPECT: output contains "Rows Returned: 1"
-- EXPECT: output contains "Memory Used: ... bytes"

-- EXPLAIN ANALYZE on INSERT (command execution)
CREATE TABLE explain_test (id INT);

EXPLAIN ANALYZE INSERT INTO explain_test VALUES (1);
-- EXPECT: Query result with "QUERY PLAN" column
-- EXPECT: output contains "InsertValues table_id=..."
-- EXPECT: output contains "Execution: Command (INSERT)"
-- EXPECT: output contains "Rows Affected: 1"
-- EXPECT: output contains "Memory Used: ... bytes"

-- Verify the INSERT actually executed
SELECT id FROM explain_test;
-- EXPECT: 1 row: id = 1
