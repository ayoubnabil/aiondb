-- Test: Basic INSERT and SELECT operations
-- Validates that the engine can create a table, insert rows,
-- and retrieve them with correct column types and values.

CREATE TABLE users (id INT, name TEXT);

INSERT INTO users VALUES (1, 'alice'), (2, 'bob');
-- EXPECT: 2 rows affected

SELECT id, name FROM users;
-- EXPECT: 2 rows returned
-- EXPECT: row 1: 1, 'alice'
-- EXPECT: row 2: 2, 'bob'

-- Select with WHERE filter
SELECT id, name FROM users WHERE id = 2;
-- EXPECT: 1 row returned
-- EXPECT: row: 2, 'bob'

-- Select with NOT and != filters
INSERT INTO users VALUES (3, 'bob');
SELECT id, name FROM users WHERE name != 'alice' AND NOT id = 3;
-- EXPECT: 1 row returned
-- EXPECT: row: 2, 'bob'
