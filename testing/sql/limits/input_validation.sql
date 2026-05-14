-- Test: SQL input length and identifier length limits
-- Validates that oversized SQL strings, statement names, and
-- portal names are rejected with ProgramLimitExceeded.

-- SQL exceeding MAX_SQL_LENGTH should be rejected
-- (Generate a very long SQL string)
-- EXPECT ERROR: SELECT 1,1,1,...(repeated beyond limit) => ProgramLimitExceeded

-- PREPARE with SQL exceeding MAX_SQL_LENGTH
-- EXPECT ERROR: PREPARE s1 AS SELECT 1,1,...(too long) => ProgramLimitExceeded

-- PREPARE with statement name exceeding MAX_IDENTIFIER_LENGTH
-- EXPECT ERROR: PREPARE sssss...(too long) AS SELECT 1 => ProgramLimitExceeded

-- BIND with portal name exceeding MAX_IDENTIFIER_LENGTH
-- EXPECT ERROR: BIND pppp...(too long) TO s1 => ProgramLimitExceeded

-- Empty statement name is valid (unnamed prepared statement in PG protocol)
-- PREPARE '' AS SELECT 1;
-- EXPECT: success, 1 result column

-- Short SQL and short name should succeed
-- PREPARE s1 AS SELECT 1;
-- EXPECT: success
