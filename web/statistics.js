"use strict";
(() => {
  const invoke = window.__TAURI__.core.invoke;
  const byId = (id) => document.getElementById(id);
  const number = (value) => Number(value).toLocaleString();
  const usd = (value) => new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 4 }).format(value);
  const percent = (part, total) => `${total ? (part / total * 100).toFixed(1) : "0.0"}%`;
  const recentSlider = byId("interval-recent"), olderSlider = byId("interval-older");
  const interval = new globalThis.StatisticsInterval();
  let version = 0, busy = false, rerun = false, timer;
  const date = (ms) => new Date(ms).toLocaleString();

  function labels() {
    const { anchor, recentAge, olderAge, earliest } = interval;
    byId("interval-mode").textContent = interval.live ? "Live · follows now" : "Fixed historical interval";
    byId("interval-recent-label").textContent = interval.live ? `Now · ${date(anchor)}` : date(anchor - recentAge);
    byId("interval-older-label").textContent = olderAge === null ? `Earliest · ${date(earliest)}` : date(anchor - olderAge);
    recentSlider.setAttribute("aria-valuetext", interval.live ? "Now, live updates" : date(anchor - recentAge));
    olderSlider.setAttribute("aria-valuetext", date(olderAge === null ? earliest : anchor - olderAge));
  }
  function syncSliders() {
    const positions = interval.positions();
    recentSlider.value = positions.recent;
    olderSlider.value = positions.older;
    labels();
  }
  function render(stats, start, end) {
    const set = (id, text) => { byId("stat-" + id).textContent = text; };
    const hours = Math.max(1 / 3600, (end - (start ?? stats.earliest_ms ?? end)) / 3600000);
    set("calls", number(stats.calls)); set("calls-note", `${stats.models.length} models · ${(stats.calls / hours).toFixed(1)} calls/hour`);
    set("cost", stats.priced_calls ? usd(stats.estimated_usd) : "—");
    set("cost-note", `${number(stats.priced_calls)} priced · ${number(stats.calls - stats.priced_calls)} need pricing`);
    set("input", number(stats.total_input)); set("input-note", `${number(stats.ordinary_input)} ordinary input tokens`);
    set("output", number(stats.output)); set("output-note", `${number(stats.total_input + stats.output)} total tokens`);
    set("uncached", number(stats.uncached)); set("uncached-note", `Peak call ${number(stats.max_uncached)} · average ${number(Math.round(stats.calls ? stats.uncached / stats.calls : 0))}`);
    set("read", number(stats.cache_reads)); set("read-note", `${percent(stats.cache_reads, stats.total_input)} of input cached · ${usd(stats.cache_read_discount_usd)} read discount vs fresh input`);
    set("write", number(stats.cache_writes)); set("write-note", `${number(stats.cache_1h)} known 1h write tokens`);
    set("misses", number(stats.zero_cache_calls)); set("misses-note", `${percent(stats.zero_cache_calls, stats.calls)} of calls`);
    set("expensive", number(stats.expensive_calls));
    set("alerts", number(stats.alerts_sent)); set("alerts-note", `${number(stats.alerts_failed)} failed · ${number(stats.alerts_pending)} queued`);
    set("sessions", number(stats.sessions)); set("sessions-note", `${number(stats.subagents)} subagents · ${number(stats.projects)} projects`);
    set("average", stats.priced_calls ? usd(stats.estimated_usd / stats.priced_calls) : "—");
    set("average-note", stats.priced_calls ? `Most expensive call ${usd(stats.max_cost_usd)}` : "No priced calls in this interval");
    byId("model-stat-rows").replaceChildren(...stats.models.map((model) => {
      const tr = document.createElement("tr");
      const values = [model.model || "Unknown model", number(model.calls), model.priced_calls ? usd(model.estimated_usd) : "—", number(model.uncached_tokens), number(model.cache_read_tokens), number(model.output_tokens), number(model.calls - model.priced_calls)];
      for (const [index, value] of values.entries()) { const td = document.createElement("td"); td.textContent = value; if (index) td.className = "num"; tr.append(td); }
      return tr;
    }));
    byId("statistics-state").textContent = `${interval.live ? "Updated" : "Historical snapshot"} ${date(Date.now())} · ${number(stats.undated_calls)} calls without valid timestamps are excluded from interval statistics.`;
  }
  async function update() {
    if (busy) { rerun = true; return; }
    busy = true;
    const currentVersion = version;
    const { start, end } = interval.bounds();
    labels();
    try {
      const stats = await invoke("statistics", { startMs: start, endMs: end });
      if (currentVersion !== version) return;
      interval.earliest = stats.earliest_ms ?? interval.anchor - 86400000;
      syncSliders(); render(stats, start, end);
    } catch (error) { if (currentVersion === version) byId("statistics-state").textContent = `Statistics unavailable: ${error}`; }
    finally { busy = false; if (rerun) { rerun = false; update(); } }
  }
  function schedule(invalidate = true) { if (invalidate) version++; clearTimeout(timer); timer = setTimeout(update, 100); }
  function selectInterval(event) {
    interval.select(event.target === recentSlider ? "recent" : "older", Number(recentSlider.value), Number(olderSlider.value));
    syncSliders(); schedule();
  }
  recentSlider.addEventListener("input", selectInterval);
  olderSlider.addEventListener("input", selectInterval);
  for (const button of document.querySelectorAll("[data-hours]")) button.addEventListener("click", () => {
    interval.preset(button.dataset.hours === "all" ? null : Number(button.dataset.hours));
    syncSliders(); schedule();
  });
  window.refreshStatistics = () => schedule();
  update();
  setInterval(() => { if (interval.live) schedule(false); }, 3000);
})();
