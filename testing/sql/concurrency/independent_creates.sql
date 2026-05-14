-- Test: Independent concurrent DDL operations
-- Validates that non-conflicting concurrent DDL (different names)
-- can both commit successfully.
--
-- NOTE: Requires two concurrent sessions.

-- [Session 1] BEGIN;
-- [Session 2] BEGIN;

-- Independent table creation (different names)
-- [Session 1] CREATE TABLE first_users (id INT);
-- [Session 2] CREATE TABLE second_users (id INT);

-- [Session 1] COMMIT;
-- EXPECT: success

-- [Session 2] COMMIT;
-- EXPECT: success (no conflict -- different table names)

-- Both tables should exist
-- [Session 1]
INSERT INTO first_users VALUES (1);
SELECT id FROM first_users;
-- EXPECT: 1 row

-- [Session 2]
INSERT INTO second_users VALUES (2);
SELECT id FROM second_users;
-- EXPECT: 1 row

-- Independent index creation on same table
CREATE TABLE shared_table (id INT, name TEXT);
INSERT INTO shared_table VALUES (1, 'alice'), (2, 'bob');

-- [Session 1] BEGIN;
-- [Session 2] BEGIN;

-- [Session 1] CREATE INDEX idx_id ON shared_table (id);
-- [Session 2] CREATE INDEX idx_name ON shared_table (name);

-- [Session 1] COMMIT;
-- [Session 2] COMMIT;
-- EXPECT: both succeed, table has 2 indexes

-- Independent sequence creation
-- [Session 1] BEGIN;
-- [Session 2] BEGIN;

-- [Session 1] CREATE SEQUENCE first_ids;
-- [Session 2] CREATE SEQUENCE second_ids;

-- [Session 1] COMMIT;
-- [Session 2] COMMIT;
-- EXPECT: both succeed

SELECT nextval('first_ids') AS id;
-- EXPECT: 1

SELECT nextval('second_ids') AS id;
-- EXPECT: 1
