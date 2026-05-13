-- Test: Basic SELECT with literal values
-- Validates that the engine can evaluate constant expressions
-- without any table access (no FROM clause).

-- Select integer, text, boolean, and NULL literals
SELECT 1 AS one, 'x', TRUE, NULL;
-- EXPECT: 1 row returned
-- EXPECT: columns: one (INT), ?column? (TEXT), ?column? (BOOLEAN), ?column? (TEXT, nullable)
-- EXPECT: row: 1, 'x', true, NULL

-- Select with WHERE FALSE (should return 0 rows)
SELECT 1 AS one WHERE FALSE;
-- EXPECT: 0 rows returned

-- Select with boolean expression
SELECT 1 AS one WHERE (TRUE OR FALSE) AND FALSE;
-- EXPECT: 0 rows returned (AND FALSE makes the whole expression false)
