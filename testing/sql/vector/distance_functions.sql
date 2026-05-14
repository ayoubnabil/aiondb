-- Test: Vector distance functions
-- Validates l2_distance and cosine_distance computations.

CREATE TABLE vecs (id INT, v VECTOR(3));
INSERT INTO vecs VALUES
    (1, '[1.0,0.0,0.0]'),
    (2, '[0.0,1.0,0.0]'),
    (3, '[1.0,1.0,0.0]');

-- l2_distance: self-distance should be 0
SELECT id, l2_distance(v, v) AS self_dist FROM vecs;
-- EXPECT: 3 rows, all with self_dist = 0.0

-- l2_distance between orthogonal unit vectors
-- Distance between [1,0,0] and [0,1,0] = sqrt(2) ~ 1.4142
SELECT l2_distance(v, v) FROM vecs WHERE id = 1;
-- EXPECT: 0.0 (self-distance)

-- cosine_distance: identical vectors should have distance 0
SELECT id, cosine_distance(v, v) AS self_cos FROM vecs;
-- EXPECT: 3 rows, all with self_cos = 0.0

-- Order by distance (nearest-neighbor query pattern)
-- Find vectors closest to [1,0,0]
SELECT id, l2_distance(v, '[1.0,0.0,0.0]') AS dist
FROM vecs
ORDER BY dist ASC;
-- EXPECT: id=1 first (dist=0), then id=3, then id=2
