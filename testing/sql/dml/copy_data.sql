-- Test: COPY TO/FROM operations
-- Validates tab-delimited text export and import, NULL handling,
-- special character escaping, and data type support.
--
-- NOTE: COPY FROM STDIN requires the pgwire sub-protocol or engine API
-- for data delivery. These SQL statements show the command syntax;
-- actual data is delivered out-of-band.

-- Setup
CREATE TABLE items (id INT, name TEXT);
INSERT INTO items VALUES (1, 'apple'), (2, 'banana');

-- COPY TO exports tab-delimited text
COPY items TO STDOUT;
-- EXPECT: CopyOut with 2 lines
-- EXPECT: each line has 2 tab-separated fields
-- EXPECT: data contains 'apple' and 'banana'

-- COPY FROM imports tab-delimited text
CREATE TABLE import_target (id INT, name TEXT);
COPY import_target FROM STDIN;
-- EXPECT: CopyIn marker with table_id and 2 columns (id, name)
-- Data supplied via API: "1\tapple\n2\tbanana\n3\tcherry\n"
-- EXPECT: COPY 3 (3 rows affected)

SELECT id, name FROM import_target;
-- EXPECT: 3 rows

-- COPY FROM with NULL values (\N in PostgreSQL text format)
CREATE TABLE nullable_import (id INT, name TEXT);
COPY nullable_import FROM STDIN;
-- Data: "1\t\\N\n\\N\tbanana\n"
-- EXPECT: COPY 2
-- EXPECT: row 1: id=1, name=NULL
-- EXPECT: row 2: id=NULL, name='banana'

-- COPY TO escapes special characters
CREATE TABLE docs (id INT, content TEXT);
INSERT INTO docs VALUES (1, 'line1
line2'), (2, 'col1	col2');

COPY docs TO STDOUT;
-- EXPECT: newlines escaped as \n, tabs escaped as \t in text values
-- EXPECT: output has exactly 2 physical lines (one per row)

-- COPY TO represents NULL as \N
CREATE TABLE null_export (id INT, name TEXT);
INSERT INTO null_export VALUES (1, NULL);

COPY null_export TO STDOUT;
-- EXPECT: output line: "1\t\\N"

-- COPY roundtrip preserves data
CREATE TABLE roundtrip_src (id INT, name TEXT, flag BOOLEAN);
INSERT INTO roundtrip_src VALUES (1, 'hello', true), (2, 'world', false), (3, NULL, NULL);

COPY roundtrip_src TO STDOUT;
-- Export data, then import into a new table:
CREATE TABLE roundtrip_dst (id INT, name TEXT, flag BOOLEAN);
COPY roundtrip_dst FROM STDIN;
-- EXPECT: source and destination data match exactly
