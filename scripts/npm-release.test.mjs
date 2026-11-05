import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { pack, platforms, publicationState, releaseMetadata, validateArchive, validateBinary, verifyPublishedIntegrity } from './npm-release.mjs';

function header(target) {
  const bytes = Buffer.alloc(64);
  if (target.startsWith('darwin')) {
    bytes.writeUInt32LE(0xfeedfacf, 0);
    bytes.writeUInt32LE(target.endsWith('arm64') ? 0x100000c : 0x1000007, 4);
  } else {
    bytes.writeUInt32BE(0x7f454c46);
    bytes[4] = 2;
    bytes[5] = 1;
    bytes.writeUInt16LE(target.endsWith('arm64') ? 183 : 62, 18);
  }
  return bytes;
}

test('release name and version agree with Cargo metadata', () => {
  assert.equal(releaseMetadata().name, 'session-finder');
});

test('native validation catches wrong architecture, format and truncated binaries', () => {
  for (const target of platforms) {
    assert.doesNotThrow(() => validateBinary(header(target), target));
    const other = target.replace(target.endsWith('arm64') ? 'arm64' : 'x64', target.endsWith('arm64') ? 'x64' : 'arm64');
    assert.throws(() => validateBinary(header(other), target), /wrong CPU/);
    assert.throws(() => validateBinary(Buffer.alloc(64), target));
    assert.throws(() => validateBinary(Buffer.alloc(4), target), /truncated/);
  }
  assert.throws(() => validateBinary(header('linux-x64'), 'windows-x64'));
});

test('full release cannot be packed without every platform', () => {
  const directory = mkdtempSync(join(tmpdir(), 'session-finder-test-'));
  try {
    assert.throws(() => pack({ binaries: directory, output: join(directory, 'out') }), /ENOENT/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('publish retries skip only byte-identical existing releases', () => {
  assert.equal(publicationState({ status: 0, stdout: '"sha512-example"' }, 'sha512-example'), 'published');
  assert.throws(() => publicationState({ status: 0, stdout: '"different"' }, 'sha512-example'), /different archive/);
  assert.equal(publicationState({ status: 1, stdout: '{"error":{"code":"E404"}}' }, 'unused'), 'missing');
  assert.throws(() => publicationState({ status: 1, stdout: '{"error":{"code":"E401"}}', stderr: 'unauthorized' }), /Registry lookup failed/);
  assert.throws(() => publicationState({ status: 1, stdout: '', stderr: 'network error' }), /network error/);
  assert.throws(() => publicationState({ error: new Error('timeout') }), /timeout/);
});

function verificationClock() {
  let elapsed = 0;
  const waits = [];
  return {
    waits,
    options: {
      timeoutMs: 25, intervalMs: 10, now: () => elapsed,
      wait: async ms => { waits.push(ms); elapsed += ms; },
      log: () => {},
    },
  };
}

test('publication verification tolerates npm processing before checking exact integrity', async () => {
  const clock = verificationClock();
  const responses = [
    { status: 1, stdout: '{"error":{"code":"E404"}}' },
    { status: 1, stdout: '{"error":{"code":"E404"}}' },
    { status: 0, stdout: '"sha512-example"' },
  ];
  const budgets = [];
  await verifyPublishedIntegrity(remaining => {
    budgets.push(remaining);
    return responses.shift();
  }, 'sha512-example', clock.options);
  assert.equal(responses.length, 0);
  assert.deepEqual(clock.waits, [10, 10]);
  assert.deepEqual(budgets, [25, 15, 5]);
});

test('publication verification checks immediately and does not delay an available version', async () => {
  const clock = verificationClock();
  await verifyPublishedIntegrity(() => ({ status: 0, stdout: '"sha512-example"' }), 'sha512-example', clock.options);
  assert.deepEqual(clock.waits, []);
});

test('publication verification stops at its deadline and explains accepted but unverified publication', async () => {
  const clock = verificationClock();
  let lookups = 0;
  await assert.rejects(verifyPublishedIntegrity(() => {
    lookups++;
    return { status: 1, stdout: '{"error":{"code":"E404"}}' };
  }, 'sha512-example', clock.options), /npm accepted publication.*timed out.*Do not republish/);
  assert.equal(lookups, 3);
  assert.deepEqual(clock.waits, [10, 10, 5]);
});

test('publication verification never retries integrity, authentication or network failures', async () => {
  for (const result of [
    { status: 0, stdout: '"different"' },
    { status: 1, stdout: '{"error":{"code":"E401"}}', stderr: 'unauthorized' },
    { status: 1, stdout: '{"error":{"code":"E403"}}', stderr: 'forbidden' },
    { status: 1, stdout: '', stderr: 'network failure' },
    { error: new Error('timeout') },
  ]) {
    const clock = verificationClock();
    let lookups = 0;
    await assert.rejects(verifyPublishedIntegrity(() => {
      lookups++;
      return result;
    }, 'sha512-example', clock.options));
    assert.equal(lookups, 1);
    assert.deepEqual(clock.waits, []);
  }
});

test('archive validation rejects a private smoke package and unexpected files', () => {
  const directory = mkdtempSync(join(tmpdir(), 'session-finder-archive-test-'));
  try {
    const host = `${process.platform}-${process.arch}`;
    const binaries = join(directory, 'native');
    mkdirSync(join(binaries, host), { recursive: true });
    writeFileSync(join(binaries, host, 'session-finder'), header(host));
    const archive = pack({ binaries, output: join(directory, 'out'), local: true });
    assert.throws(() => validateArchive(archive), /must never be published/);
    assert.doesNotThrow(() => validateArchive(archive, { local: true }));
    const extracted = join(directory, 'extract');
    mkdirSync(extracted);
    execFileSync('tar', ['-xzf', archive, '-C', extracted]);
    writeFileSync(join(extracted, 'package', '.env'), 'SYNTHETIC_TEST_ONLY=1\n');
    const modified = join(directory, 'modified.tgz');
    execFileSync('tar', ['-czf', modified, '-C', extracted, 'package']);
    assert.throws(() => validateArchive(modified, { local: true }), /unexpected files/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
