import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir, platform, arch, cpus } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { createFixture, casesFor } from './bench-fixture.mjs';
import { median, operationFailures, timingFailures } from './perf-gate.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const { values } = parseArgs({ options: {
  binary: { type: 'string', default: join(root, 'session-finder') },
  baseline: { type: 'string' },
  output: { type: 'string', default: join(root, 'bench-results') },
  runs: { type: 'string' },
  'bootstrap-reason': { type: 'string' },
} });
const budget = JSON.parse(readFileSync(join(root, 'bench/budgets.json')));
const runs = Number(values.runs ?? budget.runs);
if (!Number.isInteger(runs) || runs < 3 || runs > 31) throw new Error('--runs must be 3–31');
const directory = mkdtempSync(join(tmpdir(), 'session-finder-bench-'));
const outputDir = resolve(values.output);
mkdirSync(outputDir, { recursive: true });
const binaries = { candidate: resolve(values.binary) };
if (values.baseline) binaries.baseline = resolve(values.baseline);
const helpByBinary = new Map(Object.values(binaries).map(binary => [
  binary, spawnSync(binary, ['--help'], { encoding: 'utf8' }).stdout ?? '',
]));
const report = {
  schemaVersion: 1,
  fixtureVersion: budget.fixtureVersion,
  environment: { platform: platform(), arch: arch(), cpu: cpus()[0]?.model, node: process.version },
  runs, budget, binaries,
  baselineReason: values.baseline ? null : values['bootstrap-reason'] ?? 'Not supplied; absolute and operation gates only',
  results: [], failures: [],
};

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') {
    // Older binaries omit these additive fields on Codex session summaries.
    // Fill defaults rather than removing fields, so a changed provider/scope still fails.
    if (typeof value.nativeUrl === 'string' && value.nativeUrl.startsWith('codex://threads/')) {
      value = { provider: 'codex', scope: 'local_conversation', ...value };
    }
    return Object.fromEntries(Object.keys(value).sort().map(k => [k, canonical(value[k])]));
  }
  return value;
}

function run(binary, home, cache, scenario) {
  // Keep full metadata in cross-version comparisons after list schema v2.
  const help = helpByBinary.get(binary);
  const verbose = ['recent', 'search'].includes(scenario.args[0]) && help.includes('--verbose')
    ? ['--verbose'] : [];
  // Older Codex-only binaries have no provider option.
  const provider = help.includes('--provider') ? ['--provider', 'codex'] : [];
  const start = performance.now();
  const processResult = spawnSync(binary, [...scenario.args, ...verbose, ...provider, '--home', home, '--cache-dir', cache, '--json'], {
    encoding: 'utf8', timeout: budget.timeoutMs, maxBuffer: 32 * 1024 * 1024,
  });
  const wallMs = performance.now() - start;
  if (processResult.error || processResult.status !== 0) {
    throw new Error(`${scenario.name}: ${processResult.error ?? processResult.stderr}`);
  }
  if (processResult.stderr.trim()) throw new Error(`${scenario.name}: unexpected stderr ${processResult.stderr}`);
  const data = JSON.parse(processResult.stdout);
  if (data.warnings.length) throw new Error(`${scenario.name}: ${data.warnings.join('; ')}`);
  if (scenario.total !== undefined && data.total !== scenario.total) throw new Error(`${scenario.name}: incorrect total ${data.total}`);
  if (scenario.messages !== undefined && data.messages?.length !== scenario.messages) throw new Error('show: missing messages');
  if (scenario.edges !== undefined && data.edges?.length !== scenario.edges) throw new Error('relations: missing edges');
  const { stats, ...result } = data;
  const comparison = data.schemaVersion === 2 && Array.isArray(data.sessions)
    ? { ...result, schemaVersion: 1, sessions: data.sessions.map(({ matches, ...session }) => session) }
    : result;
  if (!Number.isFinite(stats.elapsedMs) || stats.elapsedMs < 0) throw new Error('Missing query timing');
  return {
    wallMs, queryMs: stats.elapsedMs, stats,
    digest: createHash('sha256').update(JSON.stringify(canonical(result))).digest('hex'),
    comparisonDigest: createHash('sha256').update(JSON.stringify(canonical(comparison))).digest('hex'),
  };
}

