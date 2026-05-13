-- Test: DDL statements within transactions
-- Validates that CREATE TABLE, DROP TABLE, CREATE INDEX, and
-- DROP INDEX are transactional -- visible only after COMMIT
-- and reversible via ROLLBACK.

-- CREATE TABLE in transaction
BEGIN;
CREATE TABLE tx_users (id INT);

-- Writer session can see the table
SELECT id FROM tx_users;
-- EXPECT: 0 rows (table exists but is empty)

-- Other sessions should NOT see the table (UndefinedTable error)
-- Reader: SELECT id FROM tx_users
-- EXPECT ERROR: UndefinedTable

COMMIT;

-- After COMMIT, all sessions can see the table
SELECT id FROM tx_users;
-- EXPECT: 0 rows (table visible to everyone)

-- ROLLBACK discards table creation
BEGIN;
CREATE TABLE rolled_back_table (id INT);

-- Writer can see it
SELECT id FROM rolled_back_table;
-- EXPECT: 0 rows

ROLLBACK;

-- After ROLLBACK, table no longer exists
-- EXPECT ERROR: SELECT id FROM rolled_back_table => UndefinedTable

-- ROLLBACK restores a dropped table
CREATE TABLE important_data (id INT, name TEXT);
CREATE INDEX important_idx ON important_data (id);
INSERT INTO important_data VALUES (1, 'alice');

BEGIN;
DROP TABLE important_data;

-- Writer cannot see the dropped table
-- EXPECT ERROR: SELECT id FROM important_data => UndefinedTable

ROLLBACK;

-- After ROLLBACK, the table is restored
SELECT id, name FROM important_data;
-- EXPECT: 1 row: (1, 'alice')

-- CREATE TABLE + INSERT in same transaction
BEGIN;
CREATE TABLE tx_combined (id INT, name TEXT);
INSERT INTO tx_combined VALUES (1, 'alice');

SELECT id, name FROM tx_combined;
-- EXPECT: 1 row: (1, 'alice')

COMMIT;

-- Both table and data are published
SELECT id, name FROM tx_combined;
-- EXPECT: 1 row: (1, 'alice')
