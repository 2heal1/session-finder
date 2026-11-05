import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
export const registry = 'https://registry.npmjs.org';
const versionFiles = ['package.json', 'Cargo.toml', 'Cargo.lock'];
const git = (directory, args, options = {}) => execFileSync('git', args, {
  cwd: directory, encoding: 'utf8', stdio: 'pipe', timeout: 60_000, ...options,
}).trim();

function versionParts(version) {
  assert.match(version, /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/, 'expected stable version X.Y.Z');
  const parts = version.split('.').map(Number);
  assert.ok(parts.every(Number.isSafeInteger), 'version components must be safe integers');
  return parts;
}

export function compareVersions(left, right) {
  const a = versionParts(left);
  const b = versionParts(right);
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return 0;
}

export function highestStableVersion(versions) {
  let highest;
  for (const version of versions) {
    if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) continue;
    versionParts(version);
    if (!highest || compareVersions(version, highest) > 0) highest = version;
  }
  return highest;
}

export function assertNewerThanPublished(version, versions) {
  versionParts(version);
  const latest = highestStableVersion(versions);
  assert.ok(!latest || compareVersions(version, latest) > 0,
    `Version ${version} must be newer than published version ${latest}; refusing to move latest backwards`);
}

export function selectVersion({ current, requested = '', versions = [], tags = [] }) {
  versionParts(current);
  const highest = highestStableVersion([...versions, ...tags.filter(tag => tag.startsWith('v')).map(tag => tag.slice(1))]);
  const explicit = requested.trim().replace(/^v/, '');
  if (requested.trim()) {
    versionParts(explicit);
    assert.ok(compareVersions(explicit, current) >= 0, `Version ${explicit} is below repository version ${current}`);
    assert.ok(!highest || compareVersions(explicit, highest) > 0,
      `Version ${explicit} must be newer than npm versions and release tags (highest: ${highest}). To retry a tag, rerun Release session-finder for that tag.`);
    return explicit;
  }
  if (!highest || compareVersions(current, highest) > 0) return current;
  const [major, minor, patch] = versionParts(highest);
  const next = `${major}.${minor}.${patch + 1}`;
  versionParts(next);
  return next;
}

export function registryVersions(result) {
  if (result.error) throw result.error;
  let response;
  try { response = JSON.parse(result.stdout); } catch { /* Keep the original failure below. */ }
  if (result.status !== 0) {
    if (response?.error?.code === 'E404') return [];
    throw new Error(`Registry version lookup failed: ${result.stderr || result.stdout}`);
  }
  const versions = typeof response === 'string' ? [response] : response;
  assert.ok(Array.isArray(versions) && versions.every(version => typeof version === 'string'),
    'Registry returned an invalid version list');
  return versions;
}

export function publishedVersions(name, cache) {
  const args = ['view', name, 'versions', '--json', '--registry', registry, '--prefer-online'];
  if (cache) args.push('--cache', cache);
  return registryVersions(spawnSync('npm', args, { encoding: 'utf8', timeout: 60_000 }));
}

function packageBlock(text, header) {
  const blocks = text.split(/(?=^\[)/m).filter(block =>
    header.test(block) && /^name\s*=\s*"session-finder"\s*$/m.test(block));
  assert.equal(blocks.length, 1, 'expected one session-finder package entry');
  return blocks[0];
}

function readVersionFiles(directory) {
  const metadata = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
  const cargo = readFileSync(join(directory, 'Cargo.toml'), 'utf8');
  const lock = readFileSync(join(directory, 'Cargo.lock'), 'utf8');
  const cargoPackage = packageBlock(cargo, /^\[package\]\s*\n/);
  const lockedPackage = packageBlock(lock, /^\[\[package\]\]\s*\n/);
  assert.equal(metadata.name, 'session-finder', 'unexpected npm package name');
  versionParts(metadata.version);
  for (const [file, block] of [['Cargo.toml', cargoPackage], ['Cargo.lock', lockedPackage]]) {
    assert.equal(block.match(/^version\s*=\s*"([^"]+)"/m)?.[1], metadata.version,
      `${file} and package.json versions differ`);
  }
  return { metadata, cargo, lock, cargoPackage, lockedPackage };
}

