var appInfo             = {};
var appFeatures         = {};
var editor              = null;
var connected           = false;
var bookmarks           = {};
var default_rows_limit  = 100;
var currentObject       = null;
var autocompleteObjects = [];
var inputResizing       = false;
var inputResizeOffset   = null;
var lastQueryResults    = null;
var graphState          = { nodes: [], edges: [], selected: null, scale: 1, offsetX: 0, offsetY: 0 };
var maxSessionIdLength  = 256;
var maxStoredQueryLength = 65536;

var aiondbSnippets = {
  cypher_match: [
    "MATCH (d:doc)-[:related_doc]->(next:doc)",
    "RETURN d.id AS source_id, next.id AS target_id, d.title AS source_label, next.title AS target_label",
    "LIMIT 50;"
  ].join("\n"),
  graph_schema: [
    "CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, embedding VECTOR(2));",
    "CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL, relation TEXT);",
    "",
    "CREATE NODE LABEL doc ON docs;",
    "CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc;"
  ].join("\n"),
  hybrid_vector: [
    "MATCH (d:doc)-[:related_doc]->(next:doc)",
    "RETURN d.id AS source_id, next.id AS target_id, d.title AS source_label, next.title AS target_label, l2_distance(next.embedding, '[1.0,0.0]') AS dist",
    "ORDER BY dist ASC",
    "LIMIT 20;"
  ].join("\n"),
  pgwire_smoke: [
    "SELECT 1 AS pgwire_ok, current_database() AS database_name;",
    "SELECT table_schema, table_name FROM information_schema.tables ORDER BY table_schema, table_name LIMIT 50;"
  ].join("\n\n")
};

var filterOptions = {
  "equal":      "= 'DATA'",
  "not_equal":  "!= 'DATA'",
  "greater":    "> 'DATA'" ,
  "greater_eq": ">= 'DATA'",
  "less":       "< 'DATA'",
  "less_eq":    "<= 'DATA'",
  "like":       "LIKE 'DATA'",
  "ilike":      "ILIKE 'DATA'",
  "null":       "IS NULL",
  "not_null":   "IS NOT NULL"
};

function getSessionId() {
  var id = sessionStorage.getItem("session_id");

  if (!isValidSessionId(id)) {
    id = guid();
    sessionStorage.setItem("session_id", id);
  }

  return id;
}

function isValidSessionId(id) {
  return typeof id == "string" && id.length > 0 && id.length <= maxSessionIdLength;
}

function setRowsLimit(num) {
  num = parseInt(num, 10);
  if (!Number.isFinite(num) || num < 1 || num > 100000) {
    num = default_rows_limit;
  }
  localStorage.setItem("rows_limit", num);
}

function getRowsLimit() {
  var num = parseInt(localStorage.getItem("rows_limit") || default_rows_limit, 10);
  if (!Number.isFinite(num) || num < 1 || num > 100000) {
    return default_rows_limit;
  }
  return num;
}

function getPaginationOffset() {
  var page  = $(".current-page").data("page");
  var limit = getRowsLimit();
  return (page - 1) * limit;
}

function getPagesCount(rowsCount) {
  var limit = getRowsLimit();
  var num = parseInt(rowsCount / limit);

  if ((num * limit) < rowsCount) {
    num++;
  }

  return num;
}

function apiCall(method, path, params, cb) {
  var timeout = appFeatures.query_timeout;
  if (timeout == null) {
    timeout = 300; // in seconds
  }

  $.ajax({
    timeout: timeout * 1000, // in milliseconds
    url: "api" + path,
    method: method,
    cache: false,
    data: params,
    headers: {
      "x-session-id": getSessionId()
    },
    success: cb,
    error: function(xhr, status, data) {
      switch(status) {
        case "error":
          if (xhr.readyState == 0) { // 0 = UNSENT
            showErrorBanner("Sorry, something went wrong with your request. Refresh the page and try again!");
          }
          break;
        case "timeout":
          return cb({ error: "Query timeout after " + timeout + "s" });
      }

      var responseText;
      try {
        responseText = jQuery.parseJSON(xhr.responseText);
      }
      catch {
        responseText = { error: "Failed to parse the JSON response." };
      }
      cb(responseText);
    }
  });
}

function getInfo(cb)                        { apiCall("get", "/info", {}, cb); }
function getConnection(cb)                  { apiCall("get", "/connection", {}, cb); }
function getServerSettings(cb)              { apiCall("get", "/server_settings", {}, cb); }
function getSchemas(cb)                     { apiCall("get", "/schemas", {}, cb); }
function getObjects(cb)                     { apiCall("get", "/objects", {}, cb); }
function getTables(cb)                      { apiCall("get", "/tables", {}, cb); }
function getTableRows(table, opts, cb)      { apiCall("get", "/tables/" + encodeURIComponent(table) + "/rows", opts, cb); }
function getTableStructure(table, opts, cb) { apiCall("get", "/tables/" + encodeURIComponent(table), opts, cb); }
function getTableIndexes(table, cb)         { apiCall("get", "/tables/" + encodeURIComponent(table) + "/indexes", {}, cb); }
function getTableConstraints(table, cb)     { apiCall("get", "/tables/" + encodeURIComponent(table) + "/constraints", {}, cb); }
function getTablesStats(cb)                 { apiCall("get", "/tables_stats", {}, cb); }
function getFunction(id, cb)                { apiCall("get", "/functions/" + encodeURIComponent(id), {}, cb); }
function getHistory(cb)                     { apiCall("get", "/history", {}, cb); }
function getBookmarks(cb)                   { apiCall("get", "/bookmarks", {}, cb); }
function executeQuery(query, cb)            { apiCall("post", "/query", { query: query }, cb); }
function explainQuery(query, cb)            { apiCall("post", "/explain", { query: query }, cb); }
function analyzeQuery(query, cb)            { apiCall("post", "/analyze", { query: query }, cb); }
function disconnect(cb)                     { apiCall("post", "/disconnect", {}, cb); }

function encodeQuery(query) {
  return Base64.encode(query).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, ".");
}

function showErrorBanner(text) {
  if (window.errBannerTimeout != null) {
    clearTimeout(window.errBannerTimeout);
  }

  window.errBannerTimeout = setTimeout(function() {
    $("#error_banner").fadeOut("fast").text("");
  }, 3000);

  $("#error_banner").text(text).show();
}

function buildSchemaSection(name, objects) {
  var section = "";

  var titles = {
    "table":             "Tables",
    "view":              "Views",
    "materialized_view": "Materialized Views",
    "function":          "Functions",
    "sequence":          "Sequences"
  };

  var icons = {
    "table":             '<i class="fa fa-table"></i>',
    "view":              '<i class="fa fa-table"></i>',
    "materialized_view": '<i class="fa fa-table"></i>',
    "function":          '<i class="fa fa-bolt"></i>',
    "sequence":          '<i class="fa fa-circle-o"></i>'
  };

  var klass = "";
  if (name == "public") klass = "expanded";

  section += "<div class='schema " + klass + "'>";
  section += "<div class='schema-name'><i class='fa fa-folder-o'></i><i class='fa fa-folder-open-o'></i> " + escapeHtml(name) + "</div>";
  section += "<div class='schema-container'>";

  ["table", "view", "materialized_view", "function", "sequence"].forEach(function(group) {
    group_klass = "";
    if (name == "public" && group == "table") group_klass = "expanded";

    section += "<div class='schema-group " + group_klass + "'>";
    section += "<div class='schema-group-title'><i class='fa fa-chevron-right'></i><i class='fa fa-chevron-down'></i> " + titles[group] + " <span class='schema-group-count'>" + objects[group].length + "</span></div>";
    section += "<ul data-group='" + group + "'>";

    if (objects[group]) {
      objects[group].forEach(function(item) {
        var id = name + "." + item.name;

        // Use function OID since multiple functions with the same name might exist
        if (group == "function") {
          id = item.oid;
        }

        section += "<li class='schema-item schema-" + group + "' data-type='" + group + "' data-id='" + escapeAttribute(id) + "' data-schema='" + escapeAttribute(name) + "' data-name='" + escapeAttribute(item.name) + "'>" + icons[group] + "&nbsp;" + escapeHtml(item.name) + "</li>";
      });
      section += "</ul></div>";
    }
  });

  section += "</div></div>";

  return section;
}

