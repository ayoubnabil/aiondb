/* AionDB Dashboard */
'use strict';

var state = {
  sessionId: null,
  csrfToken: null,
  username: null,
  database: null,
  currentTab: 'query',
  queryHistory: [],
  metricsHistory: [],
  metricsInterval: null,
  historyOpen: false,
  lastQueryTime: null,
  lastRowCount: null,
  productInfo: null,
  queryMode: 'sql',
  lastResults: [],
  graph: { nodes: [], edges: [], selected: null, scale: 1, offsetX: 0, offsetY: 0 },
  graphAnimation: null,
};

// ── API ──

async function api(path, body) {
  var payload = Object.assign({}, body);
  if (state.sessionId) {
    payload.session_id = state.sessionId;
    payload.csrf_token = state.csrfToken;
  }
  var resp = await fetch('/api' + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  var data = await resp.json();
  if (resp.status === 401 || resp.status === 403) {
    handleSessionExpired();
    throw new Error(data.error || 'Session expired');
  }
  return data;
}

function handleSessionExpired() {
  state.sessionId = null;
  state.csrfToken = null;
  state.productInfo = null;
  stopMetrics();
  document.getElementById('login-screen').hidden = false;
  document.getElementById('dashboard-screen').hidden = true;
  renderProductInfo();
  var err = document.getElementById('login-error');
  err.textContent = 'Session expired. Please log in again.';
  err.hidden = false;
}

// ── Login ──

document.getElementById('login-form').addEventListener('submit', async function(e) {
  e.preventDefault();
  var errEl = document.getElementById('login-error');
  errEl.hidden = true;
  var btn = document.getElementById('login-btn');
  btn.disabled = true;
  btn.textContent = 'Connecting...';

  var username = document.getElementById('login-user').value.trim();
  var password = document.getElementById('login-pass').value;
  var database = document.getElementById('login-db').value.trim() || 'aiondb';

  try {
    var data = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: username, password: password, database: database }),
    }).then(function(r) { return r.json(); });

    if (data.error) {
      errEl.textContent = data.error;
      errEl.hidden = false;
      btn.disabled = false;
      btn.textContent = 'Connect';
      return;
    }

    state.sessionId = data.session_id;
    state.csrfToken = data.csrf_token;
    state.username = data.username;
    state.database = data.database;

    document.getElementById('login-screen').hidden = true;
    document.getElementById('dashboard-screen').hidden = false;
    document.getElementById('session-info').textContent = state.username + '@' + state.database;
    document.getElementById('status-db').textContent = state.database;
    await loadProductInfo();
    document.getElementById('sql-editor').focus();
    updateGutter();
  } catch (err) {
    errEl.textContent = 'Connection failed';
    errEl.hidden = false;
  }
  btn.disabled = false;
  btn.textContent = 'Connect';
});

// ── Logout ──

document.getElementById('logout-btn').addEventListener('click', async function() {
  try { await api('/auth/logout', {}); } catch (e) {}
  state.sessionId = null;
  state.csrfToken = null;
  state.productInfo = null;
  stopMetrics();
  document.getElementById('login-screen').hidden = false;
  document.getElementById('dashboard-screen').hidden = true;
  document.getElementById('login-error').hidden = true;
  document.getElementById('login-pass').value = '';
  renderProductInfo();
});

async function loadProductInfo() {
  try {
    state.productInfo = await api('/info', {});
  } catch (e) {
    state.productInfo = null;
  }
  renderProductInfo();
}

function renderProductInfo() {
  var el = document.getElementById('status-support');
  if (!el) return;
  if (!state.productInfo) {
    el.textContent = '';
    el.title = '';
    return;
  }

  var deployment = state.productInfo.deployment || {};
  var storage = state.productInfo.storage || {};
  var operations = state.productInfo.operations || {};
  el.textContent =
    'v' + (state.productInfo.release_line || '0.1') +
    ' ' + (deployment.label || 'single-node only') +
    ' / ' + (storage.label || 'unencrypted at rest') +
    ' / ' + (operations.label || 'logical dump/restore');
  el.title = [
    deployment.summary || '',
    storage.summary || '',
    operations.summary || '',
  ].filter(Boolean).join(' ');
}

// ── Tabs ──

document.querySelectorAll('.tab-btn').forEach(function(btn) {
  btn.addEventListener('click', function() { switchTab(btn.dataset.tab); });
});

function switchTab(name) {
  state.currentTab = name;
  document.querySelectorAll('.tab-btn').forEach(function(b) {
    b.classList.toggle('active', b.dataset.tab === name);
  });
  document.querySelectorAll('.tab-pane').forEach(function(p) {
    var active = p.id === 'tab-' + name;
    p.hidden = !active;
    p.classList.toggle('active', active);
  });
  if (name === 'query') { document.getElementById('sql-editor').focus(); updateGutter(); }
  if (name === 'graph') { resizeGraphCanvas(); drawGraph(); }
  if (name === 'schema') loadSchemaTree();
  if (name === 'tools') renderToolConnection();
  if (name === 'metrics') startMetrics();
  else stopMetrics();
}

// ── Syntax Highlighting ──

