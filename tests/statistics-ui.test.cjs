const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
require("../web/interval.js");

test("every nonzero recent position freezes concrete endpoints, including after backfill", () => {
  for (const historyAge of [86400000, 365 * 86400000]) {
    let now = Date.UTC(2026, 9, 16);
    const interval = new globalThis.StatisticsInterval(() => now);
    interval.earliest = now - historyAge;
    interval.select("recent", 1, 1000);
    assert.equal(interval.live, false);
    assert.ok(interval.positions().recent > 0);
    const fixed = interval.bounds();
    assert.equal(fixed.start, interval.earliest);
    now += 86400000; interval.earliest -= 86400000;
    assert.deepEqual(interval.bounds(), fixed);
    interval.select("recent", 0, 1000);
    assert.equal(interval.live, true);
    assert.equal(interval.bounds().end, now);
  }
});

test("live preset preserves its duration as time advances", () => {
  let now = Date.UTC(2026, 9, 16);
  const interval = new globalThis.StatisticsInterval(() => now);
  interval.preset(24);
  const first = interval.bounds(); now += 3000;
  const second = interval.bounds();
  assert.equal(second.start - first.start, 3000);
  assert.equal(second.end - second.start, 86400000);
});

test("moving one handle preserves the other endpoint when presets exceed available history", () => {
  const now = Date.UTC(2026, 9, 16);
  const interval = new globalThis.StatisticsInterval(() => now);
  interval.earliest = now - 3600000;
  interval.preset(24);
  const originalStart = interval.bounds().start;
  interval.select("recent", 500, interval.positions().older);
  assert.equal(interval.live, false);
  assert.equal(interval.bounds().start, originalStart);
  const originalEnd = interval.bounds().end;
  interval.select("older", interval.positions().recent, 900);
  assert.equal(interval.bounds().end, originalEnd);
});

function harness() {
  const elements = new Map(), timeouts = new Map(), polls = [], requests = [];
  let timerId = 0;
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, {
      textContent: "", value: id === "interval-older" ? 1000 : 0, handlers: {},
      setAttribute() {}, replaceChildren() {},
      addEventListener(name, handler) { this.handlers[name] = handler; },
    });
    return elements.get(id);
  };
  const context = vm.createContext({
    window: { __TAURI__: { core: { invoke: (command, args) => new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })) } } },
    document: { getElementById: element, querySelectorAll: () => [], createElement: () => ({ append() {} }) },
    Intl, Date, setInterval: (fn) => polls.push(fn),
    setTimeout: (fn) => { const id = ++timerId; timeouts.set(id, fn); return id; },
    clearTimeout: (id) => timeouts.delete(id),
  });
  for (const file of ["interval.js", "statistics.js"]) vm.runInContext(fs.readFileSync(path.join(__dirname, "../web", file), "utf8"), context);
  return {
    element, requests,
    poll() { polls[0](); },
    timers() { const pending = [...timeouts.values()]; timeouts.clear(); for (const fn of pending) fn(); },
    select(position) { const slider = element("interval-recent"); slider.value = position; slider.handlers.input({ target: slider }); },
  };
}

function stats(calls) {
  return { calls, priced_calls: 0, estimated_usd: 0, max_cost_usd: 0, total_input: 0, ordinary_input: 0,
    output: 0, uncached: 0, max_uncached: 0, cache_reads: 0, cache_read_discount_usd: 0, cache_writes: 0,
    cache_1h: 0, zero_cache_calls: 0, expensive_calls: 0, alerts_sent: 0, alerts_failed: 0, alerts_pending: 0,
    sessions: 0, subagents: 0, projects: 0, models: [], earliest_ms: Date.now() - 86400000, undated_calls: 0 };
}
const settle = () => new Promise(setImmediate);

test("polls during a slow calculation coalesce without discarding its success", async () => {
  const h = harness();
  for (let i = 0; i < 3; i++) { h.poll(); h.timers(); }
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve(stats(7)); await settle();
  assert.equal(h.element("stat-calls").textContent, "7");
  assert.equal(h.requests.length, 2);
});

test("obsolete errors cannot overwrite a newer interval and the replacement renders", async () => {
  const h = harness(); h.select(700); h.timers();
  h.requests[0].reject(new Error("obsolete failure")); await settle();
  assert.ok(!h.element("statistics-state").textContent.includes("obsolete"));
  assert.equal(h.requests.length, 2);
  h.requests[1].resolve(stats(4)); await settle();
  assert.equal(h.element("stat-calls").textContent, "4");
  assert.equal(h.element("interval-mode").textContent, "Fixed historical interval");
});

test("obsolete successful requests do not paint the wrong interval", async () => {
  const h = harness(); h.select(700); h.timers();
  h.requests[0].resolve(stats(99)); await settle();
  assert.notEqual(h.element("stat-calls").textContent, "99");
  h.requests[1].resolve(stats(3)); await settle();
  assert.equal(h.element("stat-calls").textContent, "3");
});
