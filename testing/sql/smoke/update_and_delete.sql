-- Test: Basic UPDATE and DELETE operations
-- Validates that rows can be modified and removed, with and without
-- WHERE filters.

CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (1, 'alice'), (2, 'bob');

-- Update all rows
UPDATE users SET id = id, name = 'updated';
-- EXPECT: 2 rows affected

SELECT id, name FROM users;
-- EXPECT: row 1: 1, 'updated'
-- EXPECT: row 2: 2, 'updated'

-- Reset data for next test
DELETE FROM users;
-- EXPECT: 2 rows affected

INSERT INTO users VALUES (1, 'alice'), (2, 'bob');

-- Update with WHERE filter
UPDATE users SET name = 'updated' WHERE id = 2;
-- EXPECT: 1 row affected

SELECT id, name FROM users;
-- EXPECT: row 1: 1, 'alice'
-- EXPECT: row 2: 2, 'updated'

-- Delete with WHERE filter
DELETE FROM users WHERE id = 1;
-- EXPECT: 1 row affected

SELECT id, name FROM users;
-- EXPECT: 1 row returned
-- EXPECT: row: 2, 'updated'
