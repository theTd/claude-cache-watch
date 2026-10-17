const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

const settle = () => new Promise(setImmediate);
function harness() {
  const requests = [], events = {}, polls = [], errors = [];
  const window = {
    __TAURI__: { core: { invoke(command) {
      assert.equal(command, "dashboard_active");
      return new Promise((resolve, reject) => requests.push({ resolve, reject }));
    } } },
    addEventListener(name, fn) { events[name] = fn; },
  };
  vm.runInNewContext(fs.readFileSync(require.resolve("../web/refresh-scheduler.js"), "utf8"), {
    window, document: { addEventListener(name, fn) { events[name] = fn; } },
    setInterval(fn) { polls.push(fn); }, console: { error(...args) { errors.push(args); } },
  });
  return { refresh: window.dashboardRefresh, requests, events, errors, poll: () => polls[0]() };
}

test("background startup and hidden polls do no dashboard work; restore refreshes all views", async () => {
  const h = harness(), updates = [];
  h.refresh.subscribe((resumed) => updates.push(resumed));
  h.requests[0].resolve(false); await settle();
  h.poll(); h.requests[1].resolve(false); await settle();
  assert.deepEqual(updates, []);
  assert.equal(h.refresh.active, false);
  h.events.focus(); h.requests[2].resolve(true); await settle();
  assert.deepEqual(updates, [true]);
  h.poll(); h.requests[3].resolve(true); await settle();
  assert.deepEqual(updates, [true, false]);
  h.events.visibilitychange(); h.requests[4].resolve(false); await settle();
  h.poll(); h.requests[5].resolve(true); await settle();
  assert.deepEqual(updates, [true, false, true]);
});

test("overlapping visibility checks coalesce and a failed check recovers", async () => {
  const h = harness(), updates = [];
  h.refresh.subscribe((resumed) => updates.push(resumed));
  h.poll(); h.events.focus();
  assert.equal(h.requests.length, 1);
  h.requests[0].reject(new Error("window unavailable")); await settle();
  assert.equal(h.errors.length, 1);
  assert.equal(h.requests.length, 2);
  assert.equal(h.refresh.active, false);
  h.requests[1].resolve(true); await settle();
  assert.deepEqual(updates, [true]);
});
