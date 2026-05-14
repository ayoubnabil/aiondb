-- Test: Vector similarity search with indexing
-- Validates that vector indexes can be created and used
-- for approximate nearest-neighbor queries.

CREATE TABLE embeddings (
    id INT NOT NULL,
    doc_name TEXT,
    vec VECTOR(4)
);

INSERT INTO embeddings VALUES
    (1, 'intro',    '[1.0, 0.0, 0.0, 0.0]'),
    (2, 'chapter1', '[0.9, 0.1, 0.0, 0.0]'),
    (3, 'chapter2', '[0.0, 0.0, 1.0, 0.0]'),
    (4, 'appendix', '[0.0, 0.0, 0.0, 1.0]'),
    (5, 'glossary', '[0.5, 0.5, 0.0, 0.0]');

-- Brute-force nearest-neighbor search (no index)
SELECT id, doc_name, l2_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
ORDER BY dist ASC
LIMIT 3;
-- EXPECT: top 3 closest to [1,0,0,0]
-- EXPECT: id=1 (dist=0), id=2 (dist~0.14), id=5 (dist~0.71)

-- Cosine similarity search
SELECT id, doc_name, cosine_distance(vec, '[1.0,0.0,0.0,0.0]') AS cos_dist
FROM embeddings
ORDER BY cos_dist ASC
LIMIT 3;
-- EXPECT: id=1 (cos_dist=0), then id=2, then id=5
