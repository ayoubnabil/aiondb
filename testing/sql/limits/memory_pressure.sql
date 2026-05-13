-- Test: Memory budget enforcement
-- Validates that queries exceeding max_memory_bytes are rejected
-- with ProgramLimitExceeded (SQLSTATE 54000).

-- With max_memory_bytes = 1 (very tight):
-- Even a simple literal select should fail
-- EXPECT ERROR: SELECT 1 => ProgramLimitExceeded

-- Table scan exceeding memory budget
CREATE TABLE mem_test (id INT, name TEXT);
INSERT INTO mem_test VALUES (1, 'hello'), (2, 'world');

-- With max_memory_bytes = 1:
-- EXPECT ERROR: SELECT * FROM mem_test => ProgramLimitExceeded

-- Join result exceeding memory budget
-- With max_memory_bytes = 24:
CREATE TABLE mj_left (lx INT);
CREATE TABLE mj_right (ry INT);
INSERT INTO mj_left VALUES (1), (2);
INSERT INTO mj_right VALUES (10), (20);

-- EXPECT ERROR: SELECT lx, ry FROM mj_left, mj_right ORDER BY lx, ry
--   => ProgramLimitExceeded

-- Aggregate output exceeding memory budget
-- With max_memory_bytes = 1:
CREATE TABLE empty_agg (id INT);
-- EXPECT ERROR: SELECT COUNT(*) AS cnt FROM empty_agg => ProgramLimitExceeded
