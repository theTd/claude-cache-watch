"use strict";
(() => {
  const pricedSum = (key) => (p) => p.priced_calls || !p.calls ? p[key] : null;
  const direct = (id, label, unit = "count") => ({ id, label, unit, value: (p) => p[id] });
  const metrics = [
    direct("calls", "API calls"),
    { id: "calls_per_hour", label: "API calls per hour", unit: "rate", value: (p) => p.calls * 3600000 / Math.max(1, p.end_ms - p.start_ms + 1) },
    { id: "estimated_usd", label: "Estimated API cost", unit: "usd", value: pricedSum("estimated_usd") },
    direct("priced_calls", "Priced calls"),
    { id: "unpriced_calls", label: "Calls needing pricing", unit: "count", value: (p) => p.calls - p.priced_calls },
    { id: "average_cost", label: "Average priced call cost", unit: "usd", value: (p) => p.priced_calls ? p.estimated_usd / p.priced_calls : null },
    { id: "max_cost_usd", label: "Most expensive call", unit: "usd", value: (p) => p.priced_calls ? p.max_cost_usd : null },
    direct("total_input", "Total input tokens", "tokens"), direct("ordinary_input", "Ordinary input tokens", "tokens"),
    direct("uncached", "Uncached input tokens", "tokens"),
    { id: "average_uncached", label: "Average uncached input per call", unit: "tokens", value: (p) => p.calls ? p.uncached / p.calls : null },
    direct("max_uncached", "Peak uncached input per call", "tokens"), direct("output", "Output tokens", "tokens"),
    { id: "total_tokens", label: "Total input + output tokens", unit: "tokens", value: (p) => p.total_input + p.output },
    direct("cache_reads", "Cache-read tokens", "tokens"),
    { id: "cache_hit_ratio", label: "Cached share of input", unit: "percent", value: (p) => p.total_input ? p.cache_reads / p.total_input * 100 : null },
    direct("cache_writes", "Cache-write tokens", "tokens"), direct("cache_1h", "Known 1h cache-write tokens", "tokens"),
    { id: "cache_5m", label: "5m / unspecified cache-write tokens", unit: "tokens", value: (p) => Math.max(0, p.cache_writes - p.cache_1h) },
    { id: "cache_read_discount_usd", label: "Cache-read discount vs fresh input", unit: "usd", value: pricedSum("cache_read_discount_usd") },
    direct("zero_cache_calls", "Calls with zero cache reads"),
    { id: "zero_cache_ratio", label: "Share of calls with zero cache reads", unit: "percent", value: (p) => p.calls ? p.zero_cache_calls / p.calls * 100 : null },
    direct("expensive_calls", "High uncached input calls"),
    direct("alerts_sent", "Alerts submitted (by call time)"), direct("alerts_failed", "Failed alerts (by call time)"), direct("alerts_pending", "Queued alerts (by call time)"),
    direct("sessions", "Active sessions"), direct("subagents", "Active subagents"), direct("projects", "Active projects"), direct("model_count", "Active models"),
  ];
  function seriesFor(trend, metricId) {
    const metric = metrics.find((m) => m.id === metricId);
    return metric ? trend.points.map((p) => {
      const value = metric.value(p);
      return typeof value === "number" && Number.isFinite(value) ? value : null;
    }) : [];
  }
  function geometry(values, width, height, padding, fixedMax) {
    const finite = values.filter((v) => v !== null && Number.isFinite(v));
    const observed = finite.length ? Math.max(...finite) : 0;
    const max = fixedMax || (observed > 0 ? observed : 1);
    const x = (index) => padding.left + (values.length === 1 ? 0.5 : index / Math.max(1, values.length - 1)) * (width - padding.left - padding.right);
    const y = (value) => height - padding.bottom - value / max * (height - padding.top - padding.bottom);
    let path = "", connected = false;
    values.forEach((value, index) => {
      if (value === null) { connected = false; return; }
      path += `${connected ? "L" : "M"}${x(index).toFixed(2)},${y(value).toFixed(2)} `; connected = true;
    });
    const isolated = values.flatMap((value, index) => value !== null
      && (index === 0 || values[index - 1] === null)
      && (index === values.length - 1 || values[index + 1] === null) ? [index] : []);
    return { path: path.trim(), max, x, y, isolated };
  }
  globalThis.TrendCharts = { metrics, seriesFor, geometry };
  if (typeof document === "undefined") return;
  const $ = (id) => document.getElementById(id);
  const svg = (name, attributes = {}) => {
    const element = document.createElementNS("http://www.w3.org/2000/svg", name);
    for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
    return element;
  };
  const format = (value, unit) => {
    if (value === null) return "No value";
    if (unit === "usd") return new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 6 }).format(value);
    return `${new Intl.NumberFormat(undefined, { maximumFractionDigits: unit === "percent" || unit === "rate" ? 2 : 1 }).format(value)}${unit === "percent" ? "%" : ""}`;
  };
  const compact = (value, unit) => unit === "percent" ? `${Math.round(value)}%` : `${unit === "usd" ? "$" : ""}${new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 2 }).format(value)}`;
  const duration = (ms) => ms >= 86400000 ? `${(ms / 86400000).toFixed(1)} days` : ms >= 3600000 ? `${(ms / 3600000).toFixed(1)} hours` : ms >= 60000 ? `${Math.round(ms / 60000)} min` : `${Math.max(1, Math.round(ms / 1000))} sec`;
  const chart = $("trend-chart"), selector = $("trend-metric");
  let current = null, selected = "calls", inspected = null, plot = null, values = [];
  const padding = { left: 68, right: 18, top: 20, bottom: 42 }, width = 1000, height = 280;
  for (const metric of metrics) {
    const option = document.createElement("option"); option.value = metric.id; option.textContent = metric.label; selector.append(option);
  }
  const cardMetrics = { calls: "calls", cost: "estimated_usd", input: "total_input", output: "output", uncached: "uncached", read: "cache_reads", write: "cache_writes", misses: "zero_cache_calls", expensive: "expensive_calls", alerts: "alerts_sent", sessions: "sessions", average: "average_cost" };
  const sparks = new Map();
  for (const [card, metricId] of Object.entries(cardMetrics)) {
    const button = document.createElement("button"); button.type = "button"; button.className = "card-trend";
    const name = metrics.find((m) => m.id === metricId).label;
    button.setAttribute("aria-label", `Show ${name} trend`); button.title = `Explore ${name}`;
    const image = svg("svg", { viewBox: "0 0 220 34", "aria-hidden": "true", preserveAspectRatio: "none" });
    const line = svg("path", { fill: "none", stroke: "currentColor", "stroke-width": 1.8, "vector-effect": "non-scaling-stroke" });
    const markers = svg("g");
    image.append(line, markers); button.append(image); $("stat-" + card).parentElement.append(button);
    button.addEventListener("click", () => { selected = metricId; selector.value = metricId; draw(); $("trend-panel").scrollIntoView({ behavior: "smooth", block: "center" }); });
    sparks.set(metricId, { button, line, markers });
  }
  function inspect(index) {
    if (!current?.points.length || !plot) return;
    inspected = Math.max(0, Math.min(current.points.length - 1, index));
    const point = current.points[inspected], value = values[inspected], metric = metrics.find((m) => m.id === selected);
    const crosshair = $("trend-crosshair"), dot = $("trend-dot");
    crosshair.setAttribute("x1", plot.x(inspected)); crosshair.setAttribute("x2", plot.x(inspected)); crosshair.setAttribute("visibility", "visible");
    dot.setAttribute("visibility", value === null ? "hidden" : "visible");
    if (value !== null) { dot.setAttribute("cx", plot.x(inspected)); dot.setAttribute("cy", plot.y(value)); }
    $("trend-reading").textContent = `${new Date(point.start_ms).toLocaleString()} – ${new Date(point.end_ms).toLocaleString()} · ${metric.label}: ${format(value, metric.unit)} · ${point.calls.toLocaleString()} calls · ${point.priced_calls.toLocaleString()} priced`;
  }
  function draw() {
    if (!current?.points.length) return;
    const metric = metrics.find((m) => m.id === selected);
    values = seriesFor(current, selected); plot = geometry(values, width, height, padding, metric.unit === "percent" ? 100 : undefined);
    $("trend-title").textContent = metric.label;
    $("trend-caption").textContent = `${current.points.length} time buckets · up to ${duration(current.bucket_ms)} each · oldest to newest`;
    chart.setAttribute("aria-label", `${metric.label} trend. Use left and right arrow keys to inspect time buckets.`); chart.replaceChildren();
    for (let i = 0; i <= 4; i++) {
      const value = plot.max * i / 4, y = plot.y(value);
      const label = svg("text", { x: padding.left - 10, y: y + 4, "text-anchor": "end", class: "chart-label" }); label.textContent = compact(value, metric.unit);
      chart.append(svg("line", { x1: padding.left, x2: width - padding.right, y1: y, y2: y, class: "chart-grid" }), label);
    }
    for (const index of [...new Set([0, Math.floor((current.points.length - 1) / 2), current.points.length - 1])]) {
      const label = svg("text", { x: plot.x(index), y: height - 12, "text-anchor": index === 0 ? "start" : index === current.points.length - 1 ? "end" : "middle", class: "chart-label" });
      label.textContent = new Date(current.points[index].start_ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }); chart.append(label);
    }
    chart.append(svg("path", { d: plot.path, fill: "none", class: "chart-line" }));
    values.forEach((value, index) => { if (value !== null) chart.append(svg("circle", { cx: plot.x(index), cy: plot.y(value), r: values.length === 1 ? 4 : 2, class: "chart-point" })); });
    chart.append(svg("line", { id: "trend-crosshair", y1: padding.top, y2: height - padding.bottom, class: "chart-crosshair", visibility: "hidden" }), svg("circle", { id: "trend-dot", r: 5, class: "chart-focus", visibility: "hidden" }));
    $("trend-empty").hidden = current.points.some((p) => p.calls > 0);
    if (inspected !== null) inspect(inspected);
    else $("trend-reading").textContent = values.every((v) => v === null) ? "No defined values for this metric. Check pricing coverage or choose another metric." : "Hover a point, or focus the chart and use arrow keys, for exact times and values.";
    for (const [id, spark] of sparks) {
      const series = seriesFor(current, id), shape = geometry(series, 220, 34, { left: 2, right: 2, top: 3, bottom: 3 });
      spark.line.setAttribute("d", shape.path);
      spark.markers.replaceChildren(...shape.isolated.map((index) => svg("circle", { cx: shape.x(index), cy: shape.y(series[index]), r: 2.5, fill: "currentColor" })));
      spark.button.classList.toggle("selected", id === selected);
    }
  }
  selector.addEventListener("change", () => { selected = selector.value; inspected = null; draw(); });
  chart.addEventListener("pointermove", (event) => {
    if (!current?.points.length) return;
    const bounds = chart.getBoundingClientRect(), x = (event.clientX - bounds.left) / bounds.width * width;
    inspect(Math.round((x - padding.left) / (width - padding.left - padding.right) * (current.points.length - 1)));
  });
  chart.addEventListener("keydown", (event) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key) || !current?.points.length) return;
    event.preventDefault(); inspect(event.key === "Home" ? 0 : event.key === "End" ? current.points.length - 1 : (inspected ?? 0) + (event.key === "ArrowLeft" ? -1 : 1));
  });
  window.renderTrends = (trend) => { current = trend; draw(); };
})();
