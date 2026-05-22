---
title: Vector Reference
order: 43
---

# Vector Reference

AionDB supports fixed-dimension vectors and distance functions for similarity search.

## Define vector columns

For pgvector migrations, the `vector` extension marker is accepted and reports
pgvector-compatible version `0.8.2` in `pg_extension` and
`pg_available_extensions`:

```sql
CREATE EXTENSION IF NOT EXISTS vector;
```

```sql
CREATE TABLE embeddings (
    id INT NOT NULL,
    doc_name TEXT,
    vec VECTOR(4)
);
```

Insert vectors as text literals:

```sql
INSERT INTO embeddings VALUES
    (1, 'intro', '[1.0,0.0,0.0,0.0]'),
    (2, 'chapter1', '[0.9,0.1,0.0,0.0]');
```

The dimension is part of the type. A `VECTOR(4)` value has four coordinates, and comparisons should use vectors with the same dimension.

For pgvector compatibility, `VECTOR` without a dimension is also accepted for casts and storage:

```sql
SELECT vector_dims(CAST('[1.0,2.0,3.0]' AS VECTOR));
SELECT CAST(ARRAY[1.0,2.0,3.0] AS VECTOR(3));
SELECT CAST(CAST('[1.0,2.0,3.0]' AS VECTOR(3)) AS REAL[]);
```

Unconstrained vector columns can hold different dimensions, but distance functions still require the two vectors being compared to have the same runtime dimension. Prefer `VECTOR(n)` for indexed search workloads.

`HALFVEC(n)` is accepted as a pgvector-compatible alias for half-precision vector storage:

```sql
CREATE TABLE compact_embeddings (
    id INT,
    vec HALFVEC(4)
);
```

`SPARSEVEC(n)` is accepted for pgvector migration compatibility. Sparse text
values such as `'{1:1.0,3:2.0}/4'` are expanded into AionDB's dense runtime
vector representation, so exact distance functions and pgvector distance
operators work. Dedicated sparse storage and sparse ANN indexes are not yet
implemented; use `VECTOR` or `HALFVEC` for indexed dense vector workloads.

## Distance functions

| Function | Behaviour | pgvector alias |
| --- | --- | --- |
| `l2_distance(a, b)` | Euclidean distance. Useful when vector magnitude is meaningful. | `vector_l2_ops` for index DDL |
| `cosine_distance(a, b)` | `1 - cos(a, b)`. Useful when direction matters more than magnitude. | `vector_cosine_ops` |
| `inner_product(a, b)` | Plain dot product. The engine also exposes a negated form for ranking (smaller = closer). | `vector_ip_ops` |
| `manhattan_distance(a, b)` | L1 distance. Aliased as `l1_distance(a, b)` for pgvector compatibility. | `vector_l1_ops` |

Use the same metric for indexing and querying whenever possible: HNSW and IVF-flat indexes are built around one metric, and the planner will refuse to substitute a different metric at query time.
The pgvector distance functions are also accepted with explicit `pg_catalog.`
qualification, which keeps ORM-generated SQL portable.

## L2 distance

```sql
SELECT id, doc_name, l2_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
ORDER BY dist ASC
LIMIT 3;

SELECT pg_catalog.l2_distance(vec, '[1.0,0.0,0.0,0.0]')
FROM embeddings;
```

## Cosine distance

```sql
SELECT id, doc_name, cosine_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
ORDER BY dist ASC
LIMIT 3;
```

## Inner product

```sql
SELECT id, doc_name, inner_product(vec, '[1.0,0.0,0.0,0.0]') AS score
FROM embeddings
ORDER BY score DESC
LIMIT 3;
```

The dot product is **maximised** for similar vectors; use `ORDER BY ... DESC` here. The negated form returned by the engine's internal ranking path inverts that so the planner can keep `ORDER BY ... ASC LIMIT k` semantics across every metric.

## Manhattan distance

```sql
SELECT id, doc_name, manhattan_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
ORDER BY dist ASC
LIMIT 3;
```

`l1_distance(...)` is the pgvector-compatible alias and produces the same result.

## Vector aggregates

```sql
SELECT avg(vec) AS centroid, sum(vec) AS component_sum
FROM embeddings;
```