function loadLocalQueries() {
  if (!appFeatures.local_queries) return;

  $("body").on("click", "a.load-local-query", function(e) {
    var id = $(this).data("id");

    apiCall("get", "/local_queries/" + id, {}, function(resp) {
      editor.setValue(resp.query);
      editor.clearSelection();
    });
  });

  apiCall("get", "/local_queries", {}, function(resp) {
    if (resp.error) return;

    var container = $("#load-query-dropdown").find(".dropdown-menu");

    resp.forEach(function(item) {
      var title = item.title || item.id;
      $("<li/>").append(
        $("<a/>").
          attr("href", "#").
          addClass("load-local-query").
          attr("data-id", item.id).
          text(title)
      ).appendTo(container);
    });

    if (resp.length > 0) $("#load-local-query").prop("disabled", "");
    $("#load-query-dropdown").show();
  });
}

function loadSchemas() {
  $("#objects").html("");

  var emptyObjectList = function() {
    return {
      table: [],
      view: [],
      materialized_view: [],
      function: [],
      sequence: []
    }
  }

  getSchemas(function(schemasData) {
    if (schemasData.error) {
      alert("Error while fetching schemas: " + schemasData.error);
      return;
    }

    getObjects(function(data) {
      if (data.error) {
        alert("Error while fetching database objects: " + data.error);
        return;
      }

      if (Object.keys(data).length == 0) {
        data["public"] = emptyObjectList();
      }

      for (schemaName of schemasData) {
        // Allow users to see empty schemas if we dont have any objects in them
        if (!data[schemaName]) {
          data[schemaName] = emptyObjectList();
        }

        $(buildSchemaSection(schemaName, data[schemaName])).appendTo("#objects");
      }

      if (Object.keys(data).length == 1) {
        $(".schema").addClass("expanded");
      }

      // Clear out all autocomplete objects
      autocompleteObjects = [];
      for (schema in data) {
        for (kind in data[schema]) {
          if (!(kind == "table" || kind == "view" || kind == "materialized_view" || kind == "function")) {
            continue
          }

          for (item in data[schema][kind]) {
            autocompleteObjects.push({
              caption: data[schema][kind][item].name,
              value: data[schema][kind][item].name,
              meta: kind
            });
          }
        }
      }

      bindContextMenus();
    });
  });
}

function escapeHtml(str) {
  if (str !== null && str !== undefined) {
    return jQuery("<div/>").text(str).html();
  }

  return "<span class='null'>null</span>";
}

function escapeAttribute(str) {
  if (str === null || str === undefined) {
    return "";
  }

  return jQuery("<div/>").text(String(str)).html();
}

