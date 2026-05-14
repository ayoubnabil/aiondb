#!/usr/bin/env bash
# Repro for SELECT FOR UPDATE SKIP LOCKED concurrency. Spins up an ephemeral
# aiondb, runs N concurrent workers that try to claim jobs via the canonical
# SELECT FOR UPDATE SKIP LOCKED + UPDATE pattern, and reports the
# duplicate-claim count.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${AIONDB_REPRO_PORT:-16542}"
WORKERS="${REPRO_WORKERS:-4}"
DURATION="${REPRO_DURATION:-3}"
JOBS="${REPRO_JOBS:-100}"
USER_NAME="${REPRO_USER:-xtask}"
PASSWORD="${REPRO_PASSWORD:-Xtask-Secret1!}"
DATA_DIR="${REPRO_DATA_DIR:-/tmp/aion-skip-locked-repro}"
BIN="${AIONDB_BIN:-target/release/aiondb}"

if [[ ! -x "$BIN" ]]; then
    echo "[repro] building aiondb-server (release)" >&2
    cargo build --release -p aiondb-server --bin aiondb >&2
fi

cleanup() {
    if [[ -n "${AIONDB_PID:-}" ]] && kill -0 "$AIONDB_PID" 2>/dev/null; then
        kill "$AIONDB_PID" 2>/dev/null || true
        wait "$AIONDB_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

rm -rf "$DATA_DIR"
mkdir -p "$DATA_DIR"

echo "[repro] starting ephemeral aiondb on 127.0.0.1:$PORT" >&2
AIONDB_PGWIRE_LISTEN_ADDR="127.0.0.1:$PORT" \
AIONDB_ALLOW_UNENCRYPTED_STORAGE=true \
AIONDB_BOOTSTRAP_USER="$USER_NAME" \
AIONDB_BOOTSTRAP_PASSWORD="$PASSWORD" \
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=0 \
"$BIN" --ephemeral >"$DATA_DIR/aiondb.log" 2>&1 &
AIONDB_PID=$!

# Wait for listener
for _ in $(seq 1 30); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
        exec 3<&-; exec 3>&-
        break
    fi
    sleep 0.2
done
if ! (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    echo "[repro] aiondb did not start within 6s" >&2
    tail -20 "$DATA_DIR/aiondb.log" >&2
    exit 1
fi
exec 3<&-; exec 3>&- 2>/dev/null || true

DSN="host=127.0.0.1 port=$PORT user=$USER_NAME password=$PASSWORD dbname=default"

python3 - <<PYEOF "$DSN" "$WORKERS" "$DURATION" "$JOBS"
import collections, random, statistics, sys, threading, time
import psycopg

dsn, workers, duration, jobs = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4])

with psycopg.connect(dsn, autocommit=True) as c, c.cursor() as cur:
    cur.execute("DROP TABLE IF EXISTS qjobs")
    cur.execute("CREATE TABLE qjobs (id INT PRIMARY KEY, status TEXT NOT NULL, worker TEXT)")
    cur.executemany("INSERT INTO qjobs VALUES (%s, 'pending', NULL)", [(i,) for i in range(1, jobs + 1)])

stop = threading.Event()
lock = threading.Lock()
claims = {}
dup = [0]
errs = collections.Counter()

def worker(wid):
    name = f"w{wid}"
    conn = psycopg.connect(dsn, autocommit=False)
    while not stop.is_set():
        try:
            with conn.cursor() as cur:
                cur.execute("BEGIN")
                cur.execute("SELECT id FROM qjobs WHERE status='pending' ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED")
                row = cur.fetchone()
                if row is not None:
                    cid = row[0]
                    cur.execute("UPDATE qjobs SET status='running', worker=%s WHERE id=%s", (name, cid))
                cur.execute("COMMIT")
            if row is None:
                time.sleep(0.005)
                continue
            with lock:
                seen = claims.setdefault(row[0], set())
                if seen and name not in seen:
                    dup[0] += 1
                seen.add(name)
        except Exception as e:
            try:
                with conn.cursor() as cur: cur.execute("ROLLBACK")
            except Exception: pass
            with lock:
                errs[type(e).__name__] += 1
    conn.close()

ths = [threading.Thread(target=worker, args=(i,), daemon=True) for i in range(workers)]
for t in ths: t.start()
time.sleep(duration)
stop.set()
for t in ths: t.join(timeout=5)

print(f"workers={workers} duration={duration}s jobs={jobs} claimed={len(claims)} duplicates={dup[0]} errors={dict(errs)}")
PYEOF
