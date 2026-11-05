import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  assertNewerThanPublished, compareVersions, prepareRelease, pushRelease,
  registryVersions, releaseManifest, selectVersion, updateVersion,
} from './release-version.mjs';

// Git integration tests use disposable local repositories; no network writes.
Object.assign(process.env, {
  GIT_AUTHOR_NAME: 'Release fixture', GIT_AUTHOR_EMAIL: 'release@example.invalid',
  GIT_COMMITTER_NAME: 'Release fixture', GIT_COMMITTER_EMAIL: 'release@example.invalid',
});
const git = (directory, args) => execFileSync('git', args, {
  cwd: directory, encoding: 'utf8', stdio: 'pipe',
}).trim();
const files = ['package.json', 'Cargo.toml', 'Cargo.lock'];
const contents = directory => files.map(file => readFileSync(join(directory, file), 'utf8'));

function manifests(directory) {
  writeFileSync(join(directory, 'package.json'), JSON.stringify({
    name: 'session-finder', version: '0.1.0', private: true,
  }, null, 2) + '\n');
  writeFileSync(join(directory, 'Cargo.toml'), [
    '[package]', 'name = "session-finder"', 'version = "0.1.0"', 'publish = false', '',
    '[dependencies.other]', 'version = "0.1.0"', '',
  ].join('\n'));
  writeFileSync(join(directory, 'Cargo.lock'), [
    '# Keep dependency entries unchanged.', 'version = 4', '',
    '[[package]]', 'name = "other"', 'version = "0.1.0"', '',
    '[[package]]', 'name = "session-finder"', 'version = "0.1.0"',
    'dependencies = ["other"]', '',
  ].join('\n'));
}

