import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { assertNewerThanPublished, publishedVersions, registry, releaseManifest } from './release-version.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
export const platforms = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64'];
export { registry };
const host = () => `${process.platform}-${process.arch}`;
const capture = (command, args, options = {}) => execFileSync(command, args, {
  encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 120_000, ...options,
});

export function releaseMetadata() {
  return releaseManifest(root);
}

export function validateBinary(bytes, target) {
  assert.ok(platforms.includes(target), `unknown target ${target}`);
  assert.ok(bytes.length >= 64, `${target}: truncated executable`);
  if (target.startsWith('darwin-')) {
    assert.equal(bytes.readUInt32LE(0), 0xfeedfacf, `${target}: expected 64-bit Mach-O`);
    assert.equal(bytes.readUInt32LE(4), target.endsWith('arm64') ? 0x100000c : 0x1000007, `${target}: wrong CPU`);
  } else {
    assert.equal(bytes.subarray(0, 4).toString('hex'), '7f454c46', `${target}: expected ELF`);
    assert.equal(bytes[4], 2, `${target}: expected 64-bit ELF`);
    assert.equal(bytes[5], 1, `${target}: expected little endian ELF`);
    assert.equal(bytes.readUInt16LE(18), target.endsWith('arm64') ? 183 : 62, `${target}: wrong CPU`);
  }
}

export function validateArchive(archive, { local = false } = {}) {
  const metadata = releaseMetadata();
  const entries = capture('tar', ['-tzf', archive]).trim().split('\n');
  const read = name => capture('tar', ['-xOzf', archive, `package/${name}`], { encoding: null });
  assert.equal(entries.filter(e => e === 'package/package.json').length, 1, 'missing or duplicated manifest');
  const manifest = JSON.parse(read('package.json'));
  assert.equal(manifest.name, metadata.name);
  assert.equal(manifest.version, metadata.version);
  assert.equal(manifest.private, local, 'local smoke packages must never be published');
  assert.deepEqual(manifest.bin, { 'session-finder': 'bin/session-finder' });
  assert.deepEqual(manifest.os, ['darwin', 'linux']);
  assert.deepEqual(manifest.cpu, ['arm64', 'x64']);
  assert.deepEqual(manifest.repository, metadata.repository);
  for (const key of ['scripts', 'dependencies', 'optionalDependencies', 'devDependencies']) {
    assert.equal(manifest[key], undefined, `unexpected package ${key}`);
  }
  const targets = local ? [host()] : platforms;
  assert.deepEqual(manifest.sessionFinder.platforms, targets);
  const expected = [
    'package/package.json', 'package/README.md', 'package/bin/session-finder',
    ...targets.map(p => `package/native/${p}/session-finder`),
  ];
  assert.deepEqual(entries.sort(), expected.sort(), 'archive contains missing, duplicated or unexpected files');
  assert.equal(read('bin/session-finder').toString(), readFileSync(join(root, 'npm/session-finder'), 'utf8'));
  const modes = capture('tar', ['-tvzf', archive]).trim().split('\n');
  assert.equal(modes.length, entries.length);
  for (const name of entries) {
    const line = modes.find(line => line.endsWith(` ${name}`));
    assert.ok(line?.startsWith('-'), `${name}: archive entry must be a regular file`);
    if (name.endsWith('/session-finder')) assert.equal(line[3], 'x', `${name}: executable bit missing`);
  }
  for (const target of targets) validateBinary(read(`native/${target}/session-finder`), target);
  return manifest;
}