var KW = new Set([
  'SELECT','FROM','WHERE','INSERT','UPDATE','DELETE','CREATE','DROP','ALTER',
  'TABLE','INDEX','VIEW','JOIN','LEFT','RIGHT','INNER','OUTER','FULL','CROSS',
  'NATURAL','ON','AND','OR','NOT','IN','IS','NULL','AS','ORDER','BY','GROUP',
  'HAVING','LIMIT','OFFSET','UNION','ALL','DISTINCT','INTO','VALUES','SET',
  'BEGIN','COMMIT','ROLLBACK','GRANT','REVOKE','CASCADE','CONSTRAINT','PRIMARY',
  'KEY','FOREIGN','REFERENCES','DEFAULT','CHECK','UNIQUE','EXISTS','BETWEEN',
  'LIKE','ILIKE','CASE','WHEN','THEN','ELSE','END','FUNCTION','RETURNS',
  'LANGUAGE','IF','SCHEMA','SEQUENCE','WITH','RECURSIVE','RETURNING','EXPLAIN',
  'ANALYZE','VACUUM','TRUNCATE','COPY','ROLE','PASSWORD','LOGIN','DATABASE',
  'EXTENSION','TYPE','ENUM','BOOLEAN','INTEGER','TEXT','VARCHAR','CHAR','BIGINT',
  'SMALLINT','REAL','DOUBLE','PRECISION','NUMERIC','DECIMAL','SERIAL',
  'TIMESTAMP','DATE','TIME','INTERVAL','JSON','JSONB','UUID','ARRAY','TRUE',
  'FALSE','ASC','DESC','NULLS','FIRST','LAST','OVER','PARTITION','WINDOW',
  'ROW','ROWS','RANGE','PRECEDING','FOLLOWING','CURRENT','UNBOUNDED','FETCH',
  'NEXT','ONLY','FOR','TRIGGER','EXECUTE','PROCEDURE','MATERIALIZED','REFRESH',
  'CONCURRENTLY','TEMPORARY','TEMP','UNLOGGED','REPLACE','CONFLICT','DO',
  'NOTHING','LATERAL','USING','EXCEPT','INTERSECT','RENAME','ADD','COLUMN',
  'OWNER','TO','RESTRICT','NO','ACTION','DEFERRABLE','INITIALLY','DEFERRED',
  'IMMEDIATE','ENABLE','DISABLE','FORCE','ABORT','ACCESS','CACHE','CYCLE',
  'INCREMENT','MINVALUE','MAXVALUE','START','RESTART','OWNED','NONE',
  'SECURITY','DEFINER','INVOKER','IMMUTABLE','STABLE','VOLATILE','STRICT',
  'CALLED','COST','PARALLEL','SAFE','UNSAFE','AGGREGATE','OPERATOR',
  'MATCH','MERGE','DETACH','NODE','EDGE','LABEL','SOURCE','TARGET','RETURN',
  'UNWIND','OPTIONAL','CALL','YIELD','COLLECT','CREATE','DELETE',
]);

var FN = new Set([
  'COUNT','SUM','AVG','MIN','MAX','COALESCE','CAST','EXTRACT','NULLIF',
  'GREATEST','LEAST','UPPER','LOWER','LENGTH','SUBSTRING','TRIM','REPLACE',
  'CONCAT','NOW','CURRENT_TIMESTAMP','CURRENT_DATE','CURRENT_TIME','AGE',
  'DATE_TRUNC','DATE_PART','TO_CHAR','TO_DATE','TO_NUMBER','TO_TIMESTAMP',
  'ARRAY_AGG','STRING_AGG','JSON_AGG','JSONB_AGG','ROW_NUMBER','RANK',
  'DENSE_RANK','NTILE','LAG','LEAD','FIRST_VALUE','LAST_VALUE','NTH_VALUE',
  'GENERATE_SERIES','UNNEST','ARRAY_LENGTH','ARRAY_APPEND','ARRAY_REMOVE',
  'ABS','CEIL','FLOOR','ROUND','TRUNC','MOD','POWER','SQRT','LOG','LN','EXP',
  'SIGN','RANDOM','MD5','ENCODE','DECODE','PG_SIZE_PRETTY',
]);

function highlight(code) {
  if (!code) return '\n';
  var out = '';
  var i = 0;
  var n = code.length;

  while (i < n) {
    // Line comment
    if (code[i] === '-' && code[i + 1] === '-') {
      var e = code.indexOf('\n', i);
      if (e === -1) e = n;
      out += '<span class="cm">' + esc(code.slice(i, e)) + '</span>';
      i = e;
      continue;
    }
    // Block comment
    if (code[i] === '/' && code[i + 1] === '*') {
      var e = code.indexOf('*/', i + 2);
      if (e === -1) e = n; else e += 2;
      out += '<span class="cm">' + esc(code.slice(i, e)) + '</span>';
      i = e;
      continue;
    }
    // String
    if (code[i] === "'") {
      var j = i + 1;
      while (j < n) {
        if (code[j] === "'" && code[j + 1] === "'") { j += 2; continue; }
        if (code[j] === "'") { j++; break; }
        j++;
      }
      out += '<span class="st">' + esc(code.slice(i, j)) + '</span>';
      i = j;
      continue;
    }
    // Number
    if (/[0-9]/.test(code[i]) && (i === 0 || /[\s,()=<>!+\-*/;]/.test(code[i - 1]))) {
      var j = i;
      while (j < n && /[0-9.eE\-+]/.test(code[j])) j++;
      out += '<span class="nu">' + esc(code.slice(i, j)) + '</span>';
      i = j;
      continue;
    }
    // Word
    if (/[a-zA-Z_]/.test(code[i])) {
      var j = i;
      while (j < n && /[a-zA-Z0-9_]/.test(code[j])) j++;
      var w = code.slice(i, j);
      var u = w.toUpperCase();
      if (KW.has(u)) out += '<span class="kw">' + esc(w) + '</span>';
      else if (FN.has(u)) out += '<span class="fn">' + esc(w) + '</span>';
      else out += esc(w);
      i = j;
      continue;
    }
    out += esc(code[i]);
    i++;
  }
  return out + '\n';
}

// ── Editor ──

var editor = document.getElementById('sql-editor');
var hcode = document.getElementById('highlight-code');
var hlayer = document.getElementById('highlight-layer');
var gutter = document.getElementById('line-gutter');
var statusEl = document.getElementById('query-status');

