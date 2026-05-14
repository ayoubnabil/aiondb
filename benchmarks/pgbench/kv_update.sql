\set k random(1, :scale * 100)
UPDATE bench_kv SET v = v + 1 WHERE k = :k;
