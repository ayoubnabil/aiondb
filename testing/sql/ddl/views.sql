-- Test: CREATE VIEW, DROP VIEW, querying through views
-- Validates view creation, querying, filtering, aggregation,
-- joins through views, and error cases.

-- Setup base tables
CREATE TABLE users (id INT, name TEXT);
INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol');

CREATE TABLE orders (user_id INT, product TEXT);
INSERT INTO orders VALUES (1, 'widget'), (2, 'gadget');

-- Create a simple view
CREATE VIEW all_users AS SELECT id, name FROM users;
-- EXPECT: CREATE VIEW success

-- Select all from view
SELECT * FROM all_users;
-- EXPECT: 3 rows: (1, 'alice'), (2, 'bob'), (3, 'carol')

-- Filter through a view
SELECT * FROM all_users WHERE id > 1;
-- EXPECT: 2 rows: (2, 'bob'), (3, 'carol')

-- View with GROUP BY aggregation
CREATE TABLE sales (product TEXT, amount INT);
INSERT INTO sales VALUES ('a', 10), ('b', 20), ('a', 30);

CREATE VIEW product_totals AS
    SELECT product, SUM(amount) AS total FROM sales GROUP BY product;

SELECT * FROM product_totals ORDER BY product;
-- EXPECT: row 1: 'a', 40
-- EXPECT: row 2: 'b', 20

-- View with ORDER BY and LIMIT
CREATE VIEW top_users AS
    SELECT id, name FROM users ORDER BY id LIMIT 2;

SELECT * FROM top_users;
-- EXPECT: 2 rows: (1, 'alice'), (2, 'bob')

-- View used in a JOIN
CREATE VIEW user_names AS SELECT id, name FROM users;

SELECT orders.product, user_names.name
FROM orders
INNER JOIN user_names ON orders.user_id = user_names.id
ORDER BY orders.product;
-- EXPECT: row 1: 'gadget', 'bob'
-- EXPECT: row 2: 'widget', 'alice'

-- Drop view
DROP VIEW all_users;
-- EXPECT: DROP VIEW success

-- Selecting from dropped view should fail
-- EXPECT ERROR: SELECT * FROM all_users => "does not exist"

-- Duplicate view name should fail
-- EXPECT ERROR: CREATE VIEW user_names AS SELECT id FROM users => "already exists"