var SNIPPETS = {
  'sql-basic': [
    'CREATE TABLE users (',
    '    id INT PRIMARY KEY,',
    '    name TEXT NOT NULL',
    ');',
    '',
    "INSERT INTO users VALUES (1, 'alice'), (2, 'bob');",
    '',
    'SELECT id, name FROM users ORDER BY id;',
  ].join('\n'),
  'cypher-match': [
    'MATCH (d:doc)-[:related_doc]->(next:doc)',
    'RETURN d.id AS source_id, next.id AS target_id, d.title AS source_label, next.title AS target_label',
    'LIMIT 50;',
  ].join('\n'),
  'graph-model': [
    'CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, embedding VECTOR(2));',
    'CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL, relation TEXT);',
    '',
    'CREATE NODE LABEL doc ON docs;',
    'CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;',
  ].join('\n'),
  'hybrid-vector': [
    'MATCH (d:doc)-[:related_doc]->(next:doc)',
    "RETURN d.id AS source_id, next.id AS target_id, d.title AS source_label, next.title AS target_label, l2_distance(next.embedding, '[1.0,0.0]') AS dist",
    'ORDER BY dist ASC',
    'LIMIT 20;',
  ].join('\n'),
};

function updateHL() { hcode.innerHTML = highlight(editor.value); }

function updateGutter() {
  var lines = (editor.value || '').split('\n');
  var cur = editor.value.substring(0, editor.selectionStart).split('\n').length;
  var h = '';
  for (var i = 1; i <= lines.length; i++) {
    h += '<span class="ln' + (i === cur ? ' cur' : '') + '">' + i + '</span>';
  }
  gutter.innerHTML = h;
}

function syncScroll() {
  hlayer.scrollTop = editor.scrollTop;
  hlayer.scrollLeft = editor.scrollLeft;
  gutter.scrollTop = editor.scrollTop;
}

editor.addEventListener('input', function() { updateHL(); updateGutter(); });
editor.addEventListener('scroll', syncScroll);
editor.addEventListener('click', updateGutter);
editor.addEventListener('keyup', updateGutter);

document.getElementById('run-query').addEventListener('click', runQuery);
document.getElementById('preview-graph').addEventListener('click', async function() {
  var data = await runQuery();
  if (data && data.results) {
    renderGraphFromResults(data.results);
    switchTab('graph');
  }
});
document.getElementById('clear-editor').addEventListener('click', function() {
  editor.value = '';
  updateHL();
  updateGutter();
  editor.focus();
});

document.querySelectorAll('.seg-btn').forEach(function(btn) {
  btn.addEventListener('click', function() {
    state.queryMode = btn.dataset.mode;
    document.querySelectorAll('.seg-btn').forEach(function(b) {
      b.classList.toggle('active', b === btn);
    });
    updateHL();
    editor.focus();
  });
});

document.getElementById('snippet-select').addEventListener('change', function(e) {
  if (!e.target.value) return;
  insertSnippet(e.target.value);
  e.target.value = '';
});

document.querySelectorAll('.snippet-btn').forEach(function(btn) {
  btn.addEventListener('click', function() {
    insertSnippet(btn.dataset.snippet);
    switchTab('query');
  });
});

function insertSnippet(name) {
  var text = SNIPPETS[name];
  if (!text) return;
  editor.value = text;
  state.queryMode = name.indexOf('cypher') === 0 || name === 'hybrid-vector' ? 'cypher' : 'sql';
  document.querySelectorAll('.seg-btn').forEach(function(b) {
    b.classList.toggle('active', b.dataset.mode === state.queryMode);
  });
  updateHL();
  updateGutter();
  editor.focus();
}

editor.addEventListener('keydown', function(e) {
  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
    e.preventDefault();
    runQuery();
  }
  if (e.key === 'Tab') {
    e.preventDefault();
    var s = editor.selectionStart;
    var end = editor.selectionEnd;
    editor.value = editor.value.substring(0, s) + '    ' + editor.value.substring(end);
    editor.selectionStart = editor.selectionEnd = s + 4;
    updateHL();
    updateGutter();
  }
});

async function runQuery() {
  var sql = editor.value.trim();
  if (!sql) return null;

  statusEl.textContent = 'Executing...';
  var container = document.getElementById('results-container');

  try {
    var data = await api('/query', { sql: sql });

    if (data.error && !data.results) {
      container.innerHTML = '';
      container.appendChild(mkError(data.error));
      statusEl.textContent = 'ERROR (' + (data.elapsed_ms || 0) + ' ms)';
      state.lastQueryTime = data.elapsed_ms || 0;
      updateStatus();
      addHistory(sql);
      return data;
    }

    container.innerHTML = '';
    var totalRows = 0;
    if (data.results) {
      state.lastResults = data.results;
      data.results.forEach(function(r) {
        container.appendChild(mkResult(r));
        if (r.row_count) totalRows += r.row_count;
        if (r.rows_affected) totalRows += r.rows_affected;
      });
    }
    if (data.error) container.appendChild(mkError(data.error));

    state.lastQueryTime = data.elapsed_ms || 0;
    state.lastRowCount = totalRows;
    statusEl.textContent = totalRows + ' rows (' + (data.elapsed_ms || 0) + ' ms)';
    updateStatus();
    addHistory(sql);
    return data;
  } catch (err) {
    statusEl.textContent = 'FAILED';
    return null;
  }
}

function addHistory(sql) {
  state.queryHistory.push({ sql: sql, time: new Date().toISOString() });
  if (state.queryHistory.length > 100) state.queryHistory.shift();
  if (state.historyOpen) renderHistory();
}

