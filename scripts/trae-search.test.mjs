import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { discoverReader, normalize, score, searchIndex } from './trae-search.mjs';

const conversation = '6aeaf5e4b9f877dbdd7aa584';
const turn1 = '6aeaf5e4b9f877dbdd7aa587';
const turn2 = '6aebe87231a9cee6cd79604f';
const index = rows => rows.map(r => JSON.stringify(r)).join('\n');

test('TRAE search handles typos, transpositions, Chinese gaps and exact mode', () => {
  assert.equal(normalize('ＭＯＤＥＲＮ—项目'), 'modern 项目');
  assert.equal(score('modren fedration', ['modern federation']), 0.75);
  assert.equal(score('threadlin', ['threadline']), 1);
  assert.equal(score('thredline', ['threadline']), 0.75);
  assert.equal(score('路由导航', ['路由模块导航']), 0.7);
  assert.equal(score('modren', ['modern'], true), 0);
  assert.equal(score('modern absent', ['modern federation']), 0);
});

test('TRAE turns group into one conversation and select the matching question', () => {
  const result = searchIndex(index([
    { conversation_id: conversation, sessions: [{ session_id: turn1, user_query: 'a general question' }] },
    { conversation_id: conversation, sessions: [{ session_id: turn2, user_query: 'modern federation setup' }] },
  ]), 'modren federation');
  assert.equal(result.total, 1);
  assert.equal(result.indexedConversations, 1);
  assert.equal(result.indexedTurns, 2);
  assert.equal(result.sessions[0].provider, 'trae');
  assert.equal(result.sessions[0].id, conversation);
  assert.equal(result.sessions[0].matchedTurnId, turn2);
  assert.equal(result.sessions[0].historyPath, `context://history/${conversation}`);
});

test('TRAE search rejects malformed schemas and removes terminal controls', () => {
  assert.throws(() => searchIndex('broken', 'query'));
  assert.throws(() => searchIndex('{"id":"123"}', 'query'), /不兼容/);
  const result = searchIndex(index([{ conversation_id: conversation, sessions: [
    { session_id: turn1, user_query: 'needle\u001b[31m\u202emessage' },
  ] }]), 'needle');
  assert.ok(!/[\x1b\u202e]/.test(result.sessions[0].preview));
  assert.throws(() => discoverReader('/nonexistent/session-finder-test-reader'), /不存在/);
});

const script = fileURLToPath(new URL('./trae-search.mjs', import.meta.url));
// Deliberately construct the environment from scratch: the parent may be an Agent.
const cleanEnv = { HOME: homedir(), PATH: `${dirname(process.execPath)}:/usr/bin:/bin` };
const fakeAuth = {
  CFS_TOKEN: 'test-only-credential-do-not-persist',
  CFS_TRANSPORT: 'ipc',
  CFS_IPC_NAME: '/test-only-contextfs-endpoint',
};

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'session-finder-trae-test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const reader = join(directory, 'ctx-cli');
  const cacheDir = join(directory, 'cache');
  const rows = [{ conversation_id: conversation, sessions: [
    { session_id: turn1, user_query: 'threadline setup' },
  ] }];
  const readerProgram = `#!/usr/bin/env node
if (!process.env.CFS_TOKEN) { process.stderr.write("unexpected reader invocation"); process.exit(7); }
const row = ${JSON.stringify(rows[0])};
row.CFS_TOKEN = process.env.CFS_TOKEN;
row.sessions[0].transport = process.env.CFS_IPC_NAME;
process.stdout.write(JSON.stringify(row) + "\\n");
`;
  writeFileSync(reader, readerProgram, { mode: 0o700 });
  const invoke = (args = [], auth = false, extraEnv = {}) => spawnSync(process.execPath,
    [script, 'thredline', '--ctx', reader, '--cache-dir', cacheDir, ...args],
    { encoding: 'utf8', env: { ...cleanEnv, ...(auth ? fakeAuth : {}), ...extraEnv } });
  return { reader, cacheDir, invoke, rows };
}

test('ordinary terminal without an index gives a refresh instruction, never invokes ctx-cli', t => {
  const { invoke } = fixture(t);
  const result = invoke();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /没有本地索引/);
  assert.match(result.stderr, /TRAE Agent.*--refresh/);
  assert.doesNotMatch(result.stderr, /unexpected reader invocation/);
});

