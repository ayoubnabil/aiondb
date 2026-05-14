-- Test: INSERT with DEFAULT values and column lists
-- Validates that DEFAULT keyword, column list with omitted defaults,
-- DEFAULT VALUES syntax, and nextval() integration work correctly.

-- Setup sequence and table with defaults
CREATE SEQUENCE user_ids;
CREATE TABLE users (
    id BIGINT NOT NULL DEFAULT nextval('user_ids'),
    name TEXT NOT NULL DEFAULT 'anon',
    active BOOLEAN NOT NULL DEFAULT TRUE
);

-- INSERT with explicit DEFAULT keyword
INSERT INTO users VALUES (DEFAULT, DEFAULT, DEFAULT);

SELECT id, name, active FROM users;
-- EXPECT: row: 1, 'anon', true

-- INSERT with column list (omitted columns get defaults)
INSERT INTO users (name) VALUES ('alice'), ('bob');

SELECT id, name, active FROM users ORDER BY id ASC;
-- EXPECT: row 1: 1, 'anon', true
-- EXPECT: row 2: 2, 'alice', true
-- EXPECT: row 3: 3, 'bob', true

-- INSERT DEFAULT VALUES (all columns use defaults)
CREATE SEQUENCE thing_ids;
CREATE TABLE things (
    id BIGINT NOT NULL DEFAULT nextval('thing_ids'),
    label TEXT NOT NULL DEFAULT 'unnamed'
);

INSERT INTO things DEFAULT VALUES;

SELECT id, label FROM things;
-- EXPECT: row: 1, 'unnamed'

-- INSERT from SELECT with defaults for omitted columns
CREATE SEQUENCE dst_ids;
CREATE TABLE src (name TEXT);
INSERT INTO src VALUES ('carol'), ('dave');

CREATE TABLE dst (
    id BIGINT NOT NULL DEFAULT nextval('dst_ids'),
    name TEXT NOT NULL
);

INSERT INTO dst (name) SELECT name FROM src ORDER BY name DESC;

SELECT id, name FROM dst ORDER BY id ASC;
-- EXPECT: row 1: 1, 'dave'
-- EXPECT: row 2: 2, 'carol'

-- UPDATE with DEFAULT
CREATE SEQUENCE upd_ids;
CREATE TABLE upd_test (
    id BIGINT NOT NULL DEFAULT nextval('upd_ids'),
    name TEXT NOT NULL DEFAULT 'anon'
);

INSERT INTO upd_test VALUES (42, 'custom');
UPDATE upd_test SET id = DEFAULT, name = DEFAULT;

SELECT id, name FROM upd_test;
-- EXPECT: row: 1, 'anon' (defaults applied via UPDATE)
