-- Test: SQL error reporting and SQLSTATE codes
-- Validates that various error conditions produce the correct
-- SQLSTATE codes and meaningful error messages.

-- Undefined table
-- EXPECT ERROR: SELECT id FROM users => UndefinedTable

-- Undefined column
-- EXPECT ERROR: SELECT missing => UndefinedColumn

-- Undefined column in UPDATE target
CREATE TABLE users (id INT, name TEXT);
-- EXPECT ERROR: UPDATE users SET missing = 1 => UndefinedColumn

-- WHERE clause requires boolean expression
-- EXPECT ERROR: SELECT id FROM users WHERE 1 => SyntaxError

-- NOT requires boolean expression
-- EXPECT ERROR: SELECT id FROM users WHERE NOT 1 => SyntaxError

-- Invalid SQL syntax
-- EXPECT ERROR: SELEC => SyntaxError

-- UPDATE on non-existent table
-- EXPECT ERROR: UPDATE no_such_table SET x = 1 => UndefinedTable

-- DELETE from non-existent table
-- EXPECT ERROR: DELETE FROM no_such_table => UndefinedTable

-- DEFAULT outside INSERT context
-- EXPECT ERROR: SELECT DEFAULT => SyntaxError

-- NULL into NOT NULL column
CREATE TABLE strict (id INT NOT NULL, name TEXT);
-- EXPECT ERROR: INSERT INTO strict VALUES (NULL, 'alice') => Constraint violation

-- Unique violation reports SQLSTATE 23505
CREATE TABLE products (id INT NOT NULL, sku TEXT UNIQUE);
INSERT INTO products VALUES (1, 'SKU-001');
-- EXPECT ERROR: INSERT INTO products VALUES (2, 'SKU-001')
--   => SQLSTATE 23505 (UniqueViolation)
--   => message contains table name "products"
