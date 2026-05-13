\set k random(1, :scale * 100)
SELECT v FROM bench_kv WHERE k = :k;