`avg(vector)`, `sum(vector)`, `avg(halfvec)`, and `sum(halfvec)` ignore NULL rows and operate component-wise, matching pgvector's centroid-style aggregate behaviour. All non-null vectors in the group must have the same runtime dimension.

## pgvector casts

Common pgvector casts are accepted and exposed through `pg_cast`/`pg_proc` for
client introspection:

```sql
SELECT ARRAY[1, 2, 3]::integer[]::vector(3);
SELECT ARRAY[1, 2, 3]::integer[]::halfvec(3);
SELECT ARRAY[1, 0, 2]::integer[]::sparsevec(3);
SELECT embedding::real[] FROM embeddings;
SELECT embedding::halfvec(4) FROM embeddings;
SELECT CAST(CAST('[1.0,0.0,2.0]' AS HALFVEC(3)) AS REAL[]);
SELECT embedding::sparsevec(4) FROM embeddings;
SELECT CAST(CAST('{1:1.0,3:2.0}/3' AS SPARSEVEC(3)) AS VECTOR(3));
SELECT CAST(CAST('{1:1.0,3:2.0}/3' AS SPARSEVEC(3)) AS HALFVEC(3));
SELECT array_to_vector(ARRAY[1.0, 0.0, 2.0], 3, true);
SELECT vector_to_float4(embedding, 4, true) FROM embeddings;
SELECT halfvec_to_float4(CAST('[1.0,0.0,2.0]' AS HALFVEC(3)), 3, true);
SELECT array_to_halfvec(ARRAY[1.0, 0.0, 2.0], 3, true);
SELECT vector_to_halfvec(embedding, 4, true) FROM embeddings;
SELECT halfvec_to_vector(embedding, 4, true) FROM embeddings;
SELECT vector_to_sparsevec(embedding, 4, true) FROM embeddings;
SELECT halfvec_to_sparsevec(embedding, 4, true) FROM embeddings;
SELECT sparsevec_to_vector(embedding, 4, true) FROM embeddings;
SELECT sparsevec_to_halfvec(embedding, 4, true) FROM embeddings;
SELECT array_to_sparsevec(ARRAY[1.0, 0.0, 2.0], 3, true);
SELECT l2_norm(CAST('{1:1.0,3:2.0}/3' AS SPARSEVEC(3)));
SELECT l2_normalize(CAST('{1:1.0,3:2.0}/3' AS SPARSEVEC(3)));
SELECT embedding + CAST('[1.0,1.0,1.0]' AS VECTOR(3)) FROM embeddings;
SELECT embedding - CAST('[1.0,0.0,1.0]' AS VECTOR(3)) FROM embeddings;
SELECT embedding * CAST('[2.0,1.0,0.5]' AS VECTOR(3)) FROM embeddings;
SELECT embedding || CAST('[0.0,0.0]' AS VECTOR(2)) FROM embeddings;
SELECT vector_add(embedding, CAST('[1.0,1.0,1.0]' AS VECTOR(3))) FROM embeddings;
SELECT vector_sub(embedding, CAST('[1.0,0.0,1.0]' AS VECTOR(3))) FROM embeddings;
SELECT vector_mul(embedding, CAST('[2.0,1.0,0.5]' AS VECTOR(3))) FROM embeddings;
SELECT vector_concat(embedding, CAST('[0.0,0.0]' AS VECTOR(2))) FROM embeddings;
SELECT halfvec_add(CAST('[1.0,2.0]' AS HALFVEC(2)), CAST('[3.0,4.0]' AS HALFVEC(2)));
SELECT halfvec_concat(CAST('[1.0,2.0]' AS HALFVEC(2)), CAST('[3.0]' AS HALFVEC(1)));
SELECT binary_quantize(embedding)::bit(4) FROM embeddings;
SELECT binary_quantize(CAST('[1.0,-2.0,0.0,0.1]' AS HALFVEC(4)));
SELECT binary_quantize(embedding, 4, true) FROM embeddings;
SELECT vector_out(vector_in('[1.0,0.0,2.0]'));
SELECT sparsevec_out(sparsevec_in('{1:1.0,3:2.0}/3'));
SELECT pg_input_is_valid('[1.0,0.0,2.0]', 'vector(3)');
SELECT pg_input_is_valid('{1:1.0,3:2.0}/3', 'sparsevec(3)');
```

