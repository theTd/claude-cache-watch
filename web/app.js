"use strict";
const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);
const number = (value) => Number(value).toLocaleString();
const dollars = (value) => new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 4, maximumFractionDigits: 6 }).format(value);
const sourceNames = {claude:"Claude Code",antigravity_ide:"Antigravity IDE",antigravity_cli:"Antigravity CLI"};
window.cacheWatchFilter = () => ({ source:$("source-filter").value, kind:$("kind-filter").value });
let page = 0, dirty = false, busy = false, queryVersion = 0;

function showError(error) {
  $("error").textContent = String(error || "");
  $("error").hidden = !error;
}

function cell(text, className, title) {
  const element = document.createElement("td");
  element.textContent = text;
  if (className) element.className = className;
  if (title) element.title = title;
  return element;
}

function renderRow(row, expandedBilling) {
  const tr = document.createElement("tr");
  const date = new Date(row.timestamp);
  const first = cell("");
  const title = document.createElement("span");
  title.className = "session-title";
  title.textContent = row.session_title || row.session || "Untitled session";
  first.append(title);
  if (row.agent_id) {
    const agent = document.createElement("span");
    agent.className = "agent-title";
    agent.textContent = `↳ ${row.subagent_title || `Subagent ${row.agent_id}`}`;
    first.append(agent);
  }
  const detail = document.createElement("span");
  detail.className = "detail";
  detail.textContent = `${sourceNames[row.source_kind] || row.source_kind || "Claude Code"} · ${row.kind || "response"} · ${row.state || "DONE"}${row.usage_complete === false ? " · Incomplete usage" : ""} · ${Number.isNaN(date.getTime()) ? "Unknown time" : date.toLocaleString()} · ${row.project || "Unknown project"}`;
  first.title = `${row.session_title}\n${row.subagent_title || ""}\n${row.project}\nSession: ${row.raw_session || row.session}
Parent: ${row.parent_session || "—"}
Usage source: ${row.metadata_source || "claude_transcript"}\nAgent: ${row.agent_id}\nRequest: ${row.id}`;
  first.append(detail);
  tr.append(first, cell(row.model || "Unknown", "", row.model),
    cell(!row.usage_complete && row.input === 0 ? "Unknown" : number(row.input + row.creation), "num uncached", `Ordinary input: ${number(row.input)}`),
    cell(row.creation_known === false ? "Unknown" : number(row.creation), "num"), cell(row.cache_known === false ? "Unknown" : number(row.read), "num"), cell(!row.usage_complete && row.output === 0 ? "Unknown" : number(row.output), "num"));
  const cost = cell("", "num");
  if (row.estimated_billing) {
    const bill = row.estimated_billing;
    const details = document.createElement("details"); details.className = "billing-detail";
    details.dataset.requestId = row.id;
    details.open = expandedBilling.has(row.id);
    const summary = document.createElement("summary"); summary.textContent = dollars(bill.total_usd);
    const breakdown = document.createElement("p");
    const rates = bill.rates_per_million;
    breakdown.textContent = `Input ${dollars(bill.input_usd)} · Cache writes ${bill.cache_write_usd == null ? "Unknown" : dollars(bill.cache_write_usd)} · Cache reads ${dollars(bill.cache_read_usd)} · Output ${dollars(bill.output_usd)} · Web search ${bill.web_search_usd == null ? "Unreported" : dollars(bill.web_search_usd)}. Rates per million (input / 5m write / 1h write / read / output): ${rates.input} / ${rates.cache_5m} / ${rates.cache_1h} / ${rates.read} / ${rates.output}. ${bill.basis}. ${bill.notes.join(" ")}`;
    details.append(summary, breakdown); cost.append(details);
  } else {
    cost.textContent = "Pricing needed";
    cost.title = "Set custom rates under API billing rates. This model or speed has no verified price.";
    cost.classList.add("muted");
  }
  tr.append(cost);
  const alert = document.createElement("td");
  const badge = document.createElement("span");
  if (row.alert) {
    badge.className = `badge ${row.alert}`;
    badge.textContent = row.alert === "sent" ? "Submitted" : row.alert;
    badge.title = row.alert_error || "Windows accepted the notification; banner visibility depends on Windows settings.";
  } else {
    badge.className = "muted";
    badge.textContent = row.imported ? "Imported" : "—";
  }
  alert.append(badge); tr.append(alert);
  return tr;
}

