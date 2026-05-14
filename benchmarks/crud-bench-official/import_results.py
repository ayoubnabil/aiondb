#!/usr/bin/env python3
from __future__ import annotations
import argparse, csv, json, re, subprocess
from pathlib import Path

DATA_RE = re.compile(r'(<script id="bench-results-data" type="application/json">)(.*?)(</script>)', re.DOTALL)
ENGINE_FILES = {
    'aiondb': ('AionDB', 'crudbench-aiondb.csv', ('aiondb.json', 'result-crudbench-aiondb.json')),
    'surrealdb': ('SurrealDB WS', 'crudbench-surrealdb.csv', ('surrealdb.json', 'result-crudbench-surrealdb.json')),
    'pgstack': ('PostgreSQL stack', 'crudbench-pgstack.csv', ('pgstack.json', 'result-crudbench-pgstack.json')),
}

def parse_float(s):
    if not s or s == '-':
        return None
    if str(s).strip().upper() in {'UNSUPPORTED', 'TIMEOUT', 'FAIL'}:
        return None
    return float(str(s).replace(',', '').replace(' ms','').strip())

def category_for(test):
    if test.startswith('[C]') or test.startswith('[R]ead') or test.startswith('[U]') or test.startswith('[D]'):
        return 'crud'
    if test.startswith('[B]atch'):
        return 'batch'
    if test.startswith('[I]ndex') or '::indexed' in test or test.startswith('[R]emoveIndex'):
        if 'fulltext' in test:
            return 'fulltext'
        return 'index'
    return 'scan'

def read_engine_csv(path):
    out = {}
    with path.open(newline='', encoding='utf-8') as f:
        for row in csv.DictReader(f):
            test = row['Test']
            status_token = str(row.get('Status') or row.get('OPS') or '').strip().upper()
            if status_token in {'UNSUPPORTED', 'TIMEOUT', 'FAIL'}:
                out[test] = {'status': status_token, 'ops': 0.0, 'mean_ms': None}
                continue
            ops = parse_float(row.get('OPS'))
            mean = parse_float(row.get('Mean'))
            if ops is None:
                out[test] = {'status':'UNSUPPORTED','ops':0.0,'mean_ms':None}
            else:
                out[test] = {'status':'OK','ops':ops,'mean_ms':mean}
    return out

def metric_row(metric):
    if not metric:
        return {'status':'UNSUPPORTED','ops':0.0,'mean_ms':None}
    ops = metric.get('ops')
    mean = metric.get('mean')
    if ops is None:
        return {'status':'UNSUPPORTED','ops':0.0,'mean_ms':None}
    mean_ms = (float(mean) / 1000.0) if mean is not None else None
    return {'status':'OK','ops':float(ops),'mean_ms':mean_ms}

def read_engine_json(path):
    data = json.loads(path.read_text(encoding='utf-8'))
    rows = {}
    for key, test in (
        ('creates', '[C]reate'),
        ('reads', '[R]ead'),
        ('updates', '[U]pdate'),
        ('deletes', '[D]elete'),
    ):
        if key in data:
            rows[test] = metric_row(data.get(key))
    for scan in data.get('scans', []) or []:
        name = scan.get('name')
        samples = scan.get('samples')
        if not name or samples is None:
            continue
        rows[f'[S]can::{name} ({samples})'] = metric_row(scan.get('without_index'))
        if scan.get('has_index_spec') or scan.get('index_build') or scan.get('with_index'):
            rows[f'[I]ndex::{name}'] = metric_row(scan.get('index_build'))
            rows[f'[S]can::{name}::indexed ({samples})'] = metric_row(scan.get('with_index'))
            rows[f'[R]emoveIndex::{name}'] = metric_row(scan.get('index_remove'))
    for batch in data.get('batches', []) or []:
        if isinstance(batch, list):
            if len(batch) < 4:
                continue
            name, samples, batch_size, metric = batch[:4]
        elif isinstance(batch, dict):
            name = batch.get('name')
            samples = batch.get('samples')
            batch_size = batch.get('batch_size')
            metric = batch.get('result') or batch.get('metrics') or batch.get('without_index') or batch
        else:
            continue
        if name:
            if samples is not None and batch_size is not None:
                suffix = f' ({samples} batches of {batch_size})'
            else:
                suffix = f' ({samples})' if samples is not None else ''
            rows[f'[B]atch::{name}{suffix}'] = metric_row(metric)
    return rows