The catalogs also include pgvector-compatible cast metadata for vector,
halfvec, sparsevec, bit helper functions, and pgvector I/O functions so
migration tools can inspect the extension surface. Runtime execution maps
halfvec and sparsevec helpers onto AionDB's dense vector runtime; sparse vector
text is expanded to dense vectors for exact distance operations. PostgreSQL
input validation helpers such as `pg_input_is_valid` accept `vector(n)`,
`halfvec(n)`, and `sparsevec(n)` type names.

Vector and halfvec arithmetic functions (`vector_add`, `vector_sub`,
`vector_mul`, `vector_concat`, `halfvec_add`, `halfvec_sub`, `halfvec_mul`,
and `halfvec_concat`) are exposed for catalog compatibility with the matching
operators. Addition, subtraction, and multiplication require matching runtime
dimensions; concat appends coordinates.

## HNSW indexes

```sql
CREATE INDEX embeddings_vec_hnsw ON embeddings USING hnsw (vec);
```

The optimizer can use an HNSW access path when the query shape and metric match the index. If a query does not use the index, first verify correctness with a brute-force distance query.

## pgvector DDL compatibility

`USING ivfflat` is accepted for pgvector migration compatibility:

```sql
CREATE INDEX embeddings_vec_ivfflat
ON embeddings USING ivfflat (vec vector_cosine_ops)
WITH (lists = 100);
```

The current runtime maps this syntax onto AionDB's vector ANN index implementation while validating `lists` as a positive integer. The pgvector operator classes `vector_l2_ops`, `vector_cosine_ops`, `vector_ip_ops`, and `vector_l1_ops` select the matching distance metric for `hnsw`; `ivfflat` accepts the pgvector-compatible L2, cosine, and inner-product vector classes. The equivalent `halfvec_l2_ops`, `halfvec_cosine_ops`, `halfvec_ip_ops`, and `halfvec_l1_ops` classes are accepted for `HALFVEC` columns, and sparse/bit opclass rows are exposed in the PostgreSQL catalogs for tooling compatibility. Use `l1_distance(vec, query)` and the `<+>` operator as pgvector-compatible aliases for AionDB's `manhattan_distance(vec, query)`. Vector arithmetic operators `+`, `-`, `*`, and `||` are exposed in `pg_operator` for `vector` and `halfvec`, with `pg_proc` rows for the matching implementation functions. The pgvector utility functions `vector_dims(vec)`, `vector_norm(vec)`, `l2_norm(vec)`, `l2_normalize(vec)`, `subvector(vec, start, count)`, `binary_quantize(vector)`, `binary_quantize(halfvec)`, `hamming_distance(bits, bits)`, and `jaccard_distance(bits, bits)` are also available. The bit-distance operators `<~>` and `<%>` work with the text bitstrings returned by `binary_quantize(...)`.

pgvector runtime settings are recognized for migration compatibility:

```sql
SET hnsw.ef_search = 100;
SET hnsw.iterative_scan = relaxed_order;
SET hnsw.max_scan_tuples = 50000;
SET hnsw.scan_mem_multiplier = 2;
SET ivfflat.probes = 10;
SET ivfflat.iterative_scan = strict_order;
SET ivfflat.max_probes = 100;
```

The integer and multiplier settings are validated as positive values, and iterative scan modes accept `off`, `strict_order`, or `relaxed_order`. They are visible through `SHOW`, `current_setting(...)`, and `pg_settings`. `hnsw.ef_search` is used as the default HNSW breadth for direct `ORDER BY <distance>(vec, query) LIMIT k` plans that lower to `HnswScan`, and for `vector_top_k_ids(...)`, `vector_top_k_hits(...)`, and `vector_recommend_top_k_hits(...)` when no explicit `ef_search` argument or JSON option is supplied. `hnsw.max_scan_tuples` caps adaptive HNSW widening for filtered vector helpers and filtered `HnswScan` wrappers. Explicit helper arguments still take priority.
The vector helper JSON options also accept Qdrant-style
`"params":{"hnsw_ef": N, "exact": true}` as aliases for AionDB's flat
`ef_search` and `exact` options.
`limit` and `offset` can be supplied in the same JSON options object; `limit`
overrides the positional `k` argument.
Hit-returning helpers accept `with_payload:false` to omit payload data, or
`with_payload:["column_name", ...]` to include only selected payload columns.
The Qdrant-style object forms `with_payload:{"include":[...]}` and
`with_payload:{"exclude":[...]}` are also accepted.
Use `with_vector:true` or `with_vectors:true` to include the matched vector in
each hit under the `vector` key.

