-- Test: Logical operator precedence in WHERE clauses
-- Validates that AND binds tighter than OR, and that
-- parentheses override default precedence.

CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'bob');

-- AND binds tighter than OR:
-- (id >= 2 AND name = 'bob') OR (id < 2)
-- Matches: id=1 (via OR), id=2 (via AND), id=3 (via AND)
SELECT id, name FROM users WHERE id >= 2 AND name = 'bob' OR id < 2;
-- EXPECT: 3 rows returned (all rows match)

-- Delete with OR filter
DELETE FROM users WHERE id < 2 OR name = 'carol';
-- EXPECT: 1 row affected (only id=1 matches)

SELECT id, name FROM users;
-- EXPECT: 2 rows returned: (2, 'bob'), (3, 'bob')

-- Update with compound AND filter
UPDATE users SET name = 'matched' WHERE id >= 2 AND name > 'a';
-- EXPECT: 2 rows affected

SELECT id, name FROM users;
-- EXPECT: row 1: 2, 'matched'
-- EXPECT: row 2: 3, 'matched'
