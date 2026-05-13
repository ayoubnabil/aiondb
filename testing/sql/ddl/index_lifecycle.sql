-- Test: CREATE INDEX, DROP INDEX, and index-based query plans
-- Validates index creation, deletion, recreation, and that
-- dropping a table also removes its dependent indexes.

-- Setup
CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave');

-- Create an index
CREATE INDEX users_id_idx ON users (id);
-- EXPECT: CREATE INDEX success

-- Queries using the index should return correct results
SELECT id, name FROM users WHERE id = 2;
-- EXPECT: 1 row: 2, 'bob'

-- Range query with index
SELECT id, name FROM users WHERE id >= 2 AND id < 4;
-- EXPECT: 2 rows: (2, 'bob'), (3, 'carol')

-- Drop the index
DROP INDEX users_id_idx;
-- EXPECT: DROP INDEX success

-- Recreate the index (ensures cleanup was correct)
CREATE INDEX users_id_idx ON users (id);
-- EXPECT: CREATE INDEX success

-- Drop the table and verify indexes are also cleaned up
DROP TABLE users;
-- EXPECT: DROP TABLE success

-- Recreate table and index with same names (proves full cleanup)
CREATE TABLE users (id INT);
CREATE INDEX users_id_idx ON users (id);
-- EXPECT: both succeed (no name conflicts from dropped table)
