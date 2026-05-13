-- Test: CREATE TABLE with various constraint types
-- Validates PRIMARY KEY, UNIQUE, CHECK, FOREIGN KEY, and NOT NULL
-- constraints at table creation time.

-- Simple table
CREATE TABLE users (id INT, name TEXT);
-- EXPECT: CREATE TABLE success

-- Table with PRIMARY KEY
CREATE TABLE employees (id INT PRIMARY KEY, name TEXT);
-- EXPECT: CREATE TABLE success

INSERT INTO employees VALUES (1, 'Alice');
-- EXPECT: 1 row affected

-- Composite PRIMARY KEY
CREATE TABLE order_items (
    order_id INT,
    product_id INT,
    qty INT,
    PRIMARY KEY (order_id, product_id)
);
-- EXPECT: CREATE TABLE success

INSERT INTO order_items VALUES (1, 100, 5);
-- EXPECT: 1 row affected

-- UNIQUE constraint
CREATE TABLE accounts (id INT, email TEXT UNIQUE);
-- EXPECT: CREATE TABLE success

INSERT INTO accounts VALUES (1, 'user@example.com');
-- EXPECT: 1 row affected

-- CHECK constraint
CREATE TABLE products (id INT, price INT, CHECK (price > 0));
-- EXPECT: CREATE TABLE success

INSERT INTO products VALUES (1, 25);
-- EXPECT: 1 row affected

-- FOREIGN KEY constraint
CREATE TABLE orders (
    id INT,
    employee_id INT,
    FOREIGN KEY (employee_id) REFERENCES employees (id)
);
-- EXPECT: CREATE TABLE success

-- NOT NULL constraint
CREATE TABLE required_fields (id INT NOT NULL, name TEXT NOT NULL);
-- EXPECT: CREATE TABLE success
