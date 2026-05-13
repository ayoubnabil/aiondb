-- Test: VECTOR column type -- CRUD operations
-- Validates CREATE TABLE with VECTOR(N), INSERT vector data via
-- text literal, and SELECT vector data back.

-- Create table with VECTOR column
CREATE TABLE items (id INT, embedding VECTOR(3));
-- EXPECT: CREATE TABLE success

-- Insert vector data using text literal
INSERT INTO items VALUES (1, '[1.0,2.0,3.0]');
-- EXPECT: 1 row affected

-- Select vector data back
SELECT id, embedding FROM items;
-- EXPECT: 1 row
-- EXPECT: id = 1
-- EXPECT: embedding column type = VECTOR(3)
-- EXPECT: embedding value = [1.0, 2.0, 3.0]

-- Insert multiple vectors
INSERT INTO items VALUES (2, '[4.0,5.0,6.0]'), (3, '[7.0,8.0,9.0]');
-- EXPECT: 2 rows affected

SELECT id, embedding FROM items ORDER BY id;
-- EXPECT: 3 rows with correct vector values
