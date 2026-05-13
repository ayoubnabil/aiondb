(() => {
  const COLORS = {
    aiondb: "#2563eb",
    surrealdb: "#7c3aed",
    pgstack: "#0f766e",
    aiondb_embedded: "#2563eb",
    surrealdb_embedded_mem: "#7c3aed",
  };

  const MAX_ROWS = 80;
  const byTestName = (a, b) => a.test.localeCompare(b.test);

  initBenchmarkChart({
    dataId: "bench-results-data",
    chartId: "bench-chart",
    categoryId: "bench-category",
    metricId: "bench-metric",
    scaleId: "bench-scale",
    sortId: "bench-sort",
    summaryBodyId: "bench-summary-body",
    baselineEngine: "pgstack",
    defaultSort: "aiondb",
    defaultMetric: "ops",
    defaultScale: "linear",
    ratioAxisLabel: "AionDB / PostgreSQL stack",
    renderSummary: renderMainSummary,
  });

  initBenchmarkChart({
    dataId: "embedded-bench-results-data",
    chartId: "embedded-bench-chart",
    categoryId: "embedded-bench-category",
    metricId: "embedded-bench-metric",
    scaleId: "embedded-bench-scale",
    sortId: "embedded-bench-sort",
    summaryBodyId: "embedded-bench-summary-body",
    baselineEngine: "surrealdb_embedded_mem",
    defaultSort: "aiondb_embedded",
    defaultMetric: "ops",
    defaultScale: "linear",
    ratioAxisLabel: "AionDB / SurrealDB embedded",
    renderSummary: renderEmbeddedSummary,
  });

  function initBenchmarkChart(config) {
    const dataEl = document.getElementById(config.dataId);
    if (!dataEl) return;

    const host = document.getElementById(config.chartId);
    const categorySelect = document.getElementById(config.categoryId);
    const metricSelect = document.getElementById(config.metricId);
    const scaleSelect = document.getElementById(config.scaleId);
    const sortSelect = document.getElementById(config.sortId);
    const summaryBody = document.getElementById(config.summaryBodyId);
    if (!host || !categorySelect || !metricSelect || !scaleSelect || !sortSelect) return;

    if (!window.d3) {
      host.textContent = "D3 is not loaded.";
      return;
    }

    const inlineData = dataEl.dataset.src ? null : JSON.parse(dataEl.textContent);
    const load = dataEl.dataset.src ? fetch(dataEl.dataset.src).then((res) => res.json()) : Promise.resolve(inlineData);

    load.then((resolvedData) => {
      const engines = resolvedData.engines.map((engine) => engine.id);
      const labels = Object.fromEntries(resolvedData.engines.map((engine) => [engine.id, engine.label]));
      const tests = resolvedData.tests.slice();
      const categories = ["all", ...Array.from(new Set(tests.map((test) => test.category))).sort()];

      categorySelect.innerHTML = categories.map((category) => `<option value="${escapeAttr(category)}">${escapeHtml(category)}</option>`).join("");
      if (engines.includes(config.defaultSort)) sortSelect.value = config.defaultSort;
      if ([...metricSelect.options].some((option) => option.value === config.defaultMetric)) metricSelect.value = config.defaultMetric;
      if ([...scaleSelect.options].some((option) => option.value === config.defaultScale)) scaleSelect.value = config.defaultScale;

      for (const control of [categorySelect, metricSelect, scaleSelect, sortSelect]) {
        control.addEventListener("change", render);
      }
      window.addEventListener("resize", debounce(render, 120));
      render();

      function render() {
        const metric = metricSelect.value;
        const scale = scaleSelect.value;
        const sort = sortSelect.value;
        const category = categorySelect.value;
        const rows = tests
          .filter((test) => category === "all" || test.category === category)
          .sort((a, b) => {
            if (sort === "test") return byTestName(a, b);
            return engineMetric(b, sort, metric, config.baselineEngine) - engineMetric(a, sort, metric, config.baselineEngine);
          });

        config.renderSummary(tests.slice().sort(byTestName), summaryBody);
        renderD3Chart({
          host,
          rows: rows.slice(0, MAX_ROWS),
          engines,
          labels,
          metric,
          scale,
          baselineEngine: config.baselineEngine,
          ratioAxisLabel: config.ratioAxisLabel,
        });
      }
    }).catch((error) => {
      host.textContent = `Failed to load benchmark data: ${error.message}`;
    });
  }

  function renderD3Chart(options) {
    const { host, rows, engines, labels, metric, scale, baselineEngine, ratioAxisLabel } = options;
    host.innerHTML = "";

    if (!rows.length) {
      host.innerHTML = '<div class="bench-chart-empty">No benchmark rows for this selection.</div>';
      return;
    }

    const width = Math.max(1040, Math.floor(host.clientWidth || 1180));
    const isSmall = width < 860;
    const margin = {
      top: 76,
      right: isSmall ? 28 : 46,
      bottom: 64,
      left: isSmall ? 260 : 350,
    };
    const rowHeight = isSmall ? 38 : 42;
    const height = margin.top + margin.bottom + rows.length * rowHeight;
    const innerWidth = Math.max(240, width - margin.left - margin.right);
    const innerHeight = rows.length * rowHeight;
    const barGroupHeight = Math.min(28, rowHeight - 10);
    const barHeight = Math.max(6, Math.min(9, barGroupHeight / engines.length - 1));

    const svg = d3
      .select(host)
      .append("svg")
      .attr("viewBox", `0 0 ${width} ${height}`)
      .attr("width", width)
      .attr("height", height)
      .attr("role", "img")
      .attr("aria-label", "Benchmark throughput comparison");

    const defs = svg.append("defs");
    const shadow = defs.append("filter").attr("id", `${host.id}-shadow`).attr("x", "-20%").attr("y", "-20%").attr("width", "140%").attr("height", "140%");
    shadow.append("feDropShadow").attr("dx", 0).attr("dy", 1).attr("stdDeviation", 1).attr("flood-color", "#0f172a").attr("flood-opacity", 0.08);

    const data = rows.flatMap((test, rowIndex) =>
      engines.map((engineId, engineIndex) => {
        const raw = engineMetric(test, engineId, metric, baselineEngine);
        const row = test.engines[engineId];
        return {
          test,
          rowIndex,
          engineId,
          engineIndex,
          raw,
          row,
          ok: Boolean(row && row.status === "OK" && Number.isFinite(row.ops) && row.ops > 0),
          value: chartValue(raw, metric, scale),
        };
      }),
    );
    const validValues = data.map((point) => point.value).filter(Number.isFinite);

    let x;
    let xZero;
    let tickValues = null;
    if (metric === "ratio" && scale === "log") {
      const extent = Math.max(0.4, ...validValues.map((value) => Math.abs(value)));
      const padded = Math.min(3, extent * 1.1);
      x = d3.scaleLinear().domain([-padded, padded]).range([margin.left, margin.left + innerWidth]).nice();
      xZero = x(0);
      tickValues = [-3, -2, -1, -0.5, 0, 0.5, 1, 2, 3].filter((value) => value >= x.domain()[0] && value <= x.domain()[1]);
    } else if (metric === "ratio") {
      const max = Math.max(1.2, ...validValues);
      x = d3.scaleLinear().domain([0, max * 1.08]).range([margin.left, margin.left + innerWidth]).nice();
      xZero = x(0);
    } else if (scale === "log") {
      const positive = validValues.filter((value) => value > 0);
      const min = Math.max(0.001, Math.min(...positive) / 1.4);
      const max = Math.max(...positive) * 1.2;
      x = d3.scaleLog().domain([min, max]).range([margin.left, margin.left + innerWidth]).nice();
      xZero = x(min);
    } else {
      const max = Math.max(1, ...validValues);
      x = d3.scaleLinear().domain([0, max * 1.08]).range([margin.left, margin.left + innerWidth]).nice();
      xZero = x(0);
    }

    const y = d3
      .scaleBand()
      .domain(rows.map((test) => test.test))
      .range([margin.top, margin.top + innerHeight])
      .paddingInner(0.22)
      .paddingOuter(0.08);

    renderLegend(svg, engines, labels, width);

    const plot = svg.append("g").attr("class", "bench-d3-plot");

    plot
      .append("g")
      .attr("class", "bench-d3-grid")
      .attr("transform", `translate(0,${margin.top + innerHeight})`)
      .call(
        d3
          .axisBottom(x)
          .tickValues(tickValues)
          .ticks(metric === "ops" && scale === "log" ? 5 : 6)
          .tickSize(-innerHeight)
          .tickFormat((value) => axisTick(value, metric, scale)),
      )
      .call((group) => group.select(".domain").remove())
      .call((group) => group.selectAll("text").attr("dy", "1.4em"));

    if (metric === "ratio") {
      plot
        .append("line")
        .attr("class", "bench-d3-parity")
        .attr("x1", xZero)
        .attr("x2", xZero)
        .attr("y1", margin.top - 8)
        .attr("y2", margin.top + innerHeight)
        .attr("stroke", "#475569")
        .attr("stroke-width", 1.4)
        .attr("stroke-dasharray", "4 4");
      plot
        .append("text")
        .attr("class", "bench-d3-axis-label")
        .attr("x", xZero + 7)
        .attr("y", margin.top - 14)
        .text("1x baseline");
    }

    const rowGroups = plot
      .selectAll(".bench-d3-row")
      .data(rows)
      .enter()
      .append("g")
      .attr("class", "bench-d3-row")
      .attr("transform", (test) => `translate(0,${y(test.test)})`);

    rowGroups
      .append("line")
      .attr("x1", margin.left)
      .attr("x2", margin.left + innerWidth)
      .attr("y1", y.bandwidth() + 4)
      .attr("y2", y.bandwidth() + 4)
      .attr("stroke", "#eef2f7");

    rowGroups
      .append("text")
      .attr("class", "bench-d3-label")
      .attr("x", margin.left - 14)
      .attr("y", y.bandwidth() / 2 + 4)
      .attr("text-anchor", "end")
      .text((test) => trimLabel(test.test, isSmall ? 32 : 52));

    const points = plot
      .selectAll(".bench-d3-bar")
      .data(data.filter((point) => point.ok && Number.isFinite(point.value)))
      .enter()
      .append("g")
      .attr("class", "bench-d3-bar")
      .attr("transform", (point) => {
        const baseY = y(point.test.test) + (y.bandwidth() - barGroupHeight) / 2;
        const offset = point.engineIndex * (barHeight + 2);
        return `translate(0,${baseY + offset})`;
      });

    points
      .append("rect")
      .attr("x", (point) => barX(point.value, x, xZero, metric, scale))
      .attr("y", 0)
      .attr("width", (point) => barWidth(point.value, x, xZero, metric, scale))
      .attr("height", barHeight)
      .attr("rx", 2)
      .attr("fill", (point) => COLORS[point.engineId] || "#475569")
      .attr("filter", `url(#${host.id}-shadow)`)
      .on("mousemove", (event, point) => showTooltip(event, point, labels, metric, baselineEngine))
      .on("mouseleave", hideTooltip);

    points
      .filter((point) => metric === "ratio" && point.engineId === baselineEngine)
      .append("circle")
      .attr("cx", xZero)
      .attr("cy", barHeight / 2)
      .attr("r", 4.5)
      .attr("fill", (point) => COLORS[point.engineId] || "#475569")
      .attr("stroke", "#ffffff")
      .attr("stroke-width", 1.5);

    svg
      .append("text")
      .attr("class", "bench-d3-axis-title")
      .attr("x", margin.left + innerWidth / 2)
      .attr("y", height - 16)
      .attr("text-anchor", "middle")
      .text(metric === "ratio" ? ratioAxisLabel : "operations per second");
  }

  function renderLegend(svg, engines, labels, width) {
    const legend = svg.append("g").attr("class", "bench-d3-legend").attr("transform", `translate(${Math.max(20, width / 2 - engines.length * 82)}, 28)`);
    let x = 0;
    for (const engineId of engines) {
      const item = legend.append("g").attr("transform", `translate(${x},0)`);
      item.append("rect").attr("width", 14).attr("height", 10).attr("rx", 2).attr("fill", COLORS[engineId] || "#475569");
      item.append("text").attr("x", 20).attr("y", 9).text(labels[engineId] || engineId);
      x += Math.max(92, (labels[engineId] || engineId).length * 7.5 + 34);
    }
  }

  function barX(value, x, xZero, metric, scale) {
    if (metric === "ratio" && scale === "log") return Math.min(x(value), xZero);
    if (metric === "ops" && scale === "log") return x.range()[0];
    return Math.min(x(value), xZero);
  }

  function barWidth(value, x, xZero, metric, scale) {
    if (metric === "ratio" && scale === "log") return Math.max(2, Math.abs(x(value) - xZero));
    if (metric === "ops" && scale === "log") return Math.max(2, x(value) - x.range()[0]);
    return Math.max(2, Math.abs(x(value) - xZero));
  }

  function showTooltip(event, point, labels, metric, baselineEngine) {
    let tooltip = document.querySelector(".bench-d3-tooltip");
    if (!tooltip) {
      tooltip = document.createElement("div");
      tooltip.className = "bench-d3-tooltip";
      document.body.appendChild(tooltip);
    }

    const baseline = point.test.engines[baselineEngine];
    const ratio = baseline && baseline.status === "OK" && baseline.ops > 0 ? point.row.ops / baseline.ops : null;
    tooltip.innerHTML = [
      `<strong>${escapeHtml(point.test.test)}</strong>`,
      `<span>${escapeHtml(labels[point.engineId] || point.engineId)}</span>`,
      `<code>${formatOps(point.row.ops)} ops/s</code>`,
      metric === "ratio" && ratio ? `<code>${formatRatio(ratio)}</code>` : "",
      point.row.mean_ms != null ? `<small>${point.row.mean_ms.toFixed(point.row.mean_ms < 10 ? 3 : 1)} ms avg</small>` : "",
    ].join("");
    tooltip.style.left = `${event.clientX + 14}px`;
    tooltip.style.top = `${event.clientY + 14}px`;
    tooltip.classList.add("is-visible");
  }

  function hideTooltip() {
    const tooltip = document.querySelector(".bench-d3-tooltip");
    if (tooltip) tooltip.classList.remove("is-visible");
  }

  function renderMainSummary(tests, summaryBody) {
    if (!summaryBody) return;
    if (!tests.length) {
      summaryBody.innerHTML = '<tr><td colspan="6">No benchmark rows for this selection.</td></tr>';
      return;
    }
    summaryBody.innerHTML = tests
      .map((test) => {
        const aion = test.engines.aiondb;
        const surreal = test.engines.surrealdb;
        const pg = test.engines.pgstack;
        const ratio = aion && aion.status === "OK" && pg && pg.status === "OK" && pg.ops > 0 ? aion.ops / pg.ops : null;
        const rowClass = aionRankClass(aion, surreal, pg);
        const trClass = rowClass ? ` class="${rowClass}"` : "";
        return [
          `<tr${trClass}>`,
          `<td><code>${escapeHtml(test.test)}</code></td>`,
          `<td>${escapeHtml(test.category)}</td>`,
          `<td>${formatEngineCell(aion)}</td>`,
          `<td>${formatEngineCell(surreal)}</td>`,
          `<td>${formatEngineCell(pg)}</td>`,
          `<td>${ratio == null ? "-" : formatRatio(ratio)}</td>`,
          "</tr>",
        ].join("");
      })
      .join("");
  }

  function renderEmbeddedSummary(tests, summaryBody) {
    if (!summaryBody) return;
    if (!tests.length) {
      summaryBody.innerHTML = '<tr><td colspan="5">No embedded benchmark rows for this selection.</td></tr>';
      return;
    }
    summaryBody.innerHTML = tests
      .map((test) => {
        const aion = test.engines.aiondb_embedded;
        const surreal = test.engines.surrealdb_embedded_mem;
        const ratio = aion && surreal && aion.status === "OK" && surreal.status === "OK" && surreal.ops > 0 ? aion.ops / surreal.ops : null;
        const rowClass = ratio == null ? "" : ratio >= 1 ? "bench-row-win" : "bench-row-loss";
        const trClass = rowClass ? ` class="${rowClass}"` : "";
        return [
          `<tr${trClass}>`,
          `<td><code>${escapeHtml(test.test)}</code></td>`,
          `<td>${escapeHtml(test.category)}</td>`,
          `<td>${formatEngineCell(aion, true)}</td>`,
          `<td>${formatEngineCell(surreal, true)}</td>`,
          `<td>${ratio == null ? "-" : formatRatio(ratio)}</td>`,
          "</tr>",
        ].join("");
      })
      .join("");
  }

  function aionRankClass(aion, surreal, pg) {
    if (!aion || aion.status !== "OK") return "";
    const surrealOk = surreal && surreal.status === "OK";
    const pgOk = pg && pg.status === "OK";
    const beatsSurreal = !surrealOk || aion.ops >= surreal.ops;
    const beatsPg = !pgOk || aion.ops >= pg.ops;
    if (beatsSurreal && beatsPg) return "bench-row-win";
    if (beatsSurreal && !beatsPg) return "bench-row-mid";
    return "bench-row-loss";
  }

  function engineMetric(test, engineId, metric, baselineEngine) {
    const row = test.engines[engineId];
    if (!row || row.status !== "OK") return 0;
    if (metric === "ops") return row.ops;
    const baseline = test.engines[baselineEngine];
    if (!baseline || baseline.status !== "OK" || baseline.ops <= 0) return 0;
    return row.ops / baseline.ops;
  }

  function chartValue(value, metric, scale) {
    if (!Number.isFinite(value) || value <= 0) return NaN;
    return metric === "ratio" && scale === "log" ? Math.log10(value) : value;
  }

  function axisTick(value, metric, scale) {
    if (metric === "ratio" && scale === "log") return formatRatio(Math.pow(10, value));
    if (metric === "ratio") return formatRatio(value);
    return formatOps(value);
  }

  function formatEngineCell(row, includeP95 = false) {
    if (!row) return "-";
    if (row.status !== "OK") return `<span class="bench-status">${escapeHtml(row.status)}</span>`;
    const ops = `<span class="bench-ops"><strong>${formatOps(row.ops)}</strong> <span class="bench-unit">ops/s</span></span>`;
    const avg =
      row.mean_ms != null
        ? `<span class="bench-ms">${includeP95 ? "avg " : ""}${row.mean_ms < 10 ? row.mean_ms.toFixed(includeP95 ? 3 : 2) : row.mean_ms.toFixed(1)} ms</span>`
        : "";
    const p95 = includeP95 && row.p95_ms != null ? `<span class="bench-ms">p95 ${row.p95_ms.toFixed(3)} ms</span>` : "";
    return `<span class="bench-engine-cell">${ops}${avg ? `<br>${avg}` : ""}${p95 ? `<br>${p95}` : ""}</span>`;
  }

  function formatRatio(value) {
    if (!Number.isFinite(value) || value <= 0) return "-";
    if (value < 0.01) return `${value.toPrecision(1)}x`;
    if (value < 0.1) return `${value.toFixed(2)}x`;
    if (value >= 100) return `${value.toFixed(0)}x`;
    return `${value >= 10 ? value.toFixed(1) : value.toFixed(2)}x`;
  }

  function formatOps(value) {
    if (!Number.isFinite(value) || value <= 0) return "-";
    if (value >= 1000000) return `${(value / 1000000).toFixed(1)}M`;
    if (value >= 1000) return `${(value / 1000).toFixed(value >= 10000 ? 0 : 1)}k`;
    if (value >= 100) return `${Math.round(value)}`;
    if (value >= 10) return `${value.toFixed(1)}`;
    return `${value.toFixed(2)}`;
  }

  function trimLabel(label, max) {
    return label.length > max ? `${label.slice(0, max - 3)}...` : label;
  }

  function debounce(fn, delay) {
    let timer = 0;
    return () => {
      clearTimeout(timer);
      timer = setTimeout(fn, delay);
    };
  }

  function escapeAttr(value) {
    return String(value).replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;");
  }

  function escapeHtml(value) {
    return String(value)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;");
  }
})();