function mkResult(r) {
  var block = document.createElement('div');
  block.className = 'result-block';

  if (r.type === 'query') {
    var hdr = document.createElement('div');
    hdr.className = 'result-header';
    hdr.textContent = r.row_count + ' row' + (r.row_count !== 1 ? 's' : '') +
      (r.truncated ? ' (truncated)' : '');
    block.appendChild(hdr);

    if (r.columns && r.columns.length > 0) {
      var wrap = document.createElement('div');
      wrap.className = 'result-table-wrap';
      var tbl = document.createElement('table');
      tbl.className = 'result-table';

      var thead = document.createElement('thead');
      var hr = document.createElement('tr');
      r.columns.forEach(function(col, i) {
        var th = document.createElement('th');
        th.textContent = col;
        th.title = r.column_types ? r.column_types[i] : '';
        hr.appendChild(th);
      });
      thead.appendChild(hr);
      tbl.appendChild(thead);

      var tbody = document.createElement('tbody');
      r.rows.forEach(function(row) {
        var tr = document.createElement('tr');
        row.forEach(function(val) {
          var td = document.createElement('td');
          if (val === null) td.innerHTML = '<span class="null-value">NULL</span>';
          else if (typeof val === 'object') td.textContent = JSON.stringify(val);
          else td.textContent = String(val);
          tr.appendChild(td);
        });
        tbody.appendChild(tr);
      });
      tbl.appendChild(tbody);
      wrap.appendChild(tbl);
      block.appendChild(wrap);
    }
  } else if (r.type === 'command') {
    var d = document.createElement('div');
    d.className = 'result-command';
    d.textContent = r.tag + (r.rows_affected > 0 ? ' (' + r.rows_affected + ' rows)' : '');
    block.appendChild(d);
  } else if (r.type === 'notice') {
    var d = document.createElement('div');
    d.className = 'result-notice';
    d.textContent = r.message;
    block.appendChild(d);
  }
  return block;
}

function mkError(msg) {
  var d = document.createElement('div');
  d.className = 'result-error';
  d.textContent = msg;
  return d;
}

// ── History ──

document.getElementById('history-toggle').addEventListener('click', toggleHistory);
document.getElementById('history-close').addEventListener('click', toggleHistory);

function toggleHistory() {
  state.historyOpen = !state.historyOpen;
  document.getElementById('history-panel').hidden = !state.historyOpen;
  if (state.historyOpen) renderHistory();
}

function renderHistory() {
  var list = document.getElementById('history-list');
  if (state.queryHistory.length === 0) {
    list.innerHTML = '<div class="history-empty">No queries yet.</div>';
    return;
  }
  var h = '';
  for (var i = state.queryHistory.length - 1; i >= 0; i--) {
    var it = state.queryHistory[i];
    var t = new Date(it.time).toLocaleTimeString();
    h += '<div class="history-item" data-i="' + i + '">' +
      '<div class="history-item-time">' + esc(t) + '</div>' +
      '<div class="history-item-sql">' + esc(it.sql) + '</div></div>';
  }
  list.innerHTML = h;
  list.querySelectorAll('.history-item').forEach(function(el) {
    el.addEventListener('click', function() {
      editor.value = state.queryHistory[parseInt(el.dataset.i)].sql;
      updateHL();
      updateGutter();
      editor.focus();
    });
  });
}

// ── Resize ──

(function() {
  var bar = document.getElementById('resize-bar');
  var panel = document.getElementById('editor-panel');
  var pane = document.getElementById('split-pane');
  var startY, startH;

  bar.addEventListener('mousedown', function(e) {
    e.preventDefault();
    startY = e.clientY;
    startH = panel.offsetHeight;
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  });

  function onMove(e) {
    var h = Math.max(60, Math.min(startH + e.clientY - startY, pane.offsetHeight - 60));
    panel.style.height = h + 'px';
  }

  function onUp() {
    document.removeEventListener('mousemove', onMove);
    document.removeEventListener('mouseup', onUp);
  }
})();

// ── Schema ──

async function loadSchemaTree() {
  var tree = document.getElementById('schema-tree');
  tree.innerHTML = '<div class="placeholder-text">Loading...</div>';

  try {
    var sd = await api('/schema/schemas', {});
    var sel = document.getElementById('schema-select');
    var cur = sel.value;
    sel.innerHTML = '';
    if (sd.schemas) {
      sd.schemas.forEach(function(s) {
        var o = document.createElement('option');
        o.value = s.name;
        o.textContent = s.name;
        if (s.name === cur) o.selected = true;
        sel.appendChild(o);
      });
    }
    await loadSchemaObjects(sel.value);
  } catch (e) {
    tree.innerHTML = '<div class="placeholder-text error">Failed to load schema.</div>';
  }
}

document.getElementById('schema-select').addEventListener('change', function(e) {
  loadSchemaObjects(e.target.value);
});
document.getElementById('refresh-schema').addEventListener('click', loadSchemaTree);

async function loadSchemaObjects(schema) {
  var tree = document.getElementById('schema-tree');
  tree.innerHTML = '';

  try {
    var r = await Promise.all([
      api('/schema/tables', { schema: schema }),
      api('/schema/sequences', { schema: schema }),
      api('/schema/views', { schema: schema }),
      api('/schema/functions', { schema: schema }),
    ]);

    if (r[0].tables && r[0].tables.length > 0) {
      tree.appendChild(mkTreeSection('Tables (' + r[0].tables.length + ')',
        r[0].tables.map(function(t) {
          return { label: t.name, fn: function() { loadTableDetail(schema, t.name); } };
        })));
    }
    if (r[2].views && r[2].views.length > 0) {
      tree.appendChild(mkTreeSection('Views (' + r[2].views.length + ')',
        r[2].views.map(function(v) {
          return { label: v.name, fn: function() { loadViewDetail(schema, v.name, v.view_definition); } };
        })));
    }
    if (r[1].sequences && r[1].sequences.length > 0) {
      tree.appendChild(mkTreeSection('Sequences (' + r[1].sequences.length + ')',
        r[1].sequences.map(function(s) {
          return { label: s.name, fn: function() { showDetail('Sequence: ' + s.name); } };
        })));
    }
    if (r[3].functions && r[3].functions.length > 0) {
      tree.appendChild(mkTreeSection('Functions (' + r[3].functions.length + ')',
        r[3].functions.map(function(f) {
          return { label: f.name + '(' + (f.language || '?') + ')', fn: function() { showDetail('Function: ' + f.name); } };
        })));
    }

    if (tree.children.length === 0)
      tree.innerHTML = '<div class="placeholder-text">No objects in this schema.</div>';
  } catch (e) {
    tree.innerHTML = '<div class="placeholder-text error">Error loading objects.</div>';
  }
}

