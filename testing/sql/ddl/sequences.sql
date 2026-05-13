-- Test: CREATE SEQUENCE, DROP SEQUENCE, nextval()
-- Validates sequence lifecycle, auto-incrementing values,
-- and integration with table column defaults.

-- Create and use a sequence
CREATE SEQUENCE user_ids;
-- EXPECT: CREATE SEQUENCE success

SELECT nextval('user_ids') AS id;
-- EXPECT: 1 row, id = 1

SELECT nextval('user_ids') AS id;
-- EXPECT: 1 row, id = 2

-- Drop and recreate
DROP SEQUENCE user_ids;
-- EXPECT: DROP SEQUENCE success

CREATE SEQUENCE user_ids;
-- EXPECT: CREATE SEQUENCE success (name is available again)

-- Sequence with table column default
CREATE TABLE users (
    id BIGINT NOT NULL DEFAULT nextval('user_ids'),
    name TEXT NOT NULL DEFAULT 'anon'
);

INSERT INTO users VALUES (DEFAULT, DEFAULT), (DEFAULT, 'bob');

SELECT id, name FROM users ORDER BY id ASC;
-- EXPECT: row 1: 1, 'anon'
-- EXPECT: row 2: 2, 'bob'

-- Insert using nextval() directly
CREATE SEQUENCE item_ids;
CREATE TABLE items (id BIGINT, label TEXT);

INSERT INTO items VALUES (nextval('item_ids'), 'apple'), (nextval('item_ids'), 'banana');

SELECT id, label FROM items ORDER BY id;
-- EXPECT: row 1: 1, 'apple'
-- EXPECT: row 2: 2, 'banana'

-- Error: nextval on non-existent sequence
-- EXPECT ERROR: SELECT nextval('missing_seq') => UndefinedObject
