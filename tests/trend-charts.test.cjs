const { test } = require("node:test");
const assert = require("node:assert/strict");
require("../web/trends.js");
const { metrics, seriesFor, geometry } = globalThis.TrendCharts;

test("pricing gaps differ from empty time buckets and averages use priced calls", () => {
  const trend = { points: [
    { calls: 0, priced_calls: 0, estimated_usd: 0 },
    { calls: 2, priced_calls: 0, estimated_usd: 0 },
    { calls: 4, priced_calls: 2, estimated_usd: 0.02 },
  ] };
  assert.deepEqual(seriesFor(trend, "estimated_usd"), [0, null, 0.02]);
  assert.deepEqual(seriesFor(trend, "average_cost"), [null, null, 0.01]);
  assert.deepEqual(seriesFor(trend, "unpriced_calls"), [0, 2, 2]);
});

test("cache percentages are weighted by tokens and a zero denominator is undefined", () => {
  const trend = { points: [{ total_input: 100, cache_reads: 60 }, { total_input: 0, cache_reads: 0 }] };
  assert.deepEqual(seriesFor(trend, "cache_hit_ratio"), [60, null]);
  assert.equal(new Set(metrics.map((m) => m.id)).size, metrics.length);
});

test("chart paths break at unknown values and small dollar values use their full range", () => {
  const p = { left: 10, right: 10, top: 10, bottom: 10 };
  const shape = geometry([0, null, 0.02, 0.01], 100, 60, p);
  assert.equal(shape.max, 0.02);
  assert.equal((shape.path.match(/M/g) || []).length, 2);
  assert.equal((shape.path.match(/L/g) || []).length, 1);
  assert.equal(shape.y(0.02), 10);
  const sparse = geometry([null, 0.02, null, 0.03, null], 100, 60, p);
  assert.deepEqual(sparse.isolated, [1, 3]);
  assert.equal((sparse.path.match(/L/g) || []).length, 0);
  for (const values of [[], [null], [0], [5]]) {
    const shape = geometry(values, 100, 60, p);
    assert.ok(!/NaN|Infinity/.test(shape.path));
    if (values.length === 1) assert.equal(shape.x(0), 50);
  }
});
