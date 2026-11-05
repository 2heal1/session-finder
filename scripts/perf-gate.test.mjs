import assert from 'node:assert/strict';
import test from 'node:test';
import { median, operationFailures, timingFailures } from './perf-gate.mjs';

const budget = { maxRegressionRatio: 1.25, minRegressionMs: 10, coldWallMs: 1500, warmWallMs: 750 };

test('performance gate rejects material slowdowns and tolerates small noise', () => {
  assert.deepEqual(timingFailures(125, 100, budget, 'cold'), []);
  assert.deepEqual(timingFailures(19, 5, { ...budget, minRegressionMs: 15 }, 'cold'), []);
  assert.match(timingFailures(126, 100, budget, 'cold').join(), /regression/);
  assert.match(timingFailures(751, undefined, budget, 'warm').join(), /absolute/);
  assert.match(timingFailures(NaN, 100, budget, 'cold').join(), /invalid/);
});

test('cold and warm reads catch cache loss and accidental full scans', () => {
  const scenario = { bodies: 10 };
  const cold = { totalFiles: 1200, parsedFiles: 10, headerFiles: 0, cachedFiles: 0 };
  const warm = { totalFiles: 1200, parsedFiles: 0, headerFiles: 0, cachedFiles: 10 };
  assert.deepEqual(operationFailures(cold, scenario, 'cold', 1200), []);
  assert.deepEqual(operationFailures(warm, scenario, 'warm', 1200), []);
  assert.match(operationFailures({ ...cold, parsedFiles: 1200 }, scenario, 'cold', 1200).join(), /parsedFiles/);
  assert.match(operationFailures(cold, scenario, 'warm', 1200).join(), /cachedFiles/);
  assert.match(operationFailures({ ...warm, headerFiles: 1 }, scenario, 'warm', 1200).join(), /headerFiles/);
});

test('relation headers and deliberately uncached show have explicit budgets', () => {
  assert.deepEqual(operationFailures(
    { totalFiles: 1200, parsedFiles: 0, headerFiles: 0, cachedFiles: 1200 },
    { bodies: 0, headers: 1200 }, 'warm', 1200), []);
  assert.deepEqual(operationFailures(
    { totalFiles: 1200, parsedFiles: 1, headerFiles: 0, cachedFiles: 0 },
    { bodies: 1, uncached: true }, 'warm', 1200), []);
});

test('median resists outliers and rejects missing or invalid measurements', () => {
  assert.equal(median([3, 500, 1, 2, 4]), 3);
  assert.equal(median([4, 1, 2, 3]), 2.5);
  assert.throws(() => median([]));
  assert.throws(() => median([NaN]));
});