## Brute-force reference query

Keep an exact query for correctness:

```sql
SELECT id, doc_name, l2_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
ORDER BY dist ASC
LIMIT 10;
```

Then compare indexed behavior against it on a dataset where expected neighbors are known.

## Filtered search

Filtered vector search should be tested separately:

```sql
SELECT id, doc_name, l2_distance(vec, '[1.0,0.0,0.0,0.0]') AS dist
FROM embeddings
WHERE doc_name LIKE 'chapter%'
ORDER BY dist ASC
LIMIT 5;
```

The `vector_top_k_ids(...)` and `vector_top_k_hits(...)` JSON options also accept a
Qdrant-style `filter` object with `must`, `should`, and `must_not` arrays. Match
clauses support `match.value`, `match.any`, `match.except`, and `match.text`;
`match.text` performs case-insensitive substring matching over `TEXT`, JSONB
strings, and string array elements. Range clauses support numeric `gt`, `gte`,
`lt`, and `lte` bounds. `values_count` supports
integer `gt`, `gte`, `lt`, and `lte` bounds over SQL arrays and JSONB arrays;
non-array values count as 1, while SQL `NULL` and JSONB `null` count as 0.
For SQL arrays and JSONB arrays, match clauses inspect array elements, matching
Qdrant's payload-array semantics; range clauses also match when any numeric
array element satisfies the bounds. JSONB columns also support dotted keys such
as `payload.city`, `payload.score`, `payload.tags`, or bracketed array paths
such as `payload.tags[]` and `payload.cities[].name` to filter nested fields.
The top-level `min_should` object is supported with `conditions` and
`min_count`. Clause fields accept either an array of conditions or a single
condition object. Top-level shorthand field matches can be mixed with clause
fields; they are appended to `must` conditions. Clause objects without
condition keys can also use shorthand maps such as `"must":{"tag":"news"}`.
`is_null` and `is_empty` payload-field conditions are also accepted for fixed
table columns; `is_empty` matches SQL `NULL`, JSONB `null`, SQL empty arrays,
and JSONB empty arrays.
`has_id` filters against the first column returned by `vector_top_k_ids(...)`,
which is AionDB's point-id equivalent for these helpers:

```sql
SELECT item_id
FROM vector_top_k_ids(
  'items',
  'embedding',
  '[1.0,0.0,0.0]',
  10,
  'l2',
  64,
  NULL,
  NULL,
  NULL,
  '{"filter":{"must":[{"key":"tag","match":{"any":["news","sports"]}},
                      {"key":"payload.city","match":{"value":"paris"}},
                      {"key":"payload.tags[]","match":{"value":"green"}},
                      {"key":"payload.summary","match":{"text":"release notes"}},
                      {"has_id":[1,3,5]},
                      {"key":"status","match":{"except":["deleted"]}}],
              "must_not":[{"key":"source","match":{"value":"spam"}}],
              "should":[{"key":"quality","range":{"gte":0.8}},
                        {"key":"comments","values_count":{"gt":2}},
                        {"is_empty":{"key":"archived_tags"}}],
              "min_should":{"conditions":[{"key":"source","match":{"value":"web"}},
                                          {"key":"priority","range":{"gte":3}}],
                            "min_count":1}}}'::jsonb
) AS hits(item_id);
```

Filtering changes planning. A selective filter may make brute-force scoring over the filtered subset more appropriate than using a vector index first. A low-selectivity filter may favor index-first execution. Record the data distribution when reporting performance.

## Dimension checks

AionDB rejects mismatched vector dimensions. A `VECTOR(4)` column should not be compared to a `VECTOR(3)` value.

Also validate null behavior in your workload. A row with a null vector should not be assumed to participate in similarity ranking unless that behavior is explicitly tested.

## Evaluation guidance

- Measure recall and latency on your own data.
- Test filtered vector search separately from unfiltered top-k search.
- Keep raw benchmark output with the query, index definition, and dataset size.

## Reporting vector results

A useful vector benchmark report includes:

- row count;
- vector dimension;
- metric;
- index definition;
- whether vectors are normalized;
- filter predicate if any;
- requested `LIMIT`;
- recall target or exact reference result;
- raw latency output.