function quoteSqlIdentifier(name) {
  return '"' + String(name).replace(/"/g, '""') + '"';
}

function quoteSqlIdentifierPath(path) {
  return String(path).split(".").map(quoteSqlIdentifier).join(".");
}

function quoteSqlLiteral(value) {
  return "'" + String(value).replace(/'/g, "''") + "'";
}

function quoteSqlLiteralValue(value) {
  return String(value).replace(/'/g, "''");
}

function getCurrentObject() {
  return currentObject || { name: "", type: "" };
}

function normalizeHostName(host) {
  host = String(host || "").trim().toLowerCase();
  if (host[0] == "[" && host[host.length - 1] == "]") {
    host = host.slice(1, -1);
  }
  return host;
}

function isIPv4LoopbackHost(host) {
  var parts = host.split(".");
  if (parts.length != 4 || parts[0] != "127") return false;

  for (var i = 0; i < parts.length; i++) {
    if (!/^\d+$/.test(parts[i])) return false;
    var octet = parseInt(parts[i], 10);
    if (octet < 0 || octet > 255) return false;
  }

  return true;
}

function isLoopbackHostName(host) {
  host = normalizeHostName(host);
  return host == "localhost" || host == "::1" || isIPv4LoopbackHost(host);
}

function isLoopbackConnectionURL(str) {
  try {
    return isLoopbackHostName(new URL(str).hostname);
  } catch (e) {
    return false;
  }
}

function resetTable() {
  $("#results_header").html("");
  $("#results_body").html("");
  $("#results_view").html("").hide();

  $("#results").
    data("mode", "").
    removeClass("empty").
    removeClass("no-crop").
    show();
}

function performTableAction(table, action, el) {
  if (action == "truncate" || action == "delete") {
    var message = "Are you sure you want to " + action + " table " + table + " ?";
    if (!confirm(message)) return;
  }

  switch(action) {
    case "truncate":
      executeQuery("TRUNCATE TABLE " + table, function(data) {
        if (data.error) alert(data.error);
        resetTable();
      });
      break;
    case "delete":
      executeQuery("DROP TABLE " + table, function(data) {
        if (data.error) alert(data.error);
        loadSchemas();
        resetTable();
      });
      break;
    case "export":
      var format = el.data("format");
      var db = $("#current_database").text();
      var filename = db + "." + table + "." + format;
      var query = "SELECT * FROM " + table;
      openPostInNewWindow("api/query", { "format": format, "filename": filename, "query": query });
      break;
    case "dump":
      openPostInNewWindow("api/export", { "table": table });
      break;
    case "copy":
      copyToClipboard(table.split('.')[1]);
      break;
    case "analyze":
      executeQuery("ANALYZE " + table, function(data) {
        if (data.error) alert(data.error);
        resetTable();
      });
      break;
  }
}

function performViewAction(view, action, el) {
  if (action == "delete") {
    var message = "Are you sure you want to " + action + " view " + view + " ?";
    if (!confirm(message)) return;
  }

  switch(action) {
    case "delete":
      executeQuery("DROP VIEW " + view, function(data) {
        if (data.error) alert(data.error);
        loadSchemas();
        resetTable();
      });
      break;
    case "export":
      var format = el.data("format");
      var db = $("#current_database").text();
      var filename = db + "." + view + "." + format;
      var query = "SELECT * FROM " + view;
      openPostInNewWindow("api/query", { "format": format, "filename": filename, "query": query });
      break;
    case "copy":
      copyToClipboard(view.split('.')[1]);
      break;
    case "copy_def":
      executeQuery("SELECT pg_get_viewdef(" + quoteSqlLiteral(view) + ", true);", function(data) {
        if (data.error) {
          alert(data.error);
          return;
        }
        copyToClipboard(data.rows[0]);
      });
      break;
    case "view_def":
      executeQuery("SELECT pg_get_viewdef(" + quoteSqlLiteral(view) + ", true);", function(data) {
        if (data.error) {
          alert(data.error);
          return;
        }
        showViewDefinition(view, data.rows[0]);
      });
      break;
  }
}

function performRowAction(action, value) {
  if (action == "stop_query") {
    var pid = parseBackendPid(value);
    if (pid === null) {
      alert("Invalid backend pid");
      return;
    }
    if (!confirm("Are you sure you want to stop the query?")) return;
    executeQuery("SELECT pg_cancel_backend(" + pid + ");", function(data) {
      if (data.error) alert(data.error);
      setTimeout(showActivityPanel, 1000);
    });
  }
}

function parseBackendPid(value) {
  var text = String(value == null ? "" : value).trim();
  if (!/^\d+$/.test(text)) return null;

  var pid = Number(text);
  if (!Number.isSafeInteger(pid)) return null;
  return text;
}

function sortArrow(direction) {
  switch (direction) {
    case "ASC":
      return "&#x25B2;";
    case "DESC":
      return "&#x25BC;";
    default:
      return "";
  }
}

function buildTable(results, sortColumn, sortOrder, options) {
  if (!options) options = {};
  var action = options.action;

  resetTable();
  if (!results.error) {
    lastQueryResults = results;
  }

  if (results.error) {
    $("#results_header").html("");
    $("#results_body").html("<tr><td>ERROR: " + escapeHtml(results.error) + "</td></tr>");
    return;
  }

  if (results.rows.length == 0) {
    $("#results_header").html("");
    $("#results_body").html("<tr><td>No records found</td></tr>");
    if (results.stats) {
      $("#result-rows-count").html(results.stats.query_duration_ms + " ms");
    } else {
      $("#result-rows-count").html("");
    }
    $("#results").addClass("empty");
    return;
  }

  var cols = "";
  var rows = "";

  results.columns.forEach(function(col) {
    var escapedCol = escapeHtml(col);
    var colAttr = escapeAttribute(col);

    if (col === sortColumn) {
      cols += "<th class='table-header-col active' data-name='" + colAttr + "' data-order='" + escapeAttribute(sortOrder) + "'>" + escapedCol + "&nbsp;" + sortArrow(sortOrder) + "</th>";
    } else {
      cols += "<th class='table-header-col' data-name='" + colAttr + "'>" + escapedCol + "</th>";
    }
  });

  // No header to make the column non-sortable
  if (action) {
    cols += "<th></th>";

    // Determine which column contains the data attribute
    action.dataColumn = results.columns.indexOf(action.data);
  }

  results.rows.forEach(function(row) {
    var r = "";

    // Add all actual row data here
    for (var i in row) {
      r += "<td data-col='" + i + "'><div>" + escapeHtml(row[i]) + "</div></td>";
    }

    // Add row action button
    if (action) {
      r += "<td><a class='btn btn-xs btn-" + escapeAttribute(action.style) + " row-action' data-action='" + escapeAttribute(action.name) + "' data-value='" + escapeAttribute(row[action.dataColumn]) + "' href='#'>" + escapeHtml(action.title) + "</a></td>";
    }

    rows += "<tr>" + r + "</tr>";
  });

  $("#results_header").html(cols);
  $("#results_body").html(rows);

  // Show number of rows rendered on the page
  if (results.stats) {
    $("#result-rows-count").html(results.stats.rows_count + " rows in " + results.stats.query_duration_ms + " ms");
  } else {
    $("#result-rows-count").html(results.rows.length + " rows");
  }
}

function setCurrentTab(id) {
  if (id == "table_graph") {
    $("#output").hide();
    $("#pagination").hide();
    $("#graph_panel").show();
    drawGraph();
  } else {
    $("#graph_panel").hide();
    $("#output").show();
  }

  // Pagination should only be visible on rows tab
  if (id != "table_content") {
    $("#body").removeClass("with-pagination");
  }

  $("#nav ul li.selected").removeClass("selected");
  $("#" + id).addClass("selected");

  // Persist tab selection into the session storage
  sessionStorage.setItem("tab", id);
}

function showGraphPanel() {
  setCurrentTab("table_graph");
}

function insertAionDBSnippet(name) {
  var text = aiondbSnippets[name];
  if (!text) return;
  editor.setValue(text);
  editor.clearSelection();
  if (name.indexOf("cypher") >= 0 || name.indexOf("graph") >= 0 || name == "hybrid_vector") {
    $("#aiondb_query_mode").val("cypher");
  }
}

function runGraphPreview() {
  var query = getEditorSelection();
  if (query.length == 0 && lastQueryResults) {
    renderGraphFromResults(lastQueryResults);
    showGraphPanel();
    return;
  }
  if (query.length == 0) return;

  showQueryProgressMessage();
  executeQuery(query, function(data) {
    hideQueryProgressMessage();
    if (data.error) {
      buildTable(data);
      return;
    }
    lastQueryResults = data;
    renderGraphFromResults(data);
    showGraphPanel();
  });
}

function renderGraphFromResults(results) {
  graphState = inferGraph(results || {});
  seedGraphLayout();
  fitGraph();
  drawGraph();
}

function inferGraph(results) {
  var nodes = {};
  var edges = [];
  var cols = (results.columns || []).map(function(col) { return String(col).toLowerCase(); });
  var rows = results.rows || [];
  var srcIdx = firstColumnIndex(cols, ["source_id", "src_id", "from_id", "source", "src", "from"]);
  var dstIdx = firstColumnIndex(cols, ["target_id", "dst_id", "to_id", "target", "dst", "to"]);

  rows.forEach(function(row, rowIdx) {
    var obj = {};
    cols.forEach(function(col, idx) { obj[col] = row[idx]; });

    if (srcIdx >= 0 && dstIdx >= 0 && row[srcIdx] != null && row[dstIdx] != null) {
      var sid = String(row[srcIdx]);
      var tid = String(row[dstIdx]);
      ensureGraphNode(nodes, sid, pickGraphValue(obj, ["source_label", "src_label", "from_label", "source_name"]) || sid, "source");
      ensureGraphNode(nodes, tid, pickGraphValue(obj, ["target_label", "dst_label", "to_label", "target_name"]) || tid, "target");
      edges.push({ source: sid, target: tid, label: pickGraphValue(obj, ["edge", "edge_label", "relation", "type"]) || "" });
      return;
    }

    var scalar = row.filter(function(value) {
      return value != null && (typeof value == "string" || typeof value == "number");
    });
    if (scalar.length >= 2) {
      var a = String(scalar[0]);
      var b = String(scalar[1]);
      ensureGraphNode(nodes, a, a, cols[0] || "node");
      ensureGraphNode(nodes, b, b, cols[1] || "node");
      edges.push({ source: a, target: b, label: "" });
    } else if (scalar.length == 1) {
      ensureGraphNode(nodes, String(scalar[0]), String(scalar[0]), cols[0] || "node");
    } else if (row.length > 0) {
      ensureGraphNode(nodes, "row-" + rowIdx, "row-" + rowIdx, "row");
    }
  });

  return { nodes: Object.keys(nodes).map(function(key) { return nodes[key]; }), edges: edges, selected: null, scale: 1, offsetX: 0, offsetY: 0 };
}

function firstColumnIndex(cols, names) {
  for (var i = 0; i < names.length; i++) {
    var idx = cols.indexOf(names[i]);
    if (idx >= 0) return idx;
  }
  return -1;
}

function pickGraphValue(obj, names) {
  for (var i = 0; i < names.length; i++) {
    if (obj[names[i]] != null) return String(obj[names[i]]);
  }
  return "";
}

function ensureGraphNode(nodes, id, label, kind) {
  if (!nodes[id]) nodes[id] = { id: id, label: label, kind: kind, x: 0, y: 0 };
}

function seedGraphLayout() {
  var nodes = graphState.nodes;
  var count = Math.max(nodes.length, 1);
  nodes.forEach(function(node, idx) {
    var angle = idx / count * Math.PI * 2;
    var radius = 150 + Math.min(180, count * 4);
    node.x = Math.cos(angle) * radius;
    node.y = Math.sin(angle) * radius;
  });
}

function fitGraph() {
  var canvas = document.getElementById("graph_canvas");
  if (!canvas || graphState.nodes.length == 0) return;
  var width = canvas.clientWidth || 600;
  var height = canvas.clientHeight || 360;
  var xs = graphState.nodes.map(function(node) { return node.x; });
  var ys = graphState.nodes.map(function(node) { return node.y; });
  var minX = Math.min.apply(null, xs), maxX = Math.max.apply(null, xs);
  var minY = Math.min.apply(null, ys), maxY = Math.max.apply(null, ys);
  var spanX = Math.max(1, maxX - minX);
  var spanY = Math.max(1, maxY - minY);
  graphState.scale = Math.min(1.4, Math.max(0.25, Math.min((width - 80) / spanX, (height - 80) / spanY)));
  graphState.offsetX = width / 2 - ((minX + maxX) / 2) * graphState.scale;
  graphState.offsetY = height / 2 - ((minY + maxY) / 2) * graphState.scale;
}

function graphScreenX(x) { return x * graphState.scale + graphState.offsetX; }
function graphScreenY(y) { return y * graphState.scale + graphState.offsetY; }

function drawGraph() {
  var canvas = document.getElementById("graph_canvas");
  if (!canvas) return;
  var rect = canvas.getBoundingClientRect();
  var dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(600, rect.width) * dpr;
  canvas.height = Math.max(360, rect.height) * dpr;
  var ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight);
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, canvas.clientWidth, canvas.clientHeight);

  if (graphState.nodes.length == 0) {
    $("#graph_status").text("Run a query with source_id/target_id columns, then click Graph Preview.");
    ctx.fillStyle = "#777";
    ctx.fillText("No graph loaded", 20, 28);
    return;
  }

  var byId = {};
  graphState.nodes.forEach(function(node) { byId[node.id] = node; });

  graphState.edges.forEach(function(edge) {
    var source = byId[edge.source];
    var target = byId[edge.target];
    if (!source || !target) return;
    var sx = graphScreenX(source.x), sy = graphScreenY(source.y);
    var tx = graphScreenX(target.x), ty = graphScreenY(target.y);
    ctx.strokeStyle = "#9aa7b2";
    ctx.beginPath();
    ctx.moveTo(sx, sy);
    ctx.lineTo(tx, ty);
    ctx.stroke();
    if (edge.label) {
      ctx.fillStyle = "#667";
      ctx.font = "11px sans-serif";
      ctx.fillText(edge.label, (sx + tx) / 2 + 4, (sy + ty) / 2 - 4);
    }
  });

  graphState.nodes.forEach(function(node) {
    var x = graphScreenX(node.x);
    var y = graphScreenY(node.y);
    ctx.fillStyle = node.kind == "target" ? "#b8e0c7" : "#9bd3ff";
    ctx.strokeStyle = "#38546b";
    ctx.beginPath();
    ctx.arc(x, y, 18, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = "#1f2933";
    ctx.font = "11px sans-serif";
    ctx.textAlign = "center";
    ctx.fillText(shortGraphLabel(node.label), x, y + 34);
  });

  $("#graph_status").text(graphState.nodes.length + " nodes / " + graphState.edges.length + " edges");
}

function shortGraphLabel(label) {
  label = String(label || "");
  return label.length > 24 ? label.slice(0, 21) + "..." : label;
}

function showQueryHistory() {
  getHistory(function(data) {
    var rows = [];

    for(i in data) {
      rows.unshift([parseInt(i) + 1, data[i].query, data[i].timestamp]);
    }

    buildTable({ columns: ["id", "query", "timestamp"], rows: rows });

    setCurrentTab("table_history");
    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").addClass("no-crop");
  });
}

function showTableIndexes() {
  var name = getCurrentObject().name;

  if (name.length == 0) {
    alert("Please select a table!");
    return;
  }

  getTableIndexes(name, function(data) {
    setCurrentTab("table_indexes");
    buildTable(data);

    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").addClass("no-crop");
  });
}

function showTableConstraints() {
  var name = getCurrentObject().name;

  if (name.length == 0) {
    alert("Please select a table!");
    return;
  }

  getTableConstraints(name, function(data) {
    setCurrentTab("table_constraints");
    buildTable(data);

    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").addClass("no-crop");
  });
}

function showTableInfo() {
  var name = getCurrentObject().name;

  if (name.length == 0) {
    alert("Please select a table!");
    return;
  }

  apiCall("get", "/tables/" + encodeURIComponent(name) + "/info", {}, function(data) {
    $(".table-information .lines").show();
    $("#table_total_size").text(data.total_size);
    $("#table_data_size").text(data.data_size);
    $("#table_index_size").text(data.index_size);
    $("#table_rows_count").text(data.rows_count);
    $("#table_encoding").text("Unknown");
  });

  buildTableFilters(name, getCurrentObject().type);
}

function updatePaginator(pagination) {
  if (!pagination) {
    $(".current-page").data("page", 1).data("pages", 1);
    $("button.page").text("1 of 1");
    $(".prev-page, .next-page").prop("disabled", "disabled");
    return;
  }

  $(".current-page").
    data("page", pagination.page).
    data("pages", pagination.pages_count);

  if (pagination.page > 1) {
    $(".prev-page").prop("disabled", "");
  }
  else {
    $(".prev-page").prop("disabled", "disabled");
  }

  if (pagination.pages_count > 1 && pagination.page < pagination.pages_count) {
    $(".next-page").prop("disabled", "");
  }
  else {
    $(".next-page").prop("disabled", "disabled");
  }

  $("#total_records").text(pagination.rows_count);
  if (pagination.pages_count == 0) pagination.pages_count = 1;
  $("button.page").text(pagination.page + " of " + pagination.pages_count);
}

function showTableContent(sortColumn, sortOrder) {
  var name = getCurrentObject().name;

  if (name.length == 0) {
    alert("Please select a table!");
    return;
  }

  if (getCurrentObject().type == "function") {
    alert("Cant view rows for a function");
    return;
  }

  var opts = {
    limit:       getRowsLimit(),
    offset:      getPaginationOffset(),
    sort_column: sortColumn,
    sort_order:  sortOrder
  };

  var filter = {
    column: $(".filters select.column").val(),
    op:     $(".filters select.filter").val(),
    input:  $(".filters input").val()
  };

  // Apply filtering only if column is selected
  if (filter.column && filter.op) {
    var where = [
      quoteSqlIdentifier(filter.column),
      filterOptions[filter.op].replace("DATA", quoteSqlLiteralValue(filter.input))
    ].join(" ");

    opts["where"] = where;
  }

  getTableRows(name, opts, function(data) {
    $("#input").hide();
    $("#body").prop("class", "with-pagination");

    buildTable(data, sortColumn, sortOrder);
    setCurrentTab("table_content");
    updatePaginator(data.pagination);

    $("#results").data("mode", "browse").data("table", name);
  });
}

function showPaginatedTableContent() {
  var activeColumn = $("#results th.active");
  var sortColumn = null;
  var sortOrder = null;

  if (activeColumn.length) {
    sortColumn = activeColumn.data("name");
    sortOrder = activeColumn.data("order");
  }

  showTableContent(sortColumn, sortOrder);
}

function showDatabaseStats() {
  getTablesStats(function(data) {
    buildTable(data);

    setCurrentTab("table_structure");
    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").addClass("no-crop");
  });
}

function downloadDatabaseStats() {
  openInNewWindow("api/tables_stats", { format: "csv", export: "true" });
}

function showServerSettings() {
  getServerSettings(function(data) {
    buildTable(data);

    setCurrentTab("table_content");
    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").addClass("no-crop");
  });
}

function showTableStructure() {
  var name = getCurrentObject().name;

  if (name.length == 0) {
    alert("Please select a table!");
    return;
  }

  setCurrentTab("table_structure");

  $("#input").hide();
  $("#body").prop("class", "full");

  getTableStructure(name, { type: getCurrentObject().type }, function(data) {
    if (getCurrentObject().type == "function") {
      var name = data.rows[0][data.columns.indexOf("proname")];
      var definition = data.rows[0][data.columns.indexOf("functiondef")];
      showFunctionDefinition(name, definition);
      return
    }

    buildTable(data);
    $("#results").addClass("no-crop");
  });
}

function showViewDefinition(viewName, viewDefintion) {
  setCurrentTab("table_structure");
  renderResultsView("View definition for: " + viewName, viewDefintion);
}

function showFunctionDefinition(functionName, definition) {
  setCurrentTab("table_structure");
  renderResultsView("Function definition for: " + functionName, definition)
}

function renderResultsView(title, content) {
  $("#results").addClass("no-crop");
  $("#input").hide();
  $("#body").prop("class", "full");
  $("#results").hide();

  var title = $("<div/>").prop("class", "title").text(title);
  var content = $("<pre/>").text(content);

  $("<div/>").
    html("<i class='fa fa-copy'></i>").
    addClass("copy").
    appendTo(content);

  $("#results_view").html("");
  title.appendTo("#results_view");
  content.appendTo("#results_view");
  $("#results_view").show();
}

function showQueryPanel() {
  if (!$("#table_query").hasClass("selected")) {
    resetTable();
  }

  setCurrentTab("table_query");
  editor.focus();

  $("#input").show();
  $("#body").prop("class", "")
}

function showConnectionPanel() {
  setCurrentTab("table_connection");
  $("#input").hide();
  $("#body").addClass("full");

  getConnection(function(data) {
    var rows = [];

    for(key in data) {
      rows.push([key, data[key]]);
    }

    buildTable({
      columns: ["attribute", "value"],
      rows: rows
    });
  });
}

function showActivityPanel() {
  var options = {
    action: {
      name: "stop_query",
      title: "stop",
      data: "pid",
      style: "danger"
    }
  }

  setCurrentTab("table_activity");
  $("#input").hide();
  $("#body").addClass("full");

  apiCall("get", "/activity", {}, function(data) {
    buildTable(data, null, null, options);
  });
}

function showQueryProgressMessage() {
  $("#run, #explain-dropdown-toggle, #csv, #json, #xml, #load-local-query").prop("disabled", true);
  $("#explain-dropdown").removeClass("open");
  $("#query_progress").show();
}

function hideQueryProgressMessage() {
  $("#run, #explain-dropdown-toggle, #csv, #json, #xml, #load-local-query").prop("disabled", false);
  $("#query_progress").hide();
}

function getEditorSelection() {
  // Return the exact selection if user has one
  var query = $.trim(editor.getSelectedText());
  if (query.length > 0) {
    return query;
  }

  query = editor.getValue();

  // Determine which query we should run when there are multiple queries without a delimiter
  if (query.indexOf(";") == -1) {
    var subquery = getSubquery(query, editor.getCursorPosition());

    if (subquery) {
      // Highlight query selection so user knows what is being executed
      if (subquery.numChunks > 1) {
        editor.selection.setSelectionRange({
          start: { row: subquery.startRow, column: 0 },
          end: { row: subquery.endRow, column: 0 },
        })
      }

      return subquery.text;
    }
  }

  return query;
}

function getSubquery(text, cursor) {
  var lines = text.split("\n");
  var startRow = undefined;
  var numChunks = 0;
  var ranges = [];

  for (i = 0; i < lines.length; i++) {
    if (lines[i].trim().length == 0) {
      if (startRow >= 0 && cursor.row >= startRow && cursor.row <= i) {
        ranges.push([startRow, i]);
      }

      numChunks++;
      startRow = undefined;
      continue;
    }

    if (startRow === undefined) {
      startRow = i;
    }

    if (i == lines.length - 1) {
      ranges.push([startRow, i + 1]);
      numChunks++;
    }
  }

  if (ranges.length > 0) {
    return {
      text: lines.slice(ranges[0][0], ranges[0][1]).join("\n"),
      startRow: ranges[0][0],
      endRow: ranges[0][1],
      numChunks: numChunks
    };
  }
}

function runQuery() {
  setCurrentTab("table_query");
  showQueryProgressMessage();

  var query = getEditorSelection();
  if (query.length == 0) {
    hideQueryProgressMessage();
    return;
  }

  executeQuery(query, function(data) {
    buildTable(data);

    hideQueryProgressMessage();
    $("#input").show();
    $("#body").removeClass("full");
    $("#results").data("mode", "query");

    if (query.toLowerCase().indexOf("explain") != -1) {
      $("#results").addClass("no-crop");
    }

    // Reload objects list if anything was created/deleted
    if (query.match(/(create|drop)\s/i)) {
      loadSchemas();
    }
  });
}

function runExplain() {
  setCurrentTab("table_query");
  showQueryProgressMessage();

  var query = getEditorSelection();
  if (query.length == 0) {
    hideQueryProgressMessage();
    return;
  }

  explainQuery(query, function(data) {
    buildTable(data);

    hideQueryProgressMessage();
    $("#input").show();
    $("#body").removeClass("full");
    $("#results").addClass("no-crop");
  });
}

function runAnalyze() {
  setCurrentTab("table_query");
  showQueryProgressMessage();

  var query = getEditorSelection();
  if (query.length == 0) {
    hideQueryProgressMessage();
    return;
  }

  analyzeQuery(query, function(data) {
    buildTable(data);

    hideQueryProgressMessage();
    $("#input").show();
    $("#body").removeClass("full");
    $("#results").addClass("no-crop");
  });
}

function generateURL(path, params) {
  var url = new URL(window.location.href.split("#")[0]);

  url.pathname += path;
  for (var key in params) {
    url.searchParams.append(key, params[key]);
  }

  // Automatically append session id so we dont have to do that everywhere
  url.searchParams.append("_session_id", getSessionId());

  return url.toString();
}

function openInNewWindow(path, params) {
  var url = generateURL(path, params);
  var win = window.open(url, "_blank", "noopener");
  if (win) {
    win.opener = null;
    win.focus();
  }
}

function openPostInNewWindow(path, params) {
  var form = $("<form>", {
    method: "post",
    action: path,
    target: "_blank",
    rel: "noopener"
  });
  params = $.extend({}, params, { "_session_id": getSessionId() });

  for (var key in params) {
    if (!Object.prototype.hasOwnProperty.call(params, key)) {
      continue;
    }
    $("<input>", {
      type: "hidden",
      name: key,
      value: params[key]
    }).appendTo(form);
  }

  $("body").append(form);
  form.submit();
  form.remove();
}

function exportTo(format) {
  var query = getEditorSelection();
  if (query.length == 0) {
    return;
  }

  setCurrentTab("table_query");

  openPostInNewWindow("api/query", {
    "format": format,
    "query": encodeQuery(query)
  })
}

// Fetch all unique values for the selected column in the table
function showUniqueColumnsValues(table, column, showCounts) {
  var quotedColumn = quoteSqlIdentifier(column);
  var quotedTable = quoteSqlIdentifierPath(table);
  var query = 'SELECT DISTINCT ' + quotedColumn + ' FROM ' + quotedTable;

  // Display results ordered by counts.
  // This could be slow on large sets without an index.
  if (showCounts) {
    query = 'SELECT DISTINCT ' + quotedColumn + ', COUNT(1) AS total_count FROM ' + quotedTable + ' GROUP BY ' + quotedColumn + ' ORDER BY total_count DESC';
  }

  executeQuery(query, function(data) {
    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").data("mode", "query");
    buildTable(data);
  });
}

// Show numeric stats on the field
function showFieldNumStats(table, column) {
  var quotedColumn = quoteSqlIdentifier(column);
  var query = 'SELECT count(1), min(' + quotedColumn + '), max(' + quotedColumn + '), avg(' + quotedColumn + ') FROM ' + quoteSqlIdentifierPath(table);

  executeQuery(query, function(data) {
    $("#input").hide();
    $("#body").prop("class", "full");
    $("#results").data("mode", "query");
    buildTable(data);
  });
}

function buildTableFilters(name, type) {
  getTableStructure(name, { type: type }, function(data) {
    if (data.rows.length == 0) {
      $("#pagination .filters").hide();
    }
    else {
      $("#pagination .filters").show();
    }

    $("#pagination select.column").html("<option value='' selected>Select column</option>");

    for (var i = 0; i < data.rows.length; i++) {
      var row = data.rows[i];

      var el = $("<option/>").attr("value", row[0]).text(row[0]);
      $("#pagination select.column").append(el);
    }
  });
}

var objectAutocompleter = {
  getCompletions: function (editor, session, pos, prefix, callback) {
    callback(null, autocompleteObjects);
  }
}

function initEditor() {
  var writeQueryTimeout = null;

  editor = ace.edit("custom_query");
  editor.setOptions({
    enableBasicAutocompletion: true,
    enableLiveAutocompletion: true,
  });
  editor.completers.push(objectAutocompleter);

  editor.setFontSize(13);
  editor.setTheme("ace/theme/tomorrow");
  editor.setShowPrintMargin(false);
  editor.getSession().setMode("ace/mode/pgsql");
  editor.getSession().setTabSize(2);
  editor.getSession().setUseSoftTabs(true);

  editor.commands.addCommands([{
    name: "run_query",
    bindKey: {
      win: "Ctrl-Enter",
      mac: "Command-Enter"
    },
    exec: function(editor) {
      runQuery();
    }
  }, {
    name: "explain_query",
    bindKey: {
      win: "Ctrl-E",
      mac: "Command-E"
    },
    exec: function(editor) {
      runExplain();
    }
  }]);

  editor.on("change", function() {
    if (writeQueryTimeout) {
      clearTimeout(writeQueryTimeout);
    }

    writeQueryTimeout = setTimeout(function() {
      var query = editor.getValue();
      if (query.length <= maxStoredQueryLength) {
        localStorage.setItem("pgweb_query", query);
      }
    }, 1000);
  });

  var query = localStorage.getItem("pgweb_query");
  if (query && query.length > 0 && query.length <= maxStoredQueryLength) {
    editor.setValue(query);
    editor.clearSelection();
  }
}

function addShortcutTooltips() {
  if (navigator.userAgent.indexOf("OS X") > 0) {
    $("#run").attr("title", "Shortcut: ⌘+Enter");
    $("#explain").attr("title", "Shortcut: ⌘+E");
  }
  else {
    $("#run").attr("title", "Shortcut: Ctrl+Enter");
    $("#explain").attr("title", "Shortcut: Ctrl+E");
  }
}

// Get the latest release from Github API
function getLatestReleaseInfo(current) {
  return;
}

function showConnectionSettings() {
  // Show the current postgres version
  $(".connection-settings .version").text("v" + appInfo.version).show();
  $("#connection_window").show();
  initConnectionWindow();

  // Check github release page for updates
  getLatestReleaseInfo(appInfo);

  getBookmarks(function(data) {
    if (data.error) {
      console.log("Error while fetching bookmarks:", data.error);
      return;
    }

    if (data.length > 0) {
      // Set bookmarks in global var
      bookmarks = data;

      // Remove all existing bookmark options
      $("#connection_bookmarks").html("");

      // Add blank option
      $("<option/>").
        val("").
        text("Select a bookmarked database to connect to").
        appendTo("#connection_bookmarks");

      // Add all available bookmarks
      for (var key of data) {
        $("<option/>").
          val(key).
          text(key).
          appendTo("#connection_bookmarks");
      }

      $(".bookmarks").show();
    }
    else {
      if (appFeatures.bookmarks_only) {
        $("#connection_error").html("Running in <b>bookmarks-only</b> mode but <b>NO</b> bookmarks configured.").show();
        $(".open-connection").hide();
      } else {
        $(".bookmarks").hide();
      }
    }
  });
}

function initConnectionWindow() {
  if (appFeatures.bookmarks_only) {
    $(".connection-group-switch").hide();
    $(".connection-scheme-group").hide();
    $(".connection-bookmarks-group").show();
    $(".connection-standard-group").hide();
    $(".connection-ssh-group").hide();
  } else {
    $(".connection-group-switch").show();
    $(".connection-scheme-group").hide();
    $(".connection-bookmarks-group").show();
    $(".connection-standard-group").show();
    $(".connection-ssh-group").hide();
  }

  if ($("#pg_host").val() == "") $("#pg_host").val("127.0.0.1");
  if ($("#pg_port").val() == "") $("#pg_port").val("5432");
  if ($("#pg_user").val() == "") $("#pg_user").val("dev");
  if ($("#pg_db").val() == "") $("#pg_db").val("default");
  $("#connection_ssl").val("disable");
}

function getConnectionString() {
  var url  = $.trim($("#connection_url").val());
  var mode = $(".connection-group-switch button.active").attr("data");
  var ssl  = $("#connection_ssl").val();

  if (mode == "standard" || mode == "ssh") {
    var host = $("#pg_host").val();
    var port = $("#pg_port").val();
    var user = $("#pg_user").val();
    var pass = encodeURIComponent($("#pg_password").val());
    var db   = $("#pg_db").val();

    if (port.length == 0) {
      port = "5432";
    }

    url = "postgres://" + user + ":" + pass + "@" + host + ":" + port + "/" + db + "?sslmode=" + ssl;
  }
  else {
    if (isLoopbackConnectionURL(url)) {
      var parsed = new URL(url);
      if (!parsed.searchParams.has("sslmode")) {
        parsed.searchParams.set("sslmode", ssl);
        url = parsed.toString();
      }
    }
  }

  return url;
}

// Add a context menu to the results table header columns
function bindTableHeaderMenu() {
  $("#results_header").contextmenu({
    scopes: "th",
    target: "#results_header_menu",
    before: function(e, element, target) {
      // Enable menu for browsing table rows view only.
      if ($("#results").data("mode") != "browse") {
        e.preventDefault();
        this.closemenu();
        return false;
      }
    },
    onItem: function(context, e) {
      var menuItem = $(e.target);

      switch(menuItem.data("action")) {
        case "copy_name":
          copyToClipboard($(context).data("name"));
          break;

        case "unique_values":
          showUniqueColumnsValues(
            $("#results").data("table"), // table name
            $(context).data("name"),     // column name
            menuItem.data("counts")      // display counts
          );
          break;

        case "num_stats":
          showFieldNumStats(
            $("#results").data("table"), // table name
            $(context).data("name")      // column name
          );
          break;
      }
    }
  });

  $("#results_body").contextmenu({
    scopes: "td",
    target: "#results_row_menu",
    before: function(e, element, target) {
      var browseMode = $("#results").data("mode");
      var isEmpty    = $("#results").hasClass("empty");
      var isAllowed  = browseMode == "browse" || browseMode == "query";

      if (isEmpty || !isAllowed) {
        e.preventDefault();
        this.closemenu();
        return false;
      }
    },
    onItem: function(context, e) {
      var menuItem = $(e.target);

      switch(menuItem.data("action")) {
        case "display_value":
          var value = $(context).text();
          $("#content_modal .content").text(value);
          $("#content_modal").show();
          break;
        case "copy_value":
          copyToClipboard($(context).text());
          break;
        case "filter_by_value":
          var colIdx   = $(context).data("col");
          var colValue = $(context).text();
          var colName  = $("#results_header th").eq(colIdx).data("name");

          $("select.column").val(colName);
          $("select.filter").val("equal");
          $("#table_filter_value").val(colValue);
          $("#rows_filter").submit();
      }
    }
  });
}

function bindCurrentDatabaseMenu() {
  $("#current_database").contextmenu({
    target: "#current_database_context_menu",
    onItem: function(context, e) {
      var menuItem = $(e.target);

      switch(menuItem.data("action")) {
        case "show_db_stats":
          showDatabaseStats();
          break;
        case "download_db_stats":
          downloadDatabaseStats();
          break;
        case "server_settings":
          showServerSettings();
          break;
        case "export":
          openPostInNewWindow("api/export");
          break;
      }
    }
  });
}

function bindDatabaseObjectsFilter() {
  var filterTimeout = null;

  $("#filter_database_objects").on("keyup", function (e) {
    clearTimeout(filterTimeout);

    var val = $(this).val().trim();

    // Reset search on ESC
    if (e.keyCode == 27 || val == "") {
      resetObjectsFilter();
      return;
    }

    $(".clear-objects-filter").show();
    $(".schema-group").addClass("expanded");

    filterTimeout = setTimeout(function() {
      filterObjectsByName(val)
    }, 200);
  });

  $(".clear-objects-filter").on("click", function(e) {
    resetObjectsFilter();
  });
}

function resetObjectsFilter() {
  $("#filter_database_objects").val("");
  $("#objects li.schema-item").show();
  $(".clear-objects-filter").hide();
}

function filterObjectsByName(query) {
  $("#objects li.schema-item").each(function (idx, el) {
    var item = $(el);
    var name = $(el).data("name");

    if (name.indexOf(query) < 0) {
      item.hide();
    } else {
      item.show();
    }
  });
}

function getQuotedSchemaTableName(schema, table) {
  if (table !== undefined) {
    return [quoteSqlIdentifier(schema), quoteSqlIdentifier(table)].join(".");
  }

  if (typeof schema === "string" && schema.indexOf(".") > -1) {
    var schemaTableComponents = schema.split(".");
    return [quoteSqlIdentifier(schemaTableComponents[0]), quoteSqlIdentifier(schemaTableComponents[1])].join(".");
  }

  return quoteSqlIdentifier(schema);
}

function bindContextMenus() {
  bindTableHeaderMenu();
  bindCurrentDatabaseMenu();

  $(".schema-group ul").each(function(id, el) {
    var group = $(el).data("group");

    if (group == "table") {
      $(el).contextmenu({
        target: "#tables_context_menu",
        scopes: "li.schema-table",
        onItem: function(context, e) {
          var el      = $(e.target);
          var table   = getQuotedSchemaTableName($(context[0]).data("schema"), $(context[0]).data("name"));
          var action  = el.data("action");
          performTableAction(table, action, el);
        }
      });
    }

    if (group == "view") {
      $(el).contextmenu({
        target: "#view_context_menu",
        scopes: "li.schema-view",
        onItem: function(context, e) {
          var el      = $(e.target);
          var table   = getQuotedSchemaTableName($(context[0]).data("schema"), $(context[0]).data("name"));
          var action  = el.data("action");
          performViewAction(table, action, el);
        }
      });
    }

    if (group == "materialized_view") {
      $(el).contextmenu({
        target: "#view_context_menu",
        scopes: "li.schema-materialized_view",
        onItem: function(context, e) {
          var el      = $(e.target);
          var table   = getQuotedSchemaTableName($(context[0]).data("schema"), $(context[0]).data("name"));
          var action  = el.data("action");
          performViewAction(table, action, el);
        }
      });
    }
  });
}

function toggleDatabaseSearch() {
  $("#current_database").toggle();
  $("#database_search").toggle();
}

function enableDatabaseSearch(data) {
  var input = $("#database_search");

  input.typeahead("destroy");

  input.typeahead({
    source: data,
    minLength: 0,
    items: "all",
    autoSelect: false,
    fitToElement: true
  });

  input.typeahead("lookup").focus();

  input.on("focusout", function(e){
    toggleDatabaseSearch();
    input.off("focusout");
  });
}

function bindInputResizeEvents() {
  var height = sessionStorage.getItem("input_height");
  if (height) {
    resizeInput(height);
    checkInputSize();
  }

  $("body").on("mousemove", onInputResize);
  $("body").on("mouseup", endInputResize);
  $("#input_resize_handler").on("mousedown", beginInputResize);
  $(window).on("resize", checkInputSize);
}

function checkInputSize() {
  var inputHeight = $("#input").height();
  var bodyHeight = $("#body").height();

  if (bodyHeight == 0 || inputHeight == 0) return;

  if (inputHeight > bodyHeight || bodyHeight - inputHeight < 200) {
    resizeInput(bodyHeight - 200);
  }
}

function resizeInput(height) {
  if (height < 100) height = 100;

  var diff = 50 + 12; // actions box + padding

  $("#input").height(height);
  $("#input .input-wrapper").height(height - diff);
  $("#custom_query").height(height - diff);
  $("#output").css("top", height + "px");

  if (editor) {
    editor.resize();
  }
}

function beginInputResize() {
  inputResizing = true;
  inputResizeOffset = $("#input").offset().top;

  $("html").css("cursor", "row-resize");
  $("#input_resize_handler").addClass("dragging");
}

function endInputResize() {
  if (!inputResizing) return;

  inputResizing = false;
  inputResizeOffset = null;

  $("html").css("cursor", "auto");
  $("#input_resize_handler").removeClass("dragging");

  // Save current settings for page reloads
  sessionStorage.setItem("input_height", $("#input").height());
}

function onInputResize(event) {
  if (!inputResizing) return;

  var computedHeight = event.clientY - inputResizeOffset;
  if (computedHeight < 150) computedHeight = 150;

  resizeInput(computedHeight);
}

function bindContentModalEvents() {
  var contentModal = document.getElementById("content_modal");

  $(window).on("click", function(e) {
    // Automatically hide the modal on any click outside of the modal window
    if (e.target && !contentModal.contains(e.target)) {
      $("#content_modal").hide();
    }
  });

  $("#content_modal .content-modal-action").on("click", function() {
    switch ($(this).data("action")) {
      case "copy":
        copyToClipboard($("#content_modal pre").text());
        break;
      case "close":
        $("#content_modal").hide();
        break;
    }
  });

  $("#results").on("dblclick", "td > div", function() {
    var value = $(this).text();
    if (!value) return;

    $("#content_modal pre").text(value);
    $("#content_modal").show();
  })
}

$(document).ready(function() {
  bindInputResizeEvents();
  bindContentModalEvents();

  $("#table_content").on("click",     function() { showTableContent();     });
  $("#table_structure").on("click",   function() { showTableStructure();   });
  $("#table_indexes").on("click",     function() { showTableIndexes();     });
  $("#table_constraints").on("click", function() { showTableConstraints(); });
  $("#table_history").on("click",     function() { showQueryHistory();     });
  $("#table_query").on("click",       function() { showQueryPanel();       });
  $("#table_graph").on("click",       function() { showGraphPanel();       });
  $("#table_connection").on("click",  function() { showConnectionPanel();  });
  $("#table_activity").on("click",    function() { showActivityPanel();    });

  $("#run").on("click", function() {
    runQuery();
  });

  $("#graph_preview").on("click", function() {
    runGraphPreview();
  });

  $("#graph_fit").on("click", function() {
    fitGraph();
    drawGraph();
  });

  $("#aiondb_snippets").on("change", function() {
    insertAionDBSnippet($(this).val());
    $(this).val("");
  });

  $("#explain").on("click", function() {
    runExplain();
  });

  $("#analyze").on("click", function() {
    runAnalyze();
  });

  $("#csv").on("click", function() {
    exportTo("csv");
  });

  $("#json").on("click", function() {
    exportTo("json");
  });

  $("#xml").on("click", function() {
    exportTo("xml");
  });

  $("#results_view").on("click", ".copy", function() {
    copyToClipboard($(this).parent().text());
  });

  $("#results").on("click", "tr", function(e) {
    $("#results tr.selected").removeClass();
    $(this).addClass("selected");
  });

  $("#objects").on("click", ".schema-group-title", function(e) {
    $(this).parent().toggleClass("expanded");
  });

  $("#objects").on("click", ".schema-name", function(e) {
    $(this).parent().toggleClass("expanded");
  });

  $("#objects").on("click", "li", function(e) {
    currentObject = {
      name: $(this).data("id"),
      type: $(this).data("type")
    };

    $("#objects li").removeClass("active");
    $(this).addClass("active");
    $(".current-page").data("page", 1);
    $(".filters select, .filters input").val("");

    if (currentObject.type == "function") {
      sessionStorage.setItem("tab", "table_structure");
    } else {
      showTableInfo();
    }

    switch(sessionStorage.getItem("tab")) {
      case "table_content":
        showTableContent();
        break;
      case "table_structure":
        showTableStructure();
        break;
      case "table_constraints":
        showTableConstraints();
        break;
      case "table_indexes":
        showTableIndexes();
        break;
      default:
        showTableContent();
    }
  });

  $("#results").on("click", "a.row-action", function(e) {
    e.preventDefault();

    var action = $(this).data("action");
    var value  = $(this).data("value");

    performRowAction(action, value);
  })

  $("#results").on("click", "th", function(e) {
    if (!$("#table_content").hasClass("selected")) return;

    var sortColumn = $(this).data("name");
    var sortOrder  = $(this).data("order") === "ASC" ? "DESC" : "ASC";

    $(this).data("order", sortOrder);
    showTableContent(sortColumn, sortOrder);
  });

  $("#refresh_tables").on("click", function() {
    loadSchemas();
  });

  $("#rows_filter").on("submit", function(e) {
    e.preventDefault();
    $(".current-page").data("page", 1);

    var column = $(this).find("select.column").val();
    var filter = $(this).find("select.filter").val();
    var query  = $.trim($(this).find("input").val());

    if (filter && filterOptions[filter].indexOf("DATA") > 0 && query == "") {
      alert("Please specify filter query");
      return
    }

    showTableContent();
  });

  $(".change-limit").on("click", function() {
    var limit = prompt("Please specify a new rows limit", getRowsLimit());

    if (limit && limit >= 1) {
      $(".current-page").data("page", 1);
      setRowsLimit(limit);
      showTableContent();
    }
  });

  $("select.filter").on("change", function(e) {
    var val = $(this).val();

    if (["null", "not_null"].indexOf(val) >= 0) {
      $(".filters input").hide().val("");
    }
    else {
      $(".filters input").show();
    }
  });

  $("button.reset-filters").on("click", function() {
    $(".filters select, .filters input").val("");
    showTableContent();
  });

  // Automatically prefill the filter if it's not set yet
  $("select.column").on("change", function() {
    if ($("select.filter").val() == "") {
      $("select.filter").val("equal");
      $("#table_filter_value").focus();
    }
  });

  $("#pagination .next-page").on("click", function() {
    var current = $(".current-page").data("page");
    var total   = $(".current-page").data("pages");

    if (total > current) {
      $(".current-page").data("page", current + 1);
      showPaginatedTableContent();

      if (current + 1 == total) {
        $(this).prop("disabled", "disabled");
      }
    }

    if (current > 1) {
      $(".prev-page").prop("disabled", "");
    }
  });

  $("#pagination .prev-page").on("click", function() {
    var current = $(".current-page").data("page");

    if (current > 1) {
      $(".current-page").data("page", current - 1);
      $(".next-page").prop("disabled", "");
      showPaginatedTableContent();
    }

    if (current == 1) {
      $(this).prop("disabled", "disabled");
    }
  });

  $("#current_database").on("click", function(e) {
    apiCall("get", "/databases", {}, function(resp) {
      toggleDatabaseSearch();
      enableDatabaseSearch(resp);
    });
  });

  $("#database_search").change(function(e) {
    var current = $("#database_search").typeahead("getActive");
    if (current && current == $("#database_search").val()) {
      apiCall("post", "/switchdb", { db: current }, function(resp) {
        if (resp.error) {
          alert(resp.error);
          return;
        };
        window.location.reload();
      });
    };
  });

  $("#edit_connection").on("click", function() {
    if (connected) {
      $("#close_connection_window").show();
    }

    showConnectionSettings();
  });

  $("#close_connection").on("click", function() {
    if (!confirm("Are you sure you want to disconnect?")) return;

    disconnect(function() {
      showConnectionSettings();
      resetTable();
      $("#close_connection_window").hide();
    });
  });

  $("#close_connection_window").on("click", function() {
    $("#connection_window").hide();
  });

  $("#connection_url").on("change", function() {
    if (isLoopbackConnectionURL($(this).val())) {
      $("#connection_ssl").val("disable");
    }
  });

  $("#pg_host").on("change", function() {
    if (isLoopbackHostName($(this).val())) {
      $("#connection_ssl").val("disable");
    }
  });

  $(".connection-group-switch button").on("click", function() {
    $(".connection-group-switch button").removeClass("active");
    $(this).addClass("active");

    switch($(this).attr("data")) {
      case "scheme":
        $(".connection-scheme-group").show();
        $(".connection-standard-group").hide();
        $(".connection-ssh-group").hide();
        return;
      case "standard":
        $(".connection-scheme-group").hide();
        $(".connection-standard-group").show();
        $(".connection-ssh-group").hide();
        return;
      case "ssh":
        $(".connection-scheme-group").hide();
        $(".connection-standard-group").show();
        $(".connection-ssh-group").show();
        return;
    }
  });

  $("#connection_bookmarks").on("change", function(e) {
    var selection = $(this).val();

    var inputs = [
      $("#connection_form input[type='text']"),
      $("#connection_form input[type='password']"),
      $("#connection_ssl")
    ];

    inputs.forEach(function(selector) {
      selector.val("").prop("disabled", selection == "" ? "" : "disabled");
    });
  });

  $("#connection_form").on("submit", function(e) {
    e.preventDefault();

    var button = $(this).find("button.open-connection");
    var params = {};
    var bookmarkID = $.trim($("#connection_bookmarks").val());

    if (bookmarkID != "") {
      params["bookmark_id"] = $("#connection_bookmarks").val();
    }
    else {
      params.url = getConnectionString();
      if (params.url.length == 0) {
        return;
      }

      if ($(".connection-group-switch button.active").attr("data") == "ssh") {
        params["ssh"]              = 1
        params["ssh_host"]         = $("#ssh_host").val();
        params["ssh_port"]         = $("#ssh_port").val();
        params["ssh_user"]         = $("#ssh_user").val();
        params["ssh_password"]     = $("#ssh_password").val();
        params["ssh_key"]          = $("#ssh_key").val();
        params["ssh_key_password"] = $("#ssh_key_password").val()
      }
    }

    $("#connection_error").hide();
    button.prop("disabled", true).text("Please wait...");

    apiCall("post", "/connect", params, function(resp) {
      button.prop("disabled", false).text("Connect");

      if (resp.error) {
        connected = false;
        $("#connection_error").text(resp.error).show();
      }
      else {
        connected = true;
        loadSchemas();
        loadLocalQueries();

        $("#connection_window").hide();
        $("#current_database").text(resp.current_database);
        $("#main").show();
      }
    });
  });

  initEditor();
  addShortcutTooltips();
  bindDatabaseObjectsFilter();

  // Set session from the url
  var reqUrl = new URL(window.location);
  var sessionId = reqUrl.searchParams.get("session");

  if (isValidSessionId(sessionId)) {
    sessionStorage.setItem("session_id", sessionId);
    window.history.pushState({}, document.title, window.location.pathname);
  }

  getInfo(function(resp) {
    if (resp.error) {
      alert("Unable to fetch app info: " + resp.error + ". Please reload the browser page.");
      return;
    }

    appInfo = resp.app;
    appFeatures = resp.features;

    getConnection(function(resp) {
      if (resp.error) {
        connected = false;
        showConnectionSettings();
        $(".connection-actions").show();
        return;
      }

      connected = true;
      loadSchemas();
      loadLocalQueries();

      $("#current_database").text(resp.current_database);
      $("#main").show();

      if (!appFeatures.session_lock) {
        $(".connection-actions").show();
      }
    });
  });
});