async function refresh() {
  if (busy || !window.dashboardRefresh.active) return;
  busy = true;
  const version = queryVersion;
  try {
    const data = await invoke("snapshot", { search: $("search").value, alertsOnly: $("alerts-only").checked, page, filter:window.cacheWatchFilter() });
    if (version !== queryVersion) return;
    const { settings, status, history } = data;
    if (!dirty) {
      $("source").value = settings.source;
      for (const source of settings.sources || []) {
        const input = source.kind === "claude" ? $("source") : $("source-"+source.kind);
        if (input) input.value=source.path;
        const toggle=$("enabled-"+source.kind); if (toggle) toggle.checked=source.enabled;
      }
      $("threshold").value = settings.threshold;
      $("zero-cache").checked = settings.zero_cache_only;
      $("paused").checked = settings.paused;
    }
    $("status").textContent = settings.paused ? "● Monitoring paused" : status.error ? "● Needs attention" : !status.ready ? "● Starting…" : status.importing ? "● Importing history" : "● Monitoring";
    $("status").classList.toggle("warn", settings.paused || Boolean(status.error));
    showError(status.error);
    const expandedBilling = new Set([...$("rows").querySelectorAll("details[open]")].map((details) => details.dataset.requestId));
    $("rows").replaceChildren(...history.rows.map((row) => renderRow(row, expandedBilling)));
    if (!history.rows.length) {
      const row = document.createElement("tr");
      const empty = cell("No matching calls yet. Logs appear after a monitored client records model usage.", "empty");
      empty.colSpan = 8; row.append(empty); $("rows").append(row);
    }
    $("count").textContent = `${number(history.total)} matching calls · 100 per page`;
    const priced = history.rows.filter((row) => row.estimated_billing);
    $("page-billing").textContent = `This page: ${dollars(priced.reduce((sum, row) => sum + row.estimated_billing.total_usd, 0))} estimated API cost · ${priced.length} priced calls · ${history.rows.length - priced.length} need pricing`;
    $("page").textContent = page + 1;
    $("prev").disabled = page === 0;
    $("next").disabled = (page + 1) * 100 >= history.total;
    $("scan").textContent = status.last_scan ? `${status.files} log files · Last scan ${new Date(status.last_scan * 1000).toLocaleTimeString()} · ${status.data_path}` : "Waiting for first scan…";
  } catch (error) { showError(error); }
  finally { busy = false; }
}

$("settings-form").addEventListener("input", () => { dirty = true; });
$("settings-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await invoke("save_settings", { settings: {
      source: $("source").value.trim(),
      sources:Object.keys(sourceNames).map((kind)=>({kind,path:(kind==="claude"?$("source"):$("source-"+kind)).value.trim(),enabled:$("enabled-"+kind).checked})), threshold: Number($("threshold").value),
      zero_cache_only: $("zero-cache").checked, paused: $("paused").checked
    } });
    dirty = false; $("feedback").textContent = "Settings saved";
    window.refreshStatistics?.();
    await refresh();
  } catch (error) { showError(error); }
});
$("test").addEventListener("click", async () => {
  try { await invoke("test_notification"); $("feedback").textContent = "Alert submitted to Windows"; }
  catch (error) { showError(error); }
});
$("data").addEventListener("click", async () => {
  try { await invoke("open_data_folder"); } catch (error) { showError(error); }
});
let catalog = [];
const rateFields = ["input", "cache_5m", "cache_1h", "read", "output"];
async function loadCatalog() {
  catalog = await invoke("pricing_catalog");
  $("rate-models").replaceChildren(...catalog.filter((entry)=>entry.family===$("rate-family").value).map((entry) => {
    const option = document.createElement("option"); option.value = entry.model; return option;
  }));
}
function showModelRates() {
  const entry = catalog.find((item) => item.family === $("rate-family").value && item.model === $("rate-model").value.trim());
  for (const field of rateFields) $("rate-" + field).value = entry?.rates?.[field] ?? "";
  $("rate-feedback").textContent = entry?.custom ? "Custom effective rates" : entry?.rates ? "Published standard rates" : "Enter rates for this model";
}
$("pricing-panel").addEventListener("toggle", async () => {
  if (!$("pricing-panel").open) return;
  try { await loadCatalog(); showModelRates(); } catch (error) { showError(error); }
});
$("rate-model").addEventListener("change", showModelRates);
$("rate-family").addEventListener("change",async()=>{await loadCatalog();showModelRates();});
$("rates-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const rates = Object.fromEntries(rateFields.map((field) => [field, Number($("rate-" + field).value)]));
    await invoke("set_model_rates", { model: $("rate-model").value.trim(), family:$("rate-family").value, rates });
    await loadCatalog(); $("rate-feedback").textContent = "Custom rates saved; history recalculated"; window.refreshStatistics?.(); await refresh();
  } catch (error) { showError(error); }
});
$("reset-rates").addEventListener("click", async () => {
  try {
    await invoke("set_model_rates", { model: $("rate-model").value.trim(), family:$("rate-family").value, rates: null });
    await loadCatalog(); showModelRates(); window.refreshStatistics?.(); await refresh();
  } catch (error) { showError(error); }
});
function resetQuery() { page = 0; queryVersion++; refresh(); }
$("search").addEventListener("input", resetQuery);
$("alerts-only").addEventListener("change", resetQuery);
for (const id of ["source-filter","kind-filter"]) $(id).addEventListener("change",()=>{resetQuery();window.refreshStatistics?.();});
$("prev").addEventListener("click", () => { if (page > 0) page--; queryVersion++; refresh(); });
$("next").addEventListener("click", () => { page++; queryVersion++; refresh(); });
$("export").addEventListener("click", async () => {
  $("export").disabled = true;
  try {
    const path = await invoke("export_history_page", { search: $("search").value, alertsOnly: $("alerts-only").checked, page, filter:window.cacheWatchFilter() });
    $("export-result").textContent = `Saved to ${path}`;
    $("export-result").hidden = false;
  } catch (error) { showError(error); }
  finally { $("export").disabled = false; }
});
window.dashboardRefresh.subscribe(refresh);
