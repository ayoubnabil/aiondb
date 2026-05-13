-- Test: GRANT and REVOKE privilege management
-- Validates granting/revoking SELECT, INSERT, UPDATE, DELETE,
-- ALL PRIVILEGES on tables, and schema-level grants.

-- Setup
CREATE TABLE items (id INT NOT NULL);
CREATE ROLE testrole;

-- GRANT SELECT on a table
GRANT SELECT ON items TO testrole;
-- EXPECT: GRANT success

-- GRANT multiple privileges
GRANT SELECT, INSERT, UPDATE, DELETE ON items TO testrole;
-- EXPECT: GRANT success

-- GRANT ALL PRIVILEGES
GRANT ALL PRIVILEGES ON items TO testrole;
-- EXPECT: GRANT success

-- GRANT with TABLE keyword (explicit)
GRANT SELECT ON TABLE items TO testrole;
-- EXPECT: GRANT success

-- GRANT on SCHEMA
GRANT CREATE ON SCHEMA public TO testrole;
-- EXPECT: GRANT success

-- REVOKE SELECT
REVOKE SELECT ON items FROM testrole;
-- EXPECT: REVOKE success

-- REVOKE ALL PRIVILEGES
GRANT ALL ON items TO testrole;
REVOKE ALL PRIVILEGES ON items FROM testrole;
-- EXPECT: REVOKE success

-- GRANT is idempotent (granting same privilege twice should succeed)
GRANT SELECT ON items TO testrole;
GRANT SELECT ON items TO testrole;
-- EXPECT: both succeed without error

-- Error: GRANT to non-existent role
-- EXPECT ERROR: GRANT SELECT ON items TO nonexistent => "does not exist"

-- Error: REVOKE from non-existent role
-- EXPECT ERROR: REVOKE SELECT ON items FROM nonexistent => "does not exist"

-- Drop role removes all its privileges
DROP ROLE testrole;
-- EXPECT: DROP ROLE success

-- Granting to a dropped role should fail
-- EXPECT ERROR: GRANT INSERT ON items TO testrole => "does not exist"