test('refresh saves a private question-only snapshot and clean terminal searches it', t => {
  const { reader, cacheDir, invoke, rows } = fixture(t);
  const live = invoke(['--refresh', '--json'], true);
  assert.equal(live.status, 0, live.stderr);
  const liveData = JSON.parse(live.stdout);
  assert.equal(liveData.indexSource, 'live');
  assert.equal(liveData.sessions[0].id, conversation);
  assert.equal(liveData.sessions[0].matchScore, 0.75);
  const files = readdirSync(cacheDir);
  assert.equal(files.length, 1);
  assert.match(files[0], /^[a-f0-9]{64}\.json$/);
  const cachePath = join(cacheDir, files[0]);
  const raw = readFileSync(cachePath, 'utf8');
  assert.ok(!raw.includes(fakeAuth.CFS_TOKEN));
  assert.ok(!raw.includes(fakeAuth.CFS_IPC_NAME));
  const snapshot = JSON.parse(raw);
  assert.deepEqual(Object.keys(snapshot).sort(), ['indexedAt', 'reader', 'rows', 'schemaVersion']);
  assert.equal(snapshot.reader, realpathSync(reader));
  assert.deepEqual(snapshot.rows, rows);
  if (process.platform !== 'win32') {
    assert.equal(statSync(cacheDir).mode & 0o777, 0o700);
    assert.equal(statSync(cachePath).mode & 0o777, 0o600);
  }
  // Make the snapshot visibly old and the reader unusable; cached search must still work.
  snapshot.indexedAt = '2020-01-02T03:04:05.000Z';
  writeFileSync(cachePath, JSON.stringify(snapshot));
  writeFileSync(reader, '#!/usr/bin/env node\nprocess.stderr.write("reader unavailable"); process.exit(9);\n');
  const cached = invoke(['--json']);
  assert.equal(cached.status, 0, cached.stderr);
  const cachedData = JSON.parse(cached.stdout);
  assert.equal(cachedData.indexSource, 'cache');
  assert.equal(cachedData.indexedAt, snapshot.indexedAt);
  assert.ok(cachedData.cacheAgeMs > 86_400_000);
  assert.deepEqual(cachedData.sessions, liveData.sessions);
  const human = invoke();
  assert.equal(human.status, 0, human.stderr);
  assert.match(human.stdout, /索引来源：本地缓存/);
  assert.ok(human.stdout.includes(snapshot.indexedAt));
  const forced = invoke(['--refresh']);
  assert.equal(forced.status, 1);
  assert.match(forced.stderr, /无法刷新.*没有 ContextFS 凭据/);
  assert.equal(forced.stdout, '');
  const failedLive = invoke(['--json'], true);
  assert.equal(failedLive.status, 1);
  assert.match(failedLive.stderr, /reader unavailable/);
  assert.equal(failedLive.stdout, '');
  assert.equal(JSON.parse(readFileSync(cachePath, 'utf8')).indexedAt, snapshot.indexedAt);
});

test('cached search rejects malformed JSON, schemas, metadata and turn records', t => {
  const { cacheDir, invoke } = fixture(t);
  const refreshed = invoke(['--refresh'], true);
  assert.equal(refreshed.status, 0, refreshed.stderr);
  const cachePath = join(cacheDir, readdirSync(cacheDir)[0]);
  const valid = JSON.parse(readFileSync(cachePath, 'utf8'));
  for (const bad of [
    '{broken',
    'null',
    JSON.stringify({ ...valid, schemaVersion: 999 }),
    JSON.stringify({ ...valid, reader: '/another/ctx-cli' }),
    JSON.stringify({ ...valid, indexedAt: 'not-a-date' }),
    JSON.stringify({ ...valid, rows: null }),
    JSON.stringify({ ...valid, rows: [null] }),
    JSON.stringify({ ...valid, rows: [{ conversation_id: conversation, sessions: [null] }] }),
  ]) {
    writeFileSync(cachePath, bad);
    const result = invoke(['--json']);
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /本地索引缓存无效/);
    assert.match(result.stderr, /--refresh/);
  }
});

test('live refresh rejects incomplete auth and mismatched apps without overwriting the cache', t => {
  const { reader, cacheDir, invoke } = fixture(t);
  const live = invoke(['--refresh'], true);
  assert.equal(live.status, 0, live.stderr);
  const cachePath = join(cacheDir, readdirSync(cacheDir)[0]);
  const before = readFileSync(cachePath, 'utf8');
  const incomplete = invoke(['--refresh'], true, { CFS_IPC_NAME: '' });
  assert.equal(incomplete.status, 1);
  assert.match(incomplete.stderr, /ContextFS 环境不完整/);
  const mismatched = invoke(['--refresh'], true, { ICUBE_APP_ROOT: '/different/trae/app' });
  assert.equal(mismatched.status, 1);
  assert.match(mismatched.stderr, /所属应用不一致/);
  writeFileSync(reader, '#!/usr/bin/env node\nprocess.stdout.write("{invalid");\n');
  const malformed = invoke(['--refresh'], true);
  assert.equal(malformed.status, 1);
  assert.equal(readFileSync(cachePath, 'utf8'), before);
  writeFileSync(reader, '#!/usr/bin/env node\nprocess.stderr.write(process.env.CFS_TOKEN); process.exit(1);\n');
  const failed = invoke(['--refresh'], true);
  assert.equal(failed.status, 1);
  assert.ok(!failed.stderr.includes(fakeAuth.CFS_TOKEN));
  assert.match(failed.stderr, /\[redacted\]/);
  assert.equal(readFileSync(cachePath, 'utf8'), before);
});

test('caches are isolated by reader path', t => {
  const { reader, cacheDir, invoke } = fixture(t);
  assert.equal(invoke(['--refresh'], true).status, 0);
  const otherReader = `${reader}-other`;
  writeFileSync(otherReader, '#!/usr/bin/env node\nprocess.exit(8);\n', { mode: 0o700 });
  const other = spawnSync(process.execPath,
    [script, 'thredline', '--ctx', otherReader, '--cache-dir', cacheDir, '--json'],
    { encoding: 'utf8', env: cleanEnv });
  assert.equal(other.status, 1);
  assert.match(other.stderr, /没有本地索引/);
});
