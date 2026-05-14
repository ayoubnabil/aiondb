-- Test: Result row and byte limits
-- Validates that queries exceeding max_result_rows or max_result_bytes
-- are rejected with ProgramLimitExceeded (SQLSTATE 54000).

-- Setup
CREATE TABLE t_rows (id INT);
INSERT INTO t_rows VALUES (1), (2), (3), (4), (5);

-- With max_result_rows = 2:
-- EXPECT ERROR: SELECT id FROM t_rows => ProgramLimitExceeded

-- With max_result_rows = 5 (within limit):
SELECT id FROM t_rows;
-- EXPECT: 5 rows returned (within limit)

-- SQL LIMIT restricts rows regardless of server limit
-- With max_result_rows = 100:
SELECT id FROM t_rows LIMIT 2;
-- EXPECT: 2 rows returned

-- With max_result_bytes = 1 (very small):
-- EXPECT ERROR: SELECT 'hello world' => ProgramLimitExceeded

-- With max_result_bytes = 1024 (generous):
SELECT 1;
-- EXPECT: 1 row returned (within byte limit)
