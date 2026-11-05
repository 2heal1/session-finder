import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { zstdCompressSync } from 'node:zlib';

export const sessionId = i => `10000000-0000-4000-8000-${String(i).padStart(12, '0')}`;
const subagent = i => i > 0 && i % 17 === 0;

// All logs, paths and repository URLs are synthetic. Never inspect CODEX_HOME.
export function createFixture(home, { sessions, turns }) {
  mkdirSync(join(home, 'sessions'), { recursive: true });
  mkdirSync(join(home, 'archived_sessions'), { recursive: true });
  const db = new DatabaseSync(join(home, 'state_5.sqlite'));
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,created_at_ms INTEGER,
    cwd TEXT,source TEXT,git_origin_url TEXT,git_branch TEXT,title TEXT,thread_source TEXT);
    CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT PRIMARY KEY,status TEXT);
    BEGIN`);
  const row = db.prepare('INSERT INTO threads VALUES(?,?,?,?,?,?,?,?)');
  const edge = db.prepare('INSERT INTO thread_spawn_edges VALUES(?,?,?)');
  const index = [];
  let ordinary = 0, scoped = 0, edges = 0, compressed = 0, bytes = 0;
  for (let i = 0; i < sessions; i++) {
    const id = sessionId(i);
    const project = `project-${String(i % 24).padStart(2, '0')}`;
    const cwd = join(home, 'projects', project);
    const repository = `https://example.invalid/bench/${project}.git`;
    const created = Date.UTC(2026, 0, 1) + i * 60_000;
    const timestamp = new Date(created).toISOString();
    const source = subagent(i) ? { subagent: { thread_spawn: { parent_thread_id: sessionId(0) } } } : 'cli';
    const meta = { id, timestamp, cwd, source, git: { repository_url: repository, branch: 'main' } };
    if (i > 0 && i % 13 === 0) {
      meta.forked_from_id = sessionId(0);
      edges++;
    }
    if (subagent(i)) {
      edge.run(sessionId(0), id, 'closed');
      edges++;
    } else {
      ordinary++;
      if (i % 24 === 3) scoped++;
    }
    row.run(id, created, cwd, JSON.stringify(source), repository, 'main', `Synthetic session ${i}`, null);
    index.push(JSON.stringify({ id, thread_name: `Synthetic session ${i}`, updated_at: timestamp }));
    const records = [{ type: 'session_meta', timestamp, payload: meta }];
    for (let t = 0; t < turns; t++) {
      records.push({
        type: 'event_msg',
        timestamp: new Date(created + t * 1000).toISOString(),
        payload: {
          type: t % 2 ? 'agent_message' : 'user_message',
          message: `Session ${i}, turn ${t}: ${'deterministic conversation content '.repeat(16)}${t === 8 ? 'needle modern federation 路由导航' : ''}`,
        },
      });
    }
    const body = Buffer.from(records.map(v => JSON.stringify(v)).join('\n') + '\n');
    bytes += body.length;
    const zipped = i % 5 === 0;
    if (zipped) compressed++;
    const dir = i % 7 === 0 ? 'archived_sessions' : 'sessions';
    writeFileSync(join(home, dir, `rollout-${id}.jsonl${zipped ? '.zst' : ''}`), zipped ? zstdCompressSync(body) : body);
  }
  db.exec('COMMIT');
  db.close();
  writeFileSync(join(home, 'session_index.jsonl'), index.join('\n') + '\n');
  return { sessions, turns, ordinary, scoped, edges, compressed, bytes };
}

export function casesFor(fixture) {
  const { sessions, ordinary, scoped, edges, turns } = fixture;
  return [
    { name: 'created-page', args: ['recent', '--sort', 'created', '-n', '10'], bodies: 10, total: ordinary },
    { name: 'recent', args: ['recent'], bodies: ordinary, total: ordinary },
    { name: 'project', args: ['recent', '-p', 'project-03'], bodies: scoped, total: scoped },
    { name: 'search', args: ['search', 'needle', '-p', 'project-03'], bodies: scoped, total: scoped },
    { name: 'fuzzy-search', args: ['search', 'modren fedration', '-p', 'project-03'], bodies: scoped, total: scoped },
    { name: 'projects', args: ['projects'], bodies: ordinary, total: 24 },
    { name: 'show', args: ['show', sessionId(0)], bodies: 1, uncached: true, messages: turns },
    { name: 'relations', args: ['relations', sessionId(0), '--depth', '3'], bodies: 0, headers: sessions, edges },
  ];
}
