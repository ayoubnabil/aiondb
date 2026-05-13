-- Test: Access control list (ACL) enforcement
-- Validates that catalog roles require explicit GRANT to access tables,
-- and that SUPERUSER bypasses all ACL checks.
--
-- NOTE: These tests require multi-session support. Comments indicate
-- which session executes each statement.

-- [Admin session] Setup
CREATE TABLE items (id INT NOT NULL);

-- SELECT denied without GRANT
CREATE ROLE reader LOGIN;
-- [Reader session as 'reader']
-- EXPECT ERROR: SELECT * FROM items => "permission denied"

-- SELECT allowed after GRANT
-- [Admin session]
GRANT SELECT ON items TO reader;
-- [Reader session]
-- EXPECT: SELECT * FROM items => succeeds (0 rows)

-- INSERT denied without GRANT
CREATE ROLE writer LOGIN;
-- [Writer session as 'writer']
-- EXPECT ERROR: INSERT INTO items (id) VALUES (1) => "permission denied"

-- GRANT ALL allows everything
CREATE ROLE poweruser LOGIN;
GRANT ALL PRIVILEGES ON items TO poweruser;
-- [Poweruser session]
-- EXPECT: SELECT * FROM items => succeeds
-- EXPECT: INSERT INTO items (id) VALUES (1) => succeeds
-- EXPECT: UPDATE items SET id = 2 WHERE id = 1 => succeeds
-- EXPECT: DELETE FROM items WHERE id = 2 => succeeds

-- SUPERUSER bypasses ACL
CREATE ROLE superadmin SUPERUSER LOGIN;
-- No explicit GRANT to superadmin
-- [Superadmin session]
-- EXPECT: SELECT * FROM items => succeeds (superuser bypass)
-- EXPECT: INSERT INTO items (id) VALUES (1) => succeeds
-- EXPECT: UPDATE items SET id = 2 => succeeds
-- EXPECT: DELETE FROM items => succeeds

-- REVOKE removes access
-- [Admin session]
REVOKE SELECT ON items FROM reader;
-- [Reader session]
-- EXPECT ERROR: SELECT * FROM items => "permission denied" (revoked)

-- UPDATE denied when only SELECT is granted
CREATE ROLE readonly LOGIN;
GRANT SELECT ON items TO readonly;
-- [Readonly session]
-- EXPECT: SELECT * FROM items => succeeds
-- EXPECT ERROR: UPDATE items SET id = 1 => "permission denied"
-- EXPECT ERROR: DELETE FROM items => "permission denied"