try {
  const home = join(directory, 'codex');
  report.fixture = createFixture(home, budget);
  for (const scenario of casesFor(report.fixture)) {
    const samples = Object.fromEntries(Object.keys(binaries).map(name => [name, { cold: [], warm: [] }]));
    // Preheat executable/OS pages; "cold" below means an empty application cache.
    for (const [name, binary] of Object.entries(binaries)) run(binary, home, join(directory, `preheat-${name}-${scenario.name}`), scenario);
    for (let round = 0; round < runs; round++) {
      const order = Object.entries(binaries);
      if (round % 2) order.reverse();
      for (const [name, binary] of order) {
        const cache = join(directory, `${scenario.name}-${name}-${round}`);
        for (const mode of ['cold', 'warm']) {
          const sample = run(binary, home, cache, scenario);
          samples[name][mode].push(sample);
          if (name === 'candidate') {
            report.failures.push(...operationFailures(sample.stats, scenario, mode, budget.sessions)
              .map(message => `${scenario.name}/${mode}[${round}]: ${message}`));
          }
        }
      }
    }
    for (const mode of ['cold', 'warm']) {
      const candidate = samples.candidate[mode];
      const baseline = samples.baseline?.[mode];
      const wallMs = median(candidate.map(s => s.wallMs));
      const baselineMs = baseline && median(baseline.map(s => s.wallMs));
      const reference = samples.candidate.cold[0].digest;
      const comparisonReference = baseline?.[0].comparisonDigest ?? samples.candidate.cold[0].comparisonDigest;
      if (candidate.some(s => s.digest !== reference || s.comparisonDigest !== comparisonReference) ||
          baseline?.some(s => s.comparisonDigest !== comparisonReference)) {
        report.failures.push(`${scenario.name}/${mode}: result differs from baseline or cold run`);
      }
      report.failures.push(...timingFailures(wallMs, baselineMs, budget, mode).map(m => `${scenario.name}/${mode}: ${m}`));
      const row = {
        name: scenario.name, mode, wallMs, baselineMs,
        queryMs: median(candidate.map(s => s.queryMs)),
        stats: candidate[0].stats, samples: candidate,
        baselineSamples: baseline,
      };
      report.results.push(row);
      console.log(`${scenario.name.padEnd(14)} ${mode} ${wallMs.toFixed(1)} ms` +
        (baselineMs === undefined ? '' : ` (base ${baselineMs.toFixed(1)} ms)`));
    }
  }
} catch (error) {
  report.failures.push(error.stack ?? String(error));
} finally {
  rmSync(directory, { recursive: true, force: true });
}
report.passed = report.failures.length === 0;
const markdown = [
  '# session-finder performance',
  '',
  `Result: **${report.passed ? 'PASS' : 'FAIL'}** · ${runs} runs · ${budget.sessions} synthetic logs · ${platform()}/${arch()}`,
  '',
  report.baselineReason ?? 'Baseline and candidate alternate on the same runner.',
  '',
  '| Scenario | Cache | Base wall ms | Current wall ms | Query ms | Body / header / cache |',
  '| --- | --- | ---: | ---: | ---: | --- |',
  ...report.results.map(r => `| ${r.name} | ${r.mode} | ${r.baselineMs?.toFixed(1) ?? '—'} | ${r.wallMs.toFixed(1)} | ${r.queryMs} | ${r.stats.parsedFiles} / ${r.stats.headerFiles} / ${r.stats.cachedFiles} |`),
  '',
  ...report.failures.map(f => `- ${f}`), '',
].join('\n');
writeFileSync(join(outputDir, 'report.json'), JSON.stringify(report, null, 2) + '\n');
writeFileSync(join(outputDir, 'report.md'), markdown);
if (!report.passed) {
  console.error(report.failures.join('\n'));
  process.exitCode = 1;
}