function temporaryDirectory(t) {
  const directory = mkdtempSync(join(tmpdir(), 'session-finder-version-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function repository(t) {
  const directory = temporaryDirectory(t);
  const remote = join(directory, 'remote.git');
  const source = join(directory, 'source');
  const checkout = join(directory, 'checkout');
  mkdirSync(source);
  git(directory, ['init', '--bare', '--initial-branch=main', remote]);
  git(source, ['init', '--initial-branch=main']);
  manifests(source);
  git(source, ['add', '--', ...files]);
  git(source, ['commit', '-m', 'Initial fixture']);
  git(source, ['remote', 'add', 'origin', remote]);
  git(source, ['push', 'origin', 'main']);
  git(directory, ['clone', '--branch', 'main', remote, checkout]);
  git(checkout, ['checkout', '--detach', 'HEAD']);
  return {
    directory: checkout, remote, source, expectedSha: git(checkout, ['rev-parse', 'HEAD']),
    advanceMain() {
      writeFileSync(join(source, 'change.txt'), 'Concurrent change\n');
      git(source, ['add', 'change.txt']);
      git(source, ['commit', '-m', 'Concurrent fixture change']);
      git(source, ['push', 'origin', 'main']);
      return git(source, ['rev-parse', 'HEAD']);
    },
  };
}

test('first stable release uses the manifest and ignores bootstrap versions', () => {
  assert.equal(selectVersion({ current: '0.1.0', versions: ['0.0.0-bootstrap.0', '0.0.0-stage'] }), '0.1.0');
  assert.equal(selectVersion({ current: '0.1.0', tags: ['v9.0.0-rc.1', 'unrelated'] }), '0.1.0');
});

test('automatic version compares numbers and reserves both npm versions and tags', () => {
  assert.equal(compareVersions('0.1.10', '0.1.9'), 1);
  assert.equal(compareVersions('1.0.0', '0.99.99'), 1);
  assert.equal(selectVersion({ current: '0.1.0', versions: ['0.1.9'], tags: ['v0.1.10'] }), '0.1.11');
  assert.equal(selectVersion({ current: '0.1.0', versions: ['0.1.10'], tags: ['v0.1.9'] }), '0.1.11');
  assert.equal(selectVersion({ current: '0.2.0', versions: ['0.1.10'], tags: ['v0.1.11'] }), '0.2.0');
});

test('explicit versions allow minor/major releases and reject downgrades or reserved versions', () => {
  assert.equal(selectVersion({ current: '0.1.0', requested: ' v0.2.0 ', versions: ['0.1.0'] }), '0.2.0');
  assert.equal(selectVersion({ current: '0.1.0', requested: '1.0.0' }), '1.0.0');
  assert.equal(selectVersion({ current: '0.1.0', requested: '0.1.0' }), '0.1.0');
  assert.throws(() => selectVersion({ current: '0.2.0', requested: '0.1.1' }), /below repository/);
  assert.throws(() => selectVersion({ current: '0.1.0', requested: '0.1.0', versions: ['0.1.0'] }), /must be newer/);
  assert.throws(() => selectVersion({ current: '0.1.0', requested: '0.1.1', tags: ['v0.1.1'] }), /retry a tag/);
});

test('manual versions reject malformed, prerelease, leading-zero and unsafe inputs', () => {
  for (const requested of ['v', '1.2', '01.2.3', '1.2.3-rc.1', '1.2.3+build', '1.2.3\ninjected', '$(echo bad)', '9007199254740992.0.0']) {
    assert.throws(() => selectVersion({ current: '0.1.0', requested }));
  }
});

test('registry failures never silently become an empty release history', () => {
  assert.deepEqual(registryVersions({ status: 0, stdout: '["0.1.0","0.1.1"]' }), ['0.1.0', '0.1.1']);
  assert.deepEqual(registryVersions({ status: 0, stdout: '"0.1.0"' }), ['0.1.0']);
  assert.deepEqual(registryVersions({ status: 1, stdout: '{"error":{"code":"E404"}}' }), []);
  for (const code of ['E401', 'E403', 'E500']) {
    assert.throws(() => registryVersions({ status: 1, stdout: JSON.stringify({ error: { code } }) }), /lookup failed/);
  }
  assert.throws(() => registryVersions({ status: 1, stdout: '', stderr: 'network failure' }), /network failure/);
  assert.throws(() => registryVersions({ error: new Error('timeout') }), /timeout/);
  assert.throws(() => registryVersions({ status: 0, stdout: '{}' }), /invalid version list/);
  assert.throws(() => registryVersions({ status: 0, stdout: 'not JSON' }), /invalid version list/);
});

test('an older release finishing later cannot downgrade npm latest', () => {
  assert.doesNotThrow(() => assertNewerThanPublished('0.1.0', ['0.0.0-bootstrap.0']));
  assert.doesNotThrow(() => assertNewerThanPublished('0.1.10', ['0.1.9']));
  assert.throws(() => assertNewerThanPublished('0.1.9', ['0.1.10']), /refusing to move latest backwards/);
  assert.throws(() => assertNewerThanPublished('0.1.10', ['0.1.10']), /must be newer/);
});

test('all three versions change together without changing dependency versions', t => {
  const directory = temporaryDirectory(t);
  manifests(directory);
  const before = contents(directory);
  updateVersion(directory, '1.2.3');
  assert.equal(releaseManifest(directory).version, '1.2.3');
  const after = contents(directory);
  assert.equal(after[0], before[0].replace('"version": "0.1.0"', '"version": "1.2.3"'));
  assert.equal(after[1], before[1].replace('version = "0.1.0"', 'version = "1.2.3"'));
  assert.equal(after[2], before[2].replace('name = "session-finder"\nversion = "0.1.0"', 'name = "session-finder"\nversion = "1.2.3"'));
});

test('inconsistent manifests fail before writing any files', t => {
  const directory = temporaryDirectory(t);
  manifests(directory);
  const lock = join(directory, 'Cargo.lock');
  writeFileSync(lock, readFileSync(lock, 'utf8').replace('name = "session-finder"\nversion = "0.1.0"', 'name = "session-finder"\nversion = "0.0.9"'));
  const before = contents(directory);
  assert.throws(() => updateVersion(directory, '0.1.1'), /Cargo.lock and package.json versions differ/);
  assert.deepEqual(contents(directory), before);
});

test('first release tags the exact main commit without an empty version commit', t => {
  const fixture = repository(t);
  const release = prepareRelease({ ...fixture, versions: ['0.0.0-bootstrap.0'] });
  assert.deepEqual(release, { version: '0.1.0', tag: 'v0.1.0', sha: fixture.expectedSha });
  assert.equal(git(fixture.directory, ['status', '--porcelain']), '');
  pushRelease(fixture.directory, release);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/heads/main']), fixture.expectedSha);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/tags/v0.1.0']), fixture.expectedSha);
});

