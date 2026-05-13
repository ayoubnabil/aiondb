-- Test: Graph data model -- populating nodes and edges
-- Validates that graph labels work with standard DML operations
-- on the underlying tables.

-- Setup node tables
CREATE TABLE persons (id INT NOT NULL, name TEXT, age INT);
CREATE TABLE cities (id INT NOT NULL, name TEXT, country TEXT);

-- Setup edge tables
CREATE TABLE lives_in (source_id INT NOT NULL, target_id INT NOT NULL);
CREATE TABLE friends (source_id INT NOT NULL, target_id INT NOT NULL, since INT);

-- Create labels
CREATE NODE LABEL person ON persons;
CREATE NODE LABEL city ON cities;
CREATE EDGE LABEL lives_in ON lives_in SOURCE person TARGET city;
CREATE EDGE LABEL friends ON friends SOURCE person TARGET person;

-- Populate node data using standard INSERT
INSERT INTO persons VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Carol', 35);
INSERT INTO cities VALUES (10, 'Paris', 'France'), (20, 'London', 'UK');

-- Populate edge data
INSERT INTO lives_in VALUES (1, 10), (2, 20), (3, 10);
INSERT INTO friends VALUES (1, 2, 2020), (2, 3, 2021);

-- Query graph data through standard SQL
-- Find all persons living in Paris
SELECT p.name, c.name AS city
FROM persons p
INNER JOIN lives_in li ON p.id = li.source_id
INNER JOIN cities c ON li.target_id = c.id
WHERE c.name = 'Paris'
ORDER BY p.name;
-- EXPECT: row 1: 'Alice', 'Paris'
-- EXPECT: row 2: 'Carol', 'Paris'

-- Find friends of Bob
SELECT p2.name AS friend
FROM persons p1
INNER JOIN friends f ON p1.id = f.source_id
INNER JOIN persons p2 ON f.target_id = p2.id
WHERE p1.name = 'Bob'
ORDER BY p2.name;
-- EXPECT: row 1: 'Carol'

-- Count persons per city
SELECT c.name AS city, COUNT(*) AS resident_count
FROM cities c
INNER JOIN lives_in li ON c.id = li.target_id
GROUP BY c.name
ORDER BY c.name;
-- EXPECT: row 1: 'London', 1
-- EXPECT: row 2: 'Paris', 2

-- Cleanup: labels can be dropped without affecting data
DROP EDGE LABEL friends;
DROP EDGE LABEL lives_in;
DROP NODE LABEL city;
DROP NODE LABEL person;

-- Data still accessible after label removal
SELECT name FROM persons ORDER BY name;
-- EXPECT: 3 rows: 'Alice', 'Bob', 'Carol'