def read_existing_payload(docs_page):
    text = docs_page.read_text(encoding='utf-8')
    m = DATA_RE.search(text)
    if not m:
        raise SystemExit(f'missing benchmark JSON script in {docs_page}')
    try:
        payload = json.loads(m.group(2))
    except json.JSONDecodeError:
        return text, m, {}
    existing = {}
    for item in payload.get('tests', []):
        test = item.get('test')
        if not test:
            continue
        existing[test] = item.get('engines', {})
    return text, m, existing

def better_row(new_row, old_row):
    if old_row is None:
        return new_row
    new_ok = new_row.get('status') == 'OK'
    old_ok = old_row.get('status') == 'OK'
    if new_ok and not old_ok:
        return new_row
    if new_ok and old_ok and float(new_row.get('ops') or 0.0) > float(old_row.get('ops') or 0.0):
        return new_row
    return old_row

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('run_dir', type=Path)
    ap.add_argument('--run-id', required=True)
    ap.add_argument('--docs-page', type=Path, default=Path('docs/content/documentation/evaluate/benchmark-results.md'))
    ap.add_argument('--build-site', action='store_true')
    args = ap.parse_args()
    text, m, existing_rows = read_existing_payload(args.docs_page)
    engine_rows = {}
    all_tests = set(existing_rows)
    for engine, (_, csv_filename, json_filenames) in ENGINE_FILES.items():
        csv_path = args.run_dir / csv_filename
        json_path = next((args.run_dir / name for name in json_filenames if (args.run_dir / name).exists()), None)
        if csv_path.exists():
            rows = read_engine_csv(csv_path)
        elif json_path is not None:
            rows = read_engine_json(json_path)
        else:
            rows = {}
        engine_rows[engine] = rows
        all_tests.update(rows)
    tests = []
    status_counts = {}
    for test in sorted(all_tests):
        engines = {}
        for engine in ENGINE_FILES:
            old_row = existing_rows.get(test, {}).get(engine)
            new_row = engine_rows[engine].get(test)
            row = better_row(new_row, old_row) if new_row is not None else old_row
            if row is None:
                row = {'status':'UNSUPPORTED','ops':0.0,'mean_ms':None}
            engines[engine] = row
            status_counts[row['status']] = status_counts.get(row['status'], 0) + 1
        tests.append({'test': test, 'category': category_for(test), 'engines': engines})
    payload = {
        'run_id': args.run_id,
        'methodology': 'official-surrealdb-crud-bench',
        'measured': len(tests) * len(ENGINE_FILES),
        'expected': len(tests) * len(ENGINE_FILES),
        'status_counts': status_counts,
        'engines': [
            {'id':'aiondb','label':'AionDB'},
            {'id':'surrealdb','label':'SurrealDB WS'},
            {'id':'pgstack','label':'PostgreSQL stack'},
        ],
        'tests': tests,
    }
    data = json.dumps(payload, ensure_ascii=False, separators=(',', ':'), allow_nan=False).replace('</','<\\/')
    args.docs_page.write_text(DATA_RE.sub(m.group(1)+data+m.group(3), text, count=1), encoding='utf-8')
    if args.build_site:
        subprocess.run(['python3','build.py'], cwd='docs', check=True)
    print(f'imported {len(tests)} official crud-bench tests from {args.run_dir}')
if __name__ == '__main__':
    main()
