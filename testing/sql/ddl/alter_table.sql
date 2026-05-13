-- Test: ALTER TABLE operations
-- Validates RENAME table, RENAME column, SET/DROP DEFAULT,
-- SET/DROP NOT NULL.

-- Setup
CREATE TABLE old_name (id INT);
INSERT INTO old_name VALUES (1);

-- Rename table
ALTER TABLE old_name RENAME TO new_name;
-- EXPECT: ALTER TABLE success

SELECT id FROM new_name;
-- EXPECT: 1 row returned, value: 1

-- old_name should no longer resolve
-- EXPECT ERROR: SELECT id FROM old_name => "does not exist"

-- Rename column
CREATE TABLE t_rename_col (old_col INT);
INSERT INTO t_rename_col VALUES (42);

ALTER TABLE t_rename_col RENAME COLUMN old_col TO new_col;
-- EXPECT: ALTER TABLE success

SELECT new_col FROM t_rename_col;
-- EXPECT: 1 row returned, value: 42

-- SET DEFAULT
CREATE TABLE t_default (id INT, name TEXT);
ALTER TABLE t_default ALTER COLUMN name SET DEFAULT 'hello';
-- EXPECT: ALTER TABLE success

INSERT INTO t_default (id) VALUES (1);
SELECT id, name FROM t_default;
-- EXPECT: row: 1, 'hello'

-- DROP DEFAULT
CREATE TABLE t_drop_default (id INT, name TEXT DEFAULT 'world');
ALTER TABLE t_drop_default ALTER COLUMN name DROP DEFAULT;
-- EXPECT: ALTER TABLE success

INSERT INTO t_drop_default (id) VALUES (1);
SELECT id, name FROM t_drop_default;
-- EXPECT: row: 1, NULL (default was dropped)

-- SET NOT NULL
CREATE TABLE t_not_null (id INT, name TEXT);
ALTER TABLE t_not_null ALTER COLUMN name SET NOT NULL;
-- EXPECT: ALTER TABLE success

-- DROP NOT NULL
CREATE TABLE t_drop_not_null (id INT, name TEXT NOT NULL DEFAULT 'x');
ALTER TABLE t_drop_not_null ALTER COLUMN name DROP NOT NULL;
-- EXPECT: ALTER TABLE success

INSERT INTO t_drop_not_null (id) VALUES (1);
-- EXPECT: 1 row affected (NULL is now allowed for name)