export function releaseManifest(directory = root) {
  return readVersionFiles(directory).metadata;
}

export function updateVersion(directory, version) {
  versionParts(version);
  const { metadata, cargo, lock, cargoPackage, lockedPackage } = readVersionFiles(directory);
  const replace = block => block.replace(/^(version\s*=\s*")[^"]+(")/m, (_, prefix, suffix) => `${prefix}${version}${suffix}`);
  metadata.version = version;
  writeFileSync(join(directory, 'package.json'), JSON.stringify(metadata, null, 2) + '\n');
  writeFileSync(join(directory, 'Cargo.toml'), cargo.replace(cargoPackage, replace(cargoPackage)));
  writeFileSync(join(directory, 'Cargo.lock'), lock.replace(lockedPackage, replace(lockedPackage)));
  assert.equal(releaseManifest(directory).version, version);
}

export function prepareRelease({ directory = root, expectedSha, requested = '', versions }) {
  assert.ok(expectedSha, 'expected dispatch commit SHA');
  assert.equal(git(directory, ['status', '--porcelain']), '', 'release requires a clean working tree');
  git(directory, ['fetch', 'origin', 'refs/heads/main:refs/remotes/origin/main', '--tags']);
  assert.equal(git(directory, ['rev-parse', 'HEAD']), expectedSha, 'checkout differs from the dispatch commit');
  assert.equal(git(directory, ['rev-parse', 'refs/remotes/origin/main']), expectedSha,
    'main has moved; run Publish npm release again from main');
  const { version: current } = releaseManifest(directory);
  const tags = git(directory, ['tag', '--list', 'v*']).split('\n').filter(Boolean);
  const version = selectVersion({ current, requested, versions, tags });
  if (version !== current) {
    updateVersion(directory, version);
    git(directory, ['add', '--', ...versionFiles]);
    git(directory, ['commit', '--file=-'], { input: `chore: release v${version}\n` });
  }
  const tag = `v${version}`;
  git(directory, ['tag', tag, 'HEAD']);
  return { version, tag, sha: git(directory, ['rev-parse', 'HEAD']) };
}

export function pushRelease(directory, { tag, sha }) {
  assert.equal(git(directory, ['rev-parse', 'HEAD']), sha, 'release commit has changed');
  assert.equal(git(directory, ['rev-parse', `refs/tags/${tag}`]), sha, 'release tag has changed');
  // A concurrent main update or tag collision rejects BOTH refs, without a force push.
  git(directory, ['push', '--atomic', 'origin', 'HEAD:refs/heads/main', `refs/tags/${tag}:refs/tags/${tag}`]);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({ options: {
    version: { type: 'string', default: process.env.REQUESTED_VERSION || '' },
    prepare: { type: 'boolean', default: false },
  } });
  if (values.prepare) {
    assert.equal(process.env.GITHUB_ACTIONS, 'true', '--prepare is only for the GitHub Actions release button');
    assert.equal(process.env.GITHUB_REF, 'refs/heads/main', 'Select main to publish a new release');
  }
  const metadata = releaseManifest();
  const versions = publishedVersions(metadata.name);
  if (values.prepare) {
    const release = prepareRelease({ expectedSha: process.env.GITHUB_SHA, requested: values.version, versions });
    pushRelease(root, release);
    appendFileSync(process.env.GITHUB_OUTPUT, `version=${release.version}\ntag=${release.tag}\nsha=${release.sha}\n`);
    console.log(`Prepared ${release.tag} at ${release.sha}`);
  } else {
    const tags = git(root, ['ls-remote', '--tags', '--refs', 'origin', 'v*'])
      .split('\n').filter(Boolean).map(line => line.split('\t')[1].replace('refs/tags/', ''));
    console.log(selectVersion({ current: metadata.version, requested: values.version, versions, tags }));
  }
}
