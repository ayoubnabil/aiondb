-- Test: Graph node label lifecycle
-- Validates CREATE NODE LABEL, DROP NODE LABEL, and
-- that the underlying table persists after label removal.

-- Create backing table
CREATE TABLE persons (id INT NOT NULL, name TEXT);
INSERT INTO persons VALUES (1, 'alice'), (2, 'bob');

-- Create a node label on the table
CREATE NODE LABEL person ON persons;
-- EXPECT: CREATE NODE LABEL success

-- Case-insensitive label creation
CREATE TABLE companies (id INT NOT NULL, name TEXT);
CREATE NODE LABEL Company ON Companies;
-- EXPECT: CREATE NODE LABEL success (case-insensitive match)

-- Drop a node label
DROP NODE LABEL person;
-- EXPECT: DROP NODE LABEL success

-- Underlying table still exists after dropping the label
SELECT name FROM persons;
-- EXPECT: 2 rows: 'alice', 'bob'
