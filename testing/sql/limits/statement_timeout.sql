-- Test: Statement timeout enforcement
-- Validates that queries exceeding the configured statement_timeout
-- are cancelled with QueryCanceled (SQLSTATE 57014).
--
-- NOTE: These tests require specific runtime configuration.
-- The test harness must set statement_timeout to 0 (immediate timeout)
-- or a very small value.

-- With statement_timeout = 0:
-- EXPECT ERROR: SELECT 1 => QueryCanceled (SQLSTATE 57014)

-- With a normal timeout, queries should succeed:
SELECT 1 AS one;
-- EXPECT: 1 row returned
