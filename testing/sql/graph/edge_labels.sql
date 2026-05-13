-- Test: Graph edge label lifecycle
-- Validates CREATE EDGE LABEL with source/target node labels,
-- different endpoint types, and DROP EDGE LABEL.

-- Setup node tables and labels
CREATE TABLE persons (id INT NOT NULL, name TEXT);
CREATE TABLE companies (id INT NOT NULL, name TEXT);
CREATE TABLE knows_edges (source_id INT NOT NULL, target_id INT NOT NULL);
CREATE TABLE works_at_edges (source_id INT NOT NULL, target_id INT NOT NULL);

CREATE NODE LABEL person ON persons;
CREATE NODE LABEL company ON companies;

-- Create edge label (same source and target type)
CREATE EDGE LABEL knows ON knows_edges SOURCE person TARGET person;
-- EXPECT: CREATE EDGE LABEL success

-- Create edge label (different source and target types)
CREATE EDGE LABEL works_at ON works_at_edges SOURCE person TARGET company;
-- EXPECT: CREATE EDGE LABEL success

-- Drop edge label
DROP EDGE LABEL knows;
-- EXPECT: DROP EDGE LABEL success

-- Underlying edge table still exists
SELECT * FROM knows_edges;
-- EXPECT: 0 rows (table exists but is empty)

-- Drop edge label with different endpoints
DROP EDGE LABEL works_at;
-- EXPECT: DROP EDGE LABEL success
