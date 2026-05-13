\set ON_ERROR_STOP on

DROP TABLE IF EXISTS xtask_psql_users;
CREATE TABLE xtask_psql_users (id INT NOT NULL, name TEXT NOT NULL);
INSERT INTO xtask_psql_users (id, name) VALUES (1, 'alice'), (2, 'bob');

SELECT name
FROM xtask_psql_users
WHERE id = 2;

SELECT column_name
FROM information_schema.columns
WHERE table_name = 'xtask_psql_users'
ORDER BY column_name;

BEGIN;
INSERT INTO xtask_psql_users (id, name) VALUES (3, 'carol');
ROLLBACK;

SELECT COUNT(*)
FROM xtask_psql_users;

DROP TABLE xtask_psql_users;
