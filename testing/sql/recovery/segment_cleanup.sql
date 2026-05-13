-- Test: WAL segment rotation and cleanup
-- Validates that old WAL segments are removed after checkpoint,
-- and that the database remains consistent.

-- Step 1: Generate enough WAL entries to trigger segment rotation
CREATE TABLE segment_test (id INT, payload TEXT);

-- Insert many rows to fill WAL segments
INSERT INTO segment_test VALUES (1, 'row_1');
INSERT INTO segment_test VALUES (2, 'row_2');
INSERT INTO segment_test VALUES (3, 'row_3');
INSERT INTO segment_test VALUES (4, 'row_4');
INSERT INTO segment_test VALUES (5, 'row_5');

-- Step 2: Checkpoint should clean up old segments
-- (Engine API: checkpoint())
-- EXPECT: old WAL segments before checkpoint LSN are removed

-- Step 3: Verify data is still accessible
SELECT COUNT(*) AS cnt FROM segment_test;
-- EXPECT: cnt = 5

-- Step 4: Continue writing after cleanup
INSERT INTO segment_test VALUES (6, 'row_6');

SELECT id, payload FROM segment_test ORDER BY id;
-- EXPECT: 6 rows, all present and correct
