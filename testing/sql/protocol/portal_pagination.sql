-- Test: Portal-based result pagination (max_rows)
-- Validates that portals support incremental fetching of results,
-- returning partial batches and indicating exhaustion.
--
-- NOTE: Portal pagination requires the engine API (execute_portal
-- with max_rows parameter). SQL shown here is the logical query.

CREATE TABLE t_page (id INT);
INSERT INTO t_page VALUES (1), (2), (3), (4), (5);

-- PREPARE q_page AS SELECT id FROM t_page;
-- BIND p_page TO q_page;

-- EXECUTE p_page (max_rows=2);
-- EXPECT: 2 rows returned, exhausted=false

-- EXECUTE p_page (max_rows=2);
-- EXPECT: 2 rows returned, exhausted=false

-- EXECUTE p_page (max_rows=2);
-- EXPECT: 1 row returned, exhausted=true

-- EXECUTE p_page (max_rows=2);
-- EXPECT: 0 rows returned, exhausted=true (portal already done)