export function pack({ binaries, output, local = false }) {
  const metadata = releaseMetadata();
  const targets = local ? [host()] : platforms;
  assert.ok(platforms.includes(host()), `unsupported packaging host ${host()}`);
  const staging = mkdtempSync(join(tmpdir(), 'session-finder-pack-'));
  mkdirSync(output, { recursive: true });
  try {
    const manifest = {
      name: metadata.name, version: metadata.version, description: metadata.description,
      repository: metadata.repository, private: local,
      bin: { 'session-finder': 'bin/session-finder' },
      os: ['darwin', 'linux'], cpu: ['arm64', 'x64'],
      files: ['bin/', 'native/', 'README.md'],
      sessionFinder: { platforms: targets },
      publishConfig: { access: 'public', registry },
    };
    writeFileSync(join(staging, 'package.json'), JSON.stringify(manifest, null, 2) + '\n');
    copyFileSync(join(root, 'README.md'), join(staging, 'README.md'));
    mkdirSync(join(staging, 'bin'));
    copyFileSync(join(root, 'npm/session-finder'), join(staging, 'bin/session-finder'));
    chmodSync(join(staging, 'bin/session-finder'), 0o755);
    for (const target of targets) {
      const source = join(binaries, target, 'session-finder');
      validateBinary(readFileSync(source), target);
      const dest = join(staging, 'native', target);
      mkdirSync(dest, { recursive: true });
      copyFileSync(source, join(dest, 'session-finder'));
      chmodSync(join(dest, 'session-finder'), 0o755);
    }
    const packed = JSON.parse(capture('npm', [
      'pack', staging, '--json', '--ignore-scripts', '--pack-destination', output,
      '--cache', join(staging, 'npm-cache'),
    ]));
    const archive = join(output, packed[0].filename);
    validateArchive(archive, { local });
    const integrity = `sha512-${createHash('sha512').update(readFileSync(archive)).digest('base64')}`;
    writeFileSync(`${archive}.integrity`, integrity + '\n');
    return archive;
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

export function smoke(archive, { local = false } = {}) {
  const manifest = validateArchive(archive, { local });
  const directory = mkdtempSync(join(tmpdir(), 'session-finder smoke-'));
  try {
    const prefix = join(directory, 'install prefix');
    capture('npm', [
      'install', '--global', '--prefix', prefix, '--cache', join(directory, 'cache'),
      '--offline', '--ignore-scripts', '--no-audit', '--no-fund', archive,
    ]);
    const binary = join(prefix, 'bin/session-finder');
    assert.equal(capture(binary, ['--version']).trim(), `session-finder ${manifest.version}`);
    assert.match(capture(binary, ['--help']), /relations/);
    // A second relative symlink catches npm's global/local bin resolution pitfalls.
    const alias = join(directory, 'relative-link');
    symlinkSync('install prefix/bin/session-finder', alias);
    assert.equal(capture(alias, ['--version']).trim(), `session-finder ${manifest.version}`);
    const bad = spawnSync(binary, ['--invalid-option'], { encoding: 'utf8', timeout: 10_000 });
    assert.equal(bad.error, undefined);
    assert.equal(bad.status, 2, 'launcher must preserve CLI exit status');
    const home = join(directory, 'codex');
    mkdirSync(join(home, 'sessions'), { recursive: true });
    const id = '10000000-0000-4000-8000-000000000001';
    writeFileSync(join(home, 'sessions', `rollout-${id}.jsonl`),
      JSON.stringify({ type: 'session_meta', payload: { id, timestamp: '2026-01-01T00:00:00Z', cwd: home, source: 'cli' } }) + '\n');
    const result = spawnSync(binary, ['recent', '--provider', 'codex', '--json', '--timings', '--home', home, '--cache-dir', join(directory, 'query cache')], {
      encoding: 'utf8', timeout: 10_000,
    });
    assert.equal(result.status, 0, result.stderr);
    const data = JSON.parse(result.stdout);
    assert.equal(data.total, 1);
    assert.equal(data.sessions[0].id, id);
    assert.deepEqual(data.warnings, []);
    assert.match(result.stderr, /耗时 \d+ ms/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

export function publicationState(result, integrity) {
  if (result.error) throw result.error;
  if (result.status === 0) {
    assert.equal(JSON.parse(result.stdout), integrity, 'version exists with different archive; bump version');
    return 'published';
  }
  let response;
  try { response = JSON.parse(result.stdout); } catch { /* Report original registry failure below. */ }
  if (response?.error?.code === 'E404') return 'missing';
  throw new Error(`Registry lookup failed: ${result.stderr || result.stdout}`);
}

export async function verifyPublishedIntegrity(lookup, integrity, {
  timeoutMs = 600_000, intervalMs = 10_000, now = Date.now, wait = delay, log = console.log,
} = {}) {
  const deadline = now() + timeoutMs;
  while (now() < deadline) {
    const result = await lookup(deadline - now());
    if (publicationState(result, integrity) === 'published') return;
    const remaining = deadline - now();
    if (remaining <= 0) break;
    log(`npm accepted publication; waiting for registry availability (${Math.ceil(remaining / 1000)}s remaining)`);
    await wait(Math.min(intervalMs, remaining));
  }
  throw new Error('npm accepted publication, but registry verification timed out. Do not republish; verify the registry archive before rerunning the failed publish job.');
}

async function publish(archive, otp) {
  const metadata = validateArchive(archive);
  smoke(archive);
  const integrity = `sha512-${createHash('sha512').update(readFileSync(archive)).digest('base64')}`;
  const cache = mkdtempSync(join(tmpdir(), 'session-finder-publish-'));
  try {
    const lookup = (timeoutMs = 60_000) => spawnSync('npm', [
      'view', `${metadata.name}@${metadata.version}`, 'dist.integrity', '--json',
      '--registry', registry, '--cache', cache, '--prefer-online',
    ], { encoding: 'utf8', timeout: Math.max(1, Math.min(60_000, timeoutMs)) });
    if (publicationState(lookup(), integrity) === 'published') {
      console.log(`${metadata.name}@${metadata.version}: identical archive already published`);
      return;
    }
    assertNewerThanPublished(metadata.version, publishedVersions(metadata.name, cache));
    const args = ['publish', archive, '--access', 'public', '--tag', 'latest', '--registry', registry, '--cache', cache];
    if (otp) args.push('--otp', otp);
    try {
      execFileSync('npm', args, { stdio: 'inherit', timeout: 120_000 });
    } catch (error) {
      // Node's default child_process error includes spawnargs (and possibly OTP).
      throw new Error(`npm publish failed (exit ${error.status ?? 'unknown'}); see npm output above`);
    }
    // npm may acknowledge a publication before its version metadata is visible.
    // Retry only the read-only verification, never the publish command.
    await verifyPublishedIntegrity(lookup, integrity);
  } finally {
    rmSync(cache, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { values, positionals } = parseArgs({ allowPositionals: true, options: {
    binaries: { type: 'string' }, output: { type: 'string' }, archive: { type: 'string' },
    local: { type: 'boolean', default: false }, otp: { type: 'string' },
  } });
  const command = positionals[0];
  assert.equal(positionals.length, 1, 'expected pack, validate, smoke or publish');
  if (command === 'pack') {
    assert.ok(values.binaries && values.output, 'pack requires --binaries and --output');
    console.log(pack({ binaries: resolve(values.binaries), output: resolve(values.output), local: values.local }));
  } else {
    assert.ok(values.archive, `${command} requires --archive`);
    const archive = resolve(values.archive);
    if (command === 'validate') validateArchive(archive, values);
    else if (command === 'smoke') smoke(archive, values);
    else if (command === 'publish') {
      assert.equal(values.local, false, '--local cannot be published');
      await publish(archive, values.otp);
    } else throw new Error(`Unknown command: ${command}`);
    console.log(`${command}: OK`);
  }
}
