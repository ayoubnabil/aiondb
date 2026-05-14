-- Test: Prepared statement lifecycle (Parse/Bind/Execute)
-- Validates the PostgreSQL extended query protocol flow:
-- PREPARE (parse), BIND (with parameters), EXECUTE (portal).
--
-- NOTE: These tests use the extended query protocol. The SQL shown
-- here represents the logical operations; actual execution requires
-- the engine API (prepare/bind/execute_portal).

-- PREPARE a SELECT with literals
-- PREPARE s1 AS SELECT 1 AS one, 'x', TRUE, NULL;
-- EXPECT: 4 result columns: one (INT), ?column? (TEXT), ?column? (BOOLEAN), ?column? (nullable)

-- PREPARE an INSERT with parameter placeholder
CREATE SEQUENCE user_ids;
CREATE TABLE users (id BIGINT, name TEXT);

-- PREPARE ins_nextval AS INSERT INTO users VALUES (nextval('user_ids'), $1);
-- EXPECT: 1 parameter of type TEXT

-- BIND with actual value
-- BIND p1 TO ins_nextval WITH ('alice');

-- EXECUTE portal
-- EXECUTE p1;
-- EXPECT: INSERT, 1 row affected

SELECT id, name FROM users;
-- EXPECT: row: 1, 'alice'

-- PREPARE with column list and defaults
CREATE TABLE users2 (
    id BIGINT NOT NULL DEFAULT nextval('user_ids'),
    name TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE
);

-- PREPARE ins_user AS INSERT INTO users2 (name) VALUES ($1);
-- EXPECT: 1 parameter of type TEXT

-- BIND p_user TO ins_user WITH ('bob');
-- EXECUTE p_user;

SELECT id, name, active FROM users2;
-- EXPECT: row with auto-generated id, 'bob', true

-- Reject invalid SQL during PREPARE
-- PREPARE bad AS SELEC;
-- EXPECT ERROR: SyntaxError