test('a reserved tag advances the patch, commits only version files, and pushes both refs', t => {
  const fixture = repository(t);
  git(fixture.source, ['tag', 'v0.1.0']);
  git(fixture.source, ['push', 'origin', 'refs/tags/v0.1.0']);
  const release = prepareRelease({ ...fixture, versions: [] });
  assert.equal(release.version, '0.1.1');
  assert.equal(git(fixture.directory, ['rev-parse', 'HEAD^']), fixture.expectedSha);
  assert.deepEqual(git(fixture.directory, ['diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD']).split('\n').sort(), [...files].sort());
  assert.equal(releaseManifest(fixture.directory).version, '0.1.1');
  pushRelease(fixture.directory, release);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/heads/main']), release.sha);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/tags/v0.1.1']), release.sha);
});

test('manual version is used in the commit, tag and all manifests', t => {
  const fixture = repository(t);
  const release = prepareRelease({ ...fixture, requested: 'v1.0.0', versions: ['0.1.0'] });
  assert.equal(release.version, '1.0.0');
  assert.equal(release.tag, 'v1.0.0');
  assert.equal(releaseManifest(fixture.directory).version, '1.0.0');
  assert.equal(git(fixture.directory, ['log', '-1', '--format=%s']), 'chore: release v1.0.0');
});

test('a stale dispatch stops before writing files or tagging', t => {
  const fixture = repository(t);
  fixture.advanceMain();
  const before = contents(fixture.directory);
  assert.throws(() => prepareRelease({ ...fixture, versions: [] }), /main has moved/);
  assert.deepEqual(contents(fixture.directory), before);
  assert.equal(git(fixture.directory, ['tag', '--list']), '');
});

test('a concurrent main update rejects the whole atomic push, including the tag', t => {
  const fixture = repository(t);
  const release = prepareRelease({ ...fixture, versions: ['0.1.0'] });
  const latest = fixture.advanceMain();
  assert.throws(() => pushRelease(fixture.directory, release), /atomic push failed|failed to push/);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/heads/main']), latest);
  assert.equal(git(fixture.remote, ['tag', '--list']), '');
});

test('a concurrent tag collision cannot partially update main', t => {
  const fixture = repository(t);
  const release = prepareRelease({ ...fixture, versions: ['0.1.0'] });
  git(fixture.source, ['tag', release.tag]);
  git(fixture.source, ['push', 'origin', `refs/tags/${release.tag}`]);
  assert.throws(() => pushRelease(fixture.directory, release), /atomic push failed|failed to push/);
  assert.equal(git(fixture.remote, ['rev-parse', 'refs/heads/main']), fixture.expectedSha);
  assert.equal(git(fixture.remote, ['rev-parse', `refs/tags/${release.tag}`]), fixture.expectedSha);
});

test('dirty checkouts are never included in release commits', t => {
  const fixture = repository(t);
  writeFileSync(join(fixture.directory, 'unrelated.txt'), 'Keep this work\n');
  assert.throws(() => prepareRelease({ ...fixture, versions: ['0.1.0'] }), /clean working tree/);
  assert.equal(git(fixture.directory, ['rev-parse', 'HEAD']), fixture.expectedSha);
  assert.equal(git(fixture.directory, ['tag', '--list']), '');
});
