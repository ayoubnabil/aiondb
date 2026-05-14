# Cross Engine Compare

Benchmark autonome pour comparer AionDB, SurrealDB, PostgreSQL et CockroachDB sur un jeu de donnees relationnel avec 224 cas generes automatiquement:

- insert append
- lookup ponctuel
- range scan
- group by / rollup
- joins relationnels
- update slice
- top-k vectoriel
- requetes hybrides vecteur + filtre relationnel

Le crate fonctionne sans toucher au workspace racine.

## Moteurs

- `aiondb_embedded`: actif par defaut
- `surrealdb_embedded_mem`: actif par defaut
- `postgresql`: actif si `POSTGRES_BENCH_URL` est defini
- `cockroachdb`: actif si `COCKROACH_BENCH_URL` est defini

CockroachDB et PostgreSQL passent par `tokio-postgres` en `NoTls`, donc les URL locales typiques sont par exemple:

```bash
export POSTGRES_BENCH_URL='postgresql://postgres@127.0.0.1/postgres?sslmode=disable'
export COCKROACH_BENCH_URL='postgresql://root@127.0.0.1:26257/defaultdb?sslmode=disable'
```

## Execution

```bash
cd benchmarks/cross-engine-compare
cargo run --release
```

Profils disponibles:

- `smoke`
- `medium` par defaut
- `large`
- `xlarge`

Variables utiles:

```bash
export AIONDB_COMPARE_PROFILE=xlarge
export AIONDB_COMPARE_WARMUP=2
export AIONDB_COMPARE_MEASURE=8
export AIONDB_COMPARE_BATCH=400
export AIONDB_COMPARE_OUT=../results/cross-engine-compare.json
export SURREAL_BENCH_URL='mem://?sync=never'
export AIONDB_COMPARE_ENGINES='aiondb_embedded,surrealdb_embedded_mem,postgresql,cockroachdb'
```

Le rapport sort en JSON avec `status`, `ops/s`, `avg_ms`, `p95_ms`, checksum, erreurs et metadata de run.