function mkTreeSection(title, items) {
  var sec = document.createElement('div');
  sec.className = 'tree-section';

  var hd = document.createElement('div');
  hd.className = 'tree-section-hd';
  hd.innerHTML = '<span class="tree-chevron">&#9660;</span> ' + esc(title);
  sec.appendChild(hd);

  var list = document.createElement('div');
  list.className = 'tree-items';
  items.forEach(function(it) {
    var btn = document.createElement('button');
    btn.className = 'tree-item';
    btn.textContent = it.label;
    btn.addEventListener('click', function() {
      document.querySelectorAll('.tree-item').forEach(function(b) { b.classList.remove('sel'); });
      btn.classList.add('sel');
      it.fn();
    });
    list.appendChild(btn);
  });
  sec.appendChild(list);

  var collapsed = false;
  hd.addEventListener('click', function() {
    collapsed = !collapsed;
    list.hidden = collapsed;
    hd.querySelector('.tree-chevron').innerHTML = collapsed ? '&#9654;' : '&#9660;';
  });

  return sec;
}

async function loadTableDetail(schema, table) {
  var det = document.getElementById('schema-detail');
  det.innerHTML = '<div class="placeholder-text">Loading...</div>';

  try {
    var r = await Promise.all([
      api('/schema/columns', { schema: schema, table: table }),
      api('/schema/indexes', { schema: schema, table: table }),
      api('/schema/constraints', { schema: schema, table: table }),
    ]);

    var h = '<h3>' + esc(schema) + '.' + esc(table) + '</h3>';

    if (r[0].columns && r[0].columns.length > 0) {
      h += '<div class="detail-section"><h4>Columns (' + r[0].columns.length + ')</h4>';
      h += '<table class="result-table"><thead><tr><th>Name</th><th>Type</th><th>Nullable</th><th>Default</th></tr></thead><tbody>';
      r[0].columns.forEach(function(c) {
        h += '<tr><td><b>' + esc(c.name) + '</b></td><td>' + esc(c.data_type || '') +
          '</td><td>' + esc(c.is_nullable || '') + '</td><td>' + esc(c.column_default || '') + '</td></tr>';
      });
      h += '</tbody></table></div>';
    }

    if (r[1].indexes && r[1].indexes.length > 0) {
      h += '<div class="detail-section"><h4>Indexes (' + r[1].indexes.length + ')</h4>';
      h += '<table class="result-table"><thead><tr><th>Name</th><th>Unique</th><th>Primary</th></tr></thead><tbody>';
      r[1].indexes.forEach(function(x) {
        h += '<tr><td>' + esc(x.index_name) + '</td><td>' + (x.is_unique ? 'YES' : 'NO') +
          '</td><td>' + (x.is_primary ? 'YES' : 'NO') + '</td></tr>';
      });
      h += '</tbody></table></div>';
    }

    if (r[2].constraints && r[2].constraints.length > 0) {
      h += '<div class="detail-section"><h4>Constraints (' + r[2].constraints.length + ')</h4>';
      h += '<table class="result-table"><thead><tr><th>Name</th><th>Type</th></tr></thead><tbody>';
      r[2].constraints.forEach(function(c) {
        h += '<tr><td>' + esc(c.name) + '</td><td>' + esc(c.constraint_type || '') + '</td></tr>';
      });
      h += '</tbody></table></div>';
    }

    det.innerHTML = h;
  } catch (e) {
    det.innerHTML = '<div class="placeholder-text error">Failed to load table details.</div>';
  }
}

function loadViewDetail(schema, name, def) {
  var h = '<h3>' + esc(schema) + '.' + esc(name) + ' (view)</h3>';
  if (def) h += '<div class="detail-section"><h4>Definition</h4><pre>' + esc(def) + '</pre></div>';
  document.getElementById('schema-detail').innerHTML = h;
}

function showDetail(title) {
  document.getElementById('schema-detail').innerHTML = '<h3>' + esc(title) + '</h3>';
}

// ── Metrics ──

function startMetrics() {
  if (state.metricsInterval) return;
  fetchMetrics();
  state.metricsInterval = setInterval(fetchMetrics, 5000);
}

function stopMetrics() {
  if (state.metricsInterval) { clearInterval(state.metricsInterval); state.metricsInterval = null; }
}

async function fetchMetrics() {
  try {
    var d = await api('/metrics', {});
    renderMetricsTable(d);
    state.metricsHistory.push(Object.assign({}, d, { timestamp: Date.now() }));
    if (state.metricsHistory.length > 60) state.metricsHistory.shift();
    renderChart();
  } catch (e) {}
}

function renderMetricsTable(d) {
  var tbody = document.getElementById('metrics-tbody');
  var rows = [
    ['Total Queries', fmt(d.queries_total)],
    ['Failed Queries', fmt(d.queries_failed)],
    ['Rows Returned', fmt(d.rows_returned_total)],
    ['Rows Affected', fmt(d.rows_affected_total)],
    ['Active Sessions', fmt(d.active_sessions)],
    ['Dashboard Sessions', fmt(d.dashboard_sessions)],
    ['Avg Query Time', d.queries_total > 0
      ? (d.query_duration_micros_total / d.queries_total / 1000).toFixed(2) + ' ms' : '0 ms'],
    ['Graph DDL Ops', fmt(d.graph_ddl_operations)],
  ];
  tbody.innerHTML = rows.map(function(r) {
    return '<tr><td>' + r[0] + '</td><td>' + r[1] + '</td></tr>';
  }).join('');
}

