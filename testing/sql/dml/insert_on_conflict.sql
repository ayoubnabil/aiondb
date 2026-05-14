-- Test: UNIQUE constraint enforcement on INSERT
-- Validates that duplicate values are rejected for UNIQUE columns,
-- composite UNIQUE constraints work, and NULLs are treated as distinct.

-- Single-column UNIQUE violation
CREATE TABLE users (id INT NOT NULL, email TEXT UNIQUE);

INSERT INTO users VALUES (1, 'alice@example.com');
-- EXPECT: 1 row affected

INSERT INTO users VALUES (2, 'bob@example.com');
-- EXPECT: 1 row affected (distinct value, should succeed)

-- EXPECT ERROR: INSERT INTO users VALUES (3, 'alice@example.com')
--   => SQLSTATE 23505 (UniqueViolation), message contains "unique constraint"

-- Composite UNIQUE constraint
CREATE TABLE order_items (
    customer_id INT NOT NULL,
    product_id INT NOT NULL,
    qty INT,
    UNIQUE (customer_id, product_id)
);

INSERT INTO order_items VALUES (1, 100, 5);
-- EXPECT: 1 row affected

-- Same composite key should fail
-- EXPECT ERROR: INSERT INTO order_items VALUES (1, 100, 10) => UniqueViolation

-- Partial match on composite key should succeed
INSERT INTO order_items VALUES (1, 200, 3);
-- EXPECT: 1 row affected

-- NULLs are distinct in UNIQUE columns (SQL standard)
CREATE TABLE codes (id INT NOT NULL, code TEXT UNIQUE);

INSERT INTO codes VALUES (1, NULL);
INSERT INTO codes VALUES (2, NULL);
INSERT INTO codes VALUES (3, NULL);
-- EXPECT: all 3 inserts succeed (multiple NULLs allowed)

-- NULL does not conflict with a real value
INSERT INTO codes VALUES (4, 'abc');
INSERT INTO codes VALUES (5, NULL);
-- EXPECT: both succeed

-- Batch insert where second row violates UNIQUE
CREATE TABLE batch_test (id INT NOT NULL, code TEXT UNIQUE);
-- EXPECT ERROR: INSERT INTO batch_test VALUES (1, 'dup'), (2, 'dup')
--   => UniqueViolation (within same INSERT statement)
