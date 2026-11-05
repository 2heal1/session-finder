export function median(values) {
  if (!values.length || values.some(v => !Number.isFinite(v) || v < 0)) {
    throw new Error('Invalid benchmark samples');
  }
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

export function timingFailures(candidate, baseline, budget, mode) {
  const failures = [];
  if (!Number.isFinite(candidate) || candidate < 0) return ['invalid candidate time'];
  if (candidate > budget[`${mode}WallMs`]) failures.push(`absolute budget exceeded: ${candidate.toFixed(1)} ms`);
  if (baseline !== undefined) {
    if (!Number.isFinite(baseline) || baseline < 0) return [...failures, 'invalid baseline time'];
    if (candidate > baseline * budget.maxRegressionRatio && candidate - baseline > budget.minRegressionMs) {
      failures.push(`regression: ${baseline.toFixed(1)} → ${candidate.toFixed(1)} ms`);
    }
  }
  return failures;
}

export function operationFailures(stats, scenario, mode, sessions) {
  const expected = {
    totalFiles: sessions,
    parsedFiles: mode === 'cold' || scenario.uncached ? scenario.bodies : 0,
    headerFiles: mode === 'cold' ? scenario.headers ?? 0 : 0,
    cachedFiles: mode === 'warm' && !scenario.uncached ? (scenario.headers ?? scenario.bodies) : 0,
  };
  return Object.entries(expected).flatMap(([key, value]) =>
    stats[key] === value ? [] : [`${key}: expected ${value}, got ${stats[key]}`]);
}
