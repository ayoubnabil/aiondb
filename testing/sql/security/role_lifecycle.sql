-- Test: CREATE ROLE, DROP ROLE lifecycle
-- Validates role creation with various options (LOGIN, PASSWORD,
-- SUPERUSER), case-insensitive naming, and cleanup on drop.

-- Basic role creation
CREATE ROLE testrole;
-- EXPECT: CREATE ROLE success

-- Role with LOGIN
CREATE ROLE app_user WITH LOGIN;
-- EXPECT: CREATE ROLE success

-- Role with LOGIN and PASSWORD
CREATE ROLE secure_user LOGIN PASSWORD 'secret';
-- EXPECT: CREATE ROLE success

-- Role with SUPERUSER
CREATE ROLE admin_user SUPERUSER LOGIN;
-- EXPECT: CREATE ROLE success

-- Case-insensitive role names (duplicate detection)
CREATE ROLE CaseTest;
-- EXPECT: CREATE ROLE success

-- EXPECT ERROR: CREATE ROLE casetest => "already exists"

-- Drop a role
DROP ROLE testrole;
-- EXPECT: DROP ROLE success

-- Dropping a non-existent role should fail
-- EXPECT ERROR: DROP ROLE nonexistent => "does not exist"

-- Duplicate role creation should fail
CREATE ROLE dup_role;
-- EXPECT ERROR: CREATE ROLE dup_role => "already exists"

-- Full lifecycle: create, grant, revoke, drop
CREATE TABLE documents (id INT NOT NULL, title TEXT);
CREATE ROLE editor LOGIN PASSWORD 'pass123';

GRANT SELECT, INSERT, UPDATE ON documents TO editor;
-- EXPECT: GRANT success

REVOKE UPDATE ON documents FROM editor;
-- EXPECT: REVOKE success

GRANT CREATE ON SCHEMA public TO editor;
-- EXPECT: GRANT success

DROP ROLE editor;
-- EXPECT: DROP ROLE success (cleans up all privileges)

-- Verify the role is gone
-- EXPECT ERROR: DROP ROLE editor => "does not exist"