function renderChart() {
  var canvas = document.getElementById('metrics-canvas');
  var ctx = canvas.getContext('2d');
  var hist = state.metricsHistory;
  if (hist.length < 2) return;

  var dpr = window.devicePixelRatio || 1;
  var rect = canvas.parentElement.getBoundingClientRect();
  canvas.width = rect.width * dpr;
  canvas.height = 250 * dpr;
  canvas.style.width = rect.width + 'px';
  canvas.style.height = '250px';
  ctx.scale(dpr, dpr);
  var cw = rect.width, ch = 250;

  ctx.clearRect(0, 0, cw, ch);

  var qps = [];
  for (var i = 1; i < hist.length; i++) {
    var dt = (hist[i].timestamp - hist[i - 1].timestamp) / 1000;
    var dq = hist[i].queries_total - hist[i - 1].queries_total;
    qps.push(dt > 0 ? dq / dt : 0);
  }
  if (qps.length === 0) return;

  var max = Math.max.apply(null, qps.concat([1]));
  var pL = 50, pR = 16, pT = 16, pB = 24;
  var pW = cw - pL - pR, pH = ch - pT - pB;

  // Grid
  ctx.strokeStyle = '#ddd';
  ctx.lineWidth = 1;
  ctx.fillStyle = '#888';
  ctx.font = '10px system-ui, sans-serif';
  ctx.textAlign = 'right';
  for (var i = 0; i <= 4; i++) {
    var y = pT + pH * (1 - i / 4);
    ctx.beginPath(); ctx.moveTo(pL, y); ctx.lineTo(pL + pW, y); ctx.stroke();
    ctx.fillText((max * i / 4).toFixed(1), pL - 6, y + 3);
  }

  // Line
  ctx.strokeStyle = '#326690';
  ctx.lineWidth = 2;
  ctx.lineJoin = 'round';
  ctx.beginPath();
  qps.forEach(function(v, i) {
    var x = pL + (i / (qps.length - 1 || 1)) * pW;
    var y = pT + pH * (1 - v / max);
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();

  // Label
  ctx.fillStyle = '#666';
  ctx.font = '11px system-ui, sans-serif';
  ctx.textAlign = 'center';
  ctx.fillText('queries/sec (5s interval)', cw / 2, ch - 4);
}

// ── Graph preview ──

document.getElementById('graph-run-current').addEventListener('click', async function() {
  var data = await runQuery();
  if (data && data.results) renderGraphFromResults(data.results);
});

document.getElementById('graph-draw-last').addEventListener('click', function() {
  renderGraphFromResults(state.lastResults);
});

document.getElementById('graph-fit').addEventListener('click', function() {
  fitGraph();
  drawGraph();
});

window.addEventListener('resize', function() {
  if (state.currentTab === 'graph') {
    resizeGraphCanvas();
    drawGraph();
  }
});

function renderGraphFromResults(results) {
  var graph = inferGraph(results || []);
  state.graph.nodes = graph.nodes;
  state.graph.edges = graph.edges;
  state.graph.selected = null;
  seedGraphLayout();
  fitGraph();
  updateGraphStats();
  renderInspector(null);
  drawGraph();
}

function inferGraph(results) {
  var nodes = {};
  var edges = [];
  var queryResults = (results || []).filter(function(r) { return r.type === 'query'; });

  queryResults.forEach(function(r) {
    var cols = (r.columns || []).map(function(c) { return String(c).toLowerCase(); });
    (r.rows || []).forEach(function(row, rowIndex) {
      var obj = {};
      cols.forEach(function(c, i) { obj[c] = row[i]; });
      var srcIdx = firstIndex(cols, ['source_id', 'src_id', 'from_id', 'source', 'src', 'from', 'in']);
      var dstIdx = firstIndex(cols, ['target_id', 'dst_id', 'to_id', 'target', 'dst', 'to', 'out']);
      if (srcIdx >= 0 && dstIdx >= 0 && row[srcIdx] !== null && row[dstIdx] !== null) {
        var sid = String(row[srcIdx]);
        var tid = String(row[dstIdx]);
        var sl = pickObj(obj, ['source_label', 'src_label', 'from_label', 'source_name', 'src_name']) || sid;
        var tl = pickObj(obj, ['target_label', 'dst_label', 'to_label', 'target_name', 'dst_name']) || tid;
        ensureNode(nodes, sid, sl, pickObj(obj, ['source_type', 'source_label_type']) || 'node');
        ensureNode(nodes, tid, tl, pickObj(obj, ['target_type', 'target_label_type']) || 'node');
        edges.push({ id: 'e' + edges.length, source: sid, target: tid, label: pickObj(obj, ['edge', 'edge_label', 'relation', 'type']) || '' });
        return;
      }

      var stringCells = row.filter(function(v) {
        return v !== null && (typeof v === 'string' || typeof v === 'number');
      });
      if (stringCells.length >= 2) {
        var a = String(stringCells[0]);
        var b = String(stringCells[1]);
        ensureNode(nodes, a, a, cols[0] || 'node');
        ensureNode(nodes, b, b, cols[1] || 'node');
        edges.push({ id: 'e' + edges.length, source: a, target: b, label: '' });
      } else if (stringCells.length === 1) {
        var id = String(stringCells[0]);
        ensureNode(nodes, id, id, cols[0] || 'node');
      } else if (row.length > 0) {
        var fallback = 'row-' + rowIndex;
        ensureNode(nodes, fallback, fallback, 'row');
      }
    });
  });

  return { nodes: Object.keys(nodes).map(function(k) { return nodes[k]; }), edges: edges };
}

function firstIndex(cols, names) {
  for (var i = 0; i < names.length; i++) {
    var idx = cols.indexOf(names[i]);
    if (idx >= 0) return idx;
  }
  return -1;
}

function pickObj(obj, keys) {
  for (var i = 0; i < keys.length; i++) {
    if (obj[keys[i]] !== undefined && obj[keys[i]] !== null) return String(obj[keys[i]]);
  }
  return '';
}

function ensureNode(nodes, id, label, type) {
  if (!nodes[id]) nodes[id] = { id: id, label: String(label || id), type: String(type || 'node'), x: 0, y: 0, vx: 0, vy: 0 };
  return nodes[id];
}

function seedGraphLayout() {
  var nodes = state.graph.nodes;
  var count = Math.max(1, nodes.length);
  nodes.forEach(function(n, i) {
    var angle = (i / count) * Math.PI * 2;
    var radius = 140 + Math.min(140, count * 4);
    n.x = Math.cos(angle) * radius;
    n.y = Math.sin(angle) * radius;
    n.vx = 0;
    n.vy = 0;
  });
  relaxGraph(120);
}

function relaxGraph(iterations) {
  var nodes = state.graph.nodes;
  var edges = state.graph.edges;
  var byId = {};
  nodes.forEach(function(n) { byId[n.id] = n; });
  for (var step = 0; step < iterations; step++) {
    for (var i = 0; i < nodes.length; i++) {
      for (var j = i + 1; j < nodes.length; j++) {
        var a = nodes[i], b = nodes[j];
        var dx = b.x - a.x, dy = b.y - a.y;
        var d2 = Math.max(16, dx * dx + dy * dy);
        var force = 900 / d2;
        var d = Math.sqrt(d2);
        var fx = (dx / d) * force;
        var fy = (dy / d) * force;
        a.x -= fx; a.y -= fy; b.x += fx; b.y += fy;
      }
    }
    edges.forEach(function(e) {
      var s = byId[e.source], t = byId[e.target];
      if (!s || !t) return;
      var dx = t.x - s.x, dy = t.y - s.y;
      var d = Math.max(1, Math.sqrt(dx * dx + dy * dy));
      var desired = 150;
      var pull = (d - desired) * 0.025;
      var fx = (dx / d) * pull;
      var fy = (dy / d) * pull;
      s.x += fx; s.y += fy; t.x -= fx; t.y -= fy;
    });
  }
}

function resizeGraphCanvas() {
  var canvas = document.getElementById('graph-canvas');
  var parent = canvas.parentElement;
  var rect = parent.getBoundingClientRect();
  var dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(320, rect.width) * dpr;
  canvas.height = Math.max(240, rect.height - 28) * dpr;
  canvas.style.width = Math.max(320, rect.width) + 'px';
  canvas.style.height = Math.max(240, rect.height - 28) + 'px';
}

function fitGraph() {
  var nodes = state.graph.nodes;
  if (!nodes.length) return;
  resizeGraphCanvas();
  var canvas = document.getElementById('graph-canvas');
  var w = canvas.clientWidth, h = canvas.clientHeight;
  var xs = nodes.map(function(n) { return n.x; });
  var ys = nodes.map(function(n) { return n.y; });
  var minX = Math.min.apply(null, xs), maxX = Math.max.apply(null, xs);
  var minY = Math.min.apply(null, ys), maxY = Math.max.apply(null, ys);
  var spanX = Math.max(1, maxX - minX), spanY = Math.max(1, maxY - minY);
  state.graph.scale = Math.min(1.6, Math.max(0.35, Math.min((w - 80) / spanX, (h - 80) / spanY)));
  state.graph.offsetX = w / 2 - ((minX + maxX) / 2) * state.graph.scale;
  state.graph.offsetY = h / 2 - ((minY + maxY) / 2) * state.graph.scale;
}

function drawGraph() {
  resizeGraphCanvas();
  var canvas = document.getElementById('graph-canvas');
  var ctx = canvas.getContext('2d');
  var dpr = window.devicePixelRatio || 1;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  var w = canvas.clientWidth, h = canvas.clientHeight;
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, w, h);

  if (!state.graph.nodes.length) {
    ctx.fillStyle = '#999';
    ctx.font = '12px system-ui, sans-serif';
    ctx.fillText('Run a graph query or draw the last result.', 16, 24);
    document.getElementById('graph-status').textContent = 'No graph loaded';
    return;
  }

  var byId = {};
  state.graph.nodes.forEach(function(n) { byId[n.id] = n; });
  ctx.lineWidth = 1;
  state.graph.edges.forEach(function(e) {
    var s = byId[e.source], t = byId[e.target];
    if (!s || !t) return;
    var sx = toScreenX(s.x), sy = toScreenY(s.y);
    var tx = toScreenX(t.x), ty = toScreenY(t.y);
    ctx.strokeStyle = '#9aa7b2';
    ctx.beginPath();
    ctx.moveTo(sx, sy);
    ctx.lineTo(tx, ty);
    ctx.stroke();
    drawArrow(ctx, sx, sy, tx, ty);
    if (e.label) {
      ctx.fillStyle = '#667';
      ctx.font = '10px system-ui, sans-serif';
      ctx.fillText(e.label, (sx + tx) / 2 + 4, (sy + ty) / 2 - 4);
    }
  });

  state.graph.nodes.forEach(function(n) {
    var x = toScreenX(n.x), y = toScreenY(n.y);
    var selected = state.graph.selected && state.graph.selected.kind === 'node' && state.graph.selected.id === n.id;
    ctx.fillStyle = selected ? '#0f5c99' : colorForType(n.type);
    ctx.strokeStyle = selected ? '#07395f' : '#4d6275';
    ctx.lineWidth = selected ? 2 : 1;
    ctx.beginPath();
    ctx.arc(x, y, 18, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = '#1f2933';
    ctx.font = '11px system-ui, sans-serif';
    ctx.textAlign = 'center';
    ctx.fillText(shortLabel(n.label), x, y + 34);
  });

  document.getElementById('graph-status').textContent = state.graph.nodes.length + ' nodes / ' + state.graph.edges.length + ' edges';
}

function toScreenX(x) { return x * state.graph.scale + state.graph.offsetX; }
function toScreenY(y) { return y * state.graph.scale + state.graph.offsetY; }
function toWorldX(x) { return (x - state.graph.offsetX) / state.graph.scale; }
function toWorldY(y) { return (y - state.graph.offsetY) / state.graph.scale; }

function drawArrow(ctx, sx, sy, tx, ty) {
  var angle = Math.atan2(ty - sy, tx - sx);
  var endX = tx - Math.cos(angle) * 19;
  var endY = ty - Math.sin(angle) * 19;
  ctx.fillStyle = '#9aa7b2';
  ctx.beginPath();
  ctx.moveTo(endX, endY);
  ctx.lineTo(endX - Math.cos(angle - 0.45) * 8, endY - Math.sin(angle - 0.45) * 8);
  ctx.lineTo(endX - Math.cos(angle + 0.45) * 8, endY - Math.sin(angle + 0.45) * 8);
  ctx.closePath();
  ctx.fill();
}

function colorForType(type) {
  var palette = ['#9bd3ff', '#b8e0c7', '#f7d794', '#d8c3ff', '#f3b7b7', '#b7d7d8'];
  var h = 0;
  for (var i = 0; i < String(type).length; i++) h = (h * 31 + String(type).charCodeAt(i)) % palette.length;
  return palette[h];
}

function shortLabel(label) {
  label = String(label || '');
  return label.length > 22 ? label.slice(0, 19) + '...' : label;
}

function updateGraphStats() {
  document.getElementById('graph-node-count').textContent = state.graph.nodes.length;
  document.getElementById('graph-edge-count').textContent = state.graph.edges.length;
}

function renderInspector(item) {
  var el = document.getElementById('graph-inspector');
  if (!item) {
    el.innerHTML = '<div class="panel-hd">Inspector</div><div class="placeholder-text">Select a node or edge.</div>';
    return;
  }
  var h = '<div class="panel-hd">Inspector</div><table class="kv-table">';
  Object.keys(item).forEach(function(k) {
    if (typeof item[k] === 'object') return;
    h += '<tr><th>' + esc(k) + '</th><td>' + esc(item[k]) + '</td></tr>';
  });
  h += '</table>';
  el.innerHTML = h;
}

(function initGraphMouse() {
  var canvas = document.getElementById('graph-canvas');
  var dragging = null;
  canvas.addEventListener('mousedown', function(e) {
    var rect = canvas.getBoundingClientRect();
    var x = e.clientX - rect.left;
    var y = e.clientY - rect.top;
    var hit = hitNode(x, y);
    if (hit) {
      dragging = hit;
      state.graph.selected = { kind: 'node', id: hit.id };
      renderInspector(hit);
      drawGraph();
    }
  });
  canvas.addEventListener('mousemove', function(e) {
    if (!dragging) return;
    var rect = canvas.getBoundingClientRect();
    dragging.x = toWorldX(e.clientX - rect.left);
    dragging.y = toWorldY(e.clientY - rect.top);
    drawGraph();
  });
  document.addEventListener('mouseup', function() { dragging = null; });
})();

function hitNode(x, y) {
  for (var i = state.graph.nodes.length - 1; i >= 0; i--) {
    var n = state.graph.nodes[i];
    var dx = toScreenX(n.x) - x;
    var dy = toScreenY(n.y) - y;
    if (dx * dx + dy * dy <= 22 * 22) return n;
  }
  return null;
}

// ── External PostgreSQL tools ──

['tool-host', 'tool-port', 'tool-db', 'tool-user'].forEach(function(id) {
  var el = document.getElementById(id);
  if (el) el.addEventListener('input', renderToolConnection);
});

document.getElementById('copy-conn').addEventListener('click', function() {
  var text = document.getElementById('tool-conn-string').textContent;
  if (navigator.clipboard) navigator.clipboard.writeText(text);
});

function renderToolConnection() {
  var host = document.getElementById('tool-host').value || '127.0.0.1';
  var port = document.getElementById('tool-port').value || '5432';
  var db = document.getElementById('tool-db').value || state.database || 'default';
  var user = document.getElementById('tool-user').value || state.username || 'dev';
  document.getElementById('tool-conn-string').textContent =
    'postgresql://' + encodeURIComponent(user) + ':<password>@' + host + ':' + port + '/' + encodeURIComponent(db) + '?sslmode=disable';
}

// ── Statusbar ──

function updateStatus() {
  var r = document.getElementById('status-rows');
  var t = document.getElementById('status-time');
  var sep = document.getElementById('status-sep-time');
  if (state.lastRowCount !== null) r.textContent = state.lastRowCount + ' rows';
  if (state.lastQueryTime !== null) {
    t.textContent = state.lastQueryTime + ' ms';
    sep.hidden = false;
  }
}

// ── Keyboard shortcuts ──

document.addEventListener('keydown', function(e) {
  if (document.getElementById('dashboard-screen').hidden) return;
  var ctrl = e.ctrlKey || e.metaKey;

  if (ctrl && e.key === '1') { e.preventDefault(); switchTab('query'); }
  if (ctrl && e.key === '2') { e.preventDefault(); switchTab('graph'); }
  if (ctrl && e.key === '3') { e.preventDefault(); switchTab('schema'); }
  if (ctrl && e.key === '4') { e.preventDefault(); switchTab('metrics'); }
  if (ctrl && e.key === '5') { e.preventDefault(); switchTab('tools'); }
  if (ctrl && e.key === 'l' && state.currentTab === 'query') {
    e.preventDefault();
    editor.value = '';
    updateHL();
    updateGutter();
    editor.focus();
  }
  if (ctrl && e.key === 'h' && state.currentTab === 'query') {
    e.preventDefault();
    toggleHistory();
  }
});

// ── Utils ──

function esc(s) {
  if (s === null || s === undefined) return '';
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function fmt(n) {
  if (n === null || n === undefined) return '0';
  return Number(n).toLocaleString();
}

// ── Init ──
updateHL();
updateGutter();
renderToolConnection();
updateGraphStats();
