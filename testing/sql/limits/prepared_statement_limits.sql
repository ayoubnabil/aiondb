-- Test: Prepared statement and portal slot limits
-- Validates that exceeding max_prepared_statements or max_portals
-- is rejected, and that closing frees slots for reuse.

-- With max_prepared_statements = 2:
-- PREPARE s1 AS SELECT 1;
-- EXPECT: success
-- PREPARE s2 AS SELECT 2;
-- EXPECT: success
-- PREPARE s3 AS SELECT 3;
-- EXPECT ERROR: ProgramLimitExceeded

-- Closing a statement frees a slot
-- CLOSE STATEMENT s1;
-- PREPARE s3 AS SELECT 3;
-- EXPECT: success (slot freed by close)

-- With max_portals = 2:
-- PREPARE s1 AS SELECT 1;
-- BIND p1 TO s1;
-- EXPECT: success
-- BIND p2 TO s1;
-- EXPECT: success
-- BIND p3 TO s1;
-- EXPECT ERROR: ProgramLimitExceeded

-- Closing a portal frees a slot
-- CLOSE PORTAL p1;
-- BIND p3 TO s1;
-- EXPECT: success

-- Rebinding the same portal name does not consume a new slot
-- With max_portals = 1:
-- BIND p1 TO s1;
-- EXPECT: success
-- BIND p1 TO s1;  (same name, replace)
-- EXPECT: success (no additional slot used)
