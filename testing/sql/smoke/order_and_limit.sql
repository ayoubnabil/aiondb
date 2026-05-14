-- Test: ORDER BY and LIMIT clauses
-- Validates multi-column ordering, alias ordering, and row limiting.

CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (2, 'bob'), (1, 'carol'), (3, 'bob');

-- Order by multiple columns: name DESC, then id ASC
SELECT id, name FROM users ORDER BY name DESC, id ASC;
-- EXPECT: 3 rows returned
-- EXPECT: row 1: 1, 'carol'
-- EXPECT: row 2: 2, 'bob'
-- EXPECT: row 3: 3, 'bob'

-- Order by projection alias
INSERT INTO users VALUES (4, 'alice');
SELECT id AS ident, name FROM users ORDER BY ident DESC;
-- EXPECT: row 1: 4, 'alice' (wait -- 4 is max id)
-- EXPECT: rows ordered by ident descending: 4, 3, 2, 1

-- LIMIT without ordering
SELECT id, name FROM users LIMIT 2;
-- EXPECT: 2 rows returned (first 2 in storage order)

-- LIMIT with ordering
SELECT id, name FROM users ORDER BY id DESC LIMIT 2;
-- EXPECT: 2 rows returned
-- EXPECT: row 1: 4, 'alice'
-- EXPECT: row 2: 3, 'bob'
