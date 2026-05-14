-- Test: DELETE with various filter conditions
-- Validates unconditional delete, filtered delete with WHERE,
-- and compound OR/AND filters.

CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol');

-- Delete all rows
DELETE FROM users;
-- EXPECT: 3 rows affected

SELECT id, name FROM users;
-- EXPECT: 0 rows returned (table is empty)

-- Re-populate for filtered delete
INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol');

-- Delete with WHERE filter
DELETE FROM users WHERE id = 1;
-- EXPECT: 1 row affected

SELECT id, name FROM users;
-- EXPECT: 2 rows: (2, 'bob'), (3, 'carol')

-- Delete with OR filter
INSERT INTO users VALUES (1, 'alice');
DELETE FROM users WHERE id < 2 OR name = 'carol';
-- EXPECT: 2 rows affected (id=1 and name='carol')

SELECT id, name FROM users;
-- EXPECT: 1 row: (2, 'bob')

-- Primary key duplicate insert should fail
CREATE TABLE pk_test (id INT PRIMARY KEY, name TEXT);
INSERT INTO pk_test VALUES (1, 'Alice');
-- EXPECT ERROR: INSERT INTO pk_test VALUES (1, 'Bob') => UniqueViolation (SQLSTATE 23505)

-- Primary key rejects NULL
-- EXPECT ERROR: INSERT INTO pk_test VALUES (NULL, 'Alice') => constraint error

-- UPDATE that violates UNIQUE
CREATE TABLE unique_test (id INT NOT NULL, code TEXT UNIQUE);
INSERT INTO unique_test VALUES (1, 'aaa');
INSERT INTO unique_test VALUES (2, 'bbb');

-- EXPECT ERROR: UPDATE unique_test SET code = 'aaa' WHERE id = 2 => UniqueViolation

-- UPDATE to same value should succeed (self-update)
UPDATE unique_test SET code = 'aaa' WHERE id = 1;
-- EXPECT: 1 row affected (no actual conflict)

-- UPDATE to a new distinct value should succeed
UPDATE unique_test SET code = 'ccc' WHERE id = 2;
-- EXPECT: 1 row affected
