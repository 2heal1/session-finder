#!/usr/bin/env node
// Experimental TRAE IDE query-index search. Caches questions, never credentials.
import { spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const relativeReader = 'Contents/Resources/app/modules/ai-agent/bin/ctx-cli';
const ideBundleIds = new Set(['cn.trae.app', 'com.trae.app']);
const indexPath = 'context://history/conversations_metadata.jsonl';

function run(binary, args, timeout = 20_000, env = process.env) {
  const result = spawnSync(binary, args, { encoding: 'utf8', env, timeout, maxBuffer: 32 * 1024 * 1024 });
  if (result.error || result.status !== 0) {
    const message = `${binary}: ${result.error?.message || result.stderr.trim() || `exit ${result.status}`}`;
    throw new Error(env.CFS_TOKEN ? message.replaceAll(env.CFS_TOKEN, '[redacted]') : message);
  }
  return result.stdout;
}

function isIde(app) {
  if (!existsSync(join(app, relativeReader))) return false;
  try {
    return ideBundleIds.has(run('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', join(app, 'Contents/Info.plist')], 3000).trim());
  } catch {
    return false;
  }
}

export function discoverReader(explicit, env = process.env) {
  const override = explicit || env.SESSION_FINDER_TRAE_CTX;
  if (override) {
    const path = resolve(override);
    if (!existsSync(path)) throw new Error(`ctx-cli 不存在：${path}`);
    return path;
  }
  if (process.platform !== 'darwin') throw new Error('此试用脚本请通过 --ctx 指定 TRAE IDE 的 ctx-cli');
  // Runtime metadata survives app renaming and nonstandard installation paths.
  if (env.ICUBE_APP_ROOT) {
    const app = resolve(env.ICUBE_APP_ROOT, '../../..');
    if (isIde(app)) return join(app, relativeReader);
  }
  const apps = new Set();
  for (const directory of ['/Applications', join(homedir(), 'Applications')]) {
    if (!existsSync(directory)) continue;
    for (const entry of readdirSync(directory)) if (entry.endsWith('.app')) apps.add(join(directory, entry));
  }
  let candidates = [...apps].filter(isIde);
  // Standard-directory discovery is cheaper than starting a Spotlight query.
  if (!candidates.length) {
    try {
      const matches = run('/usr/bin/mdfind', ['kMDItemCFBundleIdentifier == "cn.trae.app" || kMDItemCFBundleIdentifier == "com.trae.app"'], 3000);
      candidates = [...new Set(matches.trim().split('\n').filter(Boolean))].filter(isIde);
    } catch { /* Spotlight may be disabled; the explicit override remains available. */ }
  }
  if (candidates.length === 1) return join(candidates[0], relativeReader);
  if (!candidates.length) throw new Error('未找到 TRAE IDE 的 ctx-cli；请使用 --ctx 指定');
  throw new Error(`找到多个 TRAE IDE，请用 --ctx 选择：\n${candidates.map(a => join(a, relativeReader)).join('\n')}`);
}

export const normalize = text => text.normalize('NFKC').toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ').trim();

function distance(a, b, limit) {
  if (Math.abs(a.length - b.length) > limit) return limit + 1;
  let older = Array(b.length + 1).fill(0);
  let previous = Array.from({ length: b.length + 1 }, (_, i) => i);
  let current = Array(b.length + 1).fill(0);
  for (let i = 1; i <= a.length; i++) {
    current[0] = i;
    for (let j = 1; j <= b.length; j++) {
      current[j] = Math.min(current[j - 1] + 1, previous[j] + 1, previous[j - 1] + Number(a[i - 1] !== b[j - 1]));
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
        current[j] = Math.min(current[j], older[j - 2] + 1);
      }
    }
    if (Math.min(...current) > limit) return limit + 1;
    [older, previous, current] = [previous, current, older];
  }
  return previous[b.length];
}

function nearby(term, field) {
  const needle = [...term], haystack = [...field];
  if (needle.length < 2 || needle.length > 64) return false;
  for (let start = 0; start < haystack.length; start++) {
    if (haystack[start] !== needle[0]) continue;
    let at = 1;
    for (let i = start + 1; i < Math.min(haystack.length, start + needle.length * 3); i++) {
      if (haystack[i] === needle[at] && ++at === needle.length) return true;
    }
  }
  return false;
}

export function score(query, fields, exact = false) {
  const terms = normalize(query).split(' ').filter(Boolean);
  if (!terms.length) return 0;
  const normalized = fields.map(normalize);
  let words, compact, total = 0;
  for (const term of terms) {
    if (normalized.some(f => f.includes(term))) { total++; continue; }
    compact ??= normalized.map(f => f.replaceAll(' ', ''));
    if (compact.some(f => f.includes(term))) { total += 0.95; continue; }
    if (exact) return 0;
    if (/^[a-z0-9]{4,48}$/.test(term)) {
      words ??= [...new Set(normalized.flatMap(f => f.match(/[a-z0-9]+/g) ?? []))];
      const limit = term.length >= 7 ? 2 : 1;
      if (words.some(w => distance(term, w, limit) <= limit)) { total += 0.75; continue; }
    }
    if (/\p{Script=Han}/u.test(term) && compact.some(f => nearby(term, f))) { total += 0.7; continue; }
    return 0;
  }
  return total / terms.length;
}

const printable = (text, limit = 180) => {
  const clean = text.replace(/[\x00-\x1f\x7f-\x9f\u202a-\u202e\u2066-\u2069]/g, ' ').replace(/\s+/g, ' ').trim();
  return [...clean].slice(0, limit).join('') + ([...clean].length > limit ? '…' : '');
};

function validateRows(rows) {
  if (!Array.isArray(rows)) throw new Error('TRAE 索引格式不兼容：缺少会话列表');
  return rows.map(row => {
    if (!row || typeof row.conversation_id !== 'string' || !/^[a-f0-9]{24}$/i.test(row.conversation_id) || !Array.isArray(row.sessions)) {
      throw new Error('TRAE 索引格式不兼容：缺少 conversation_id 或 sessions');
    }
    const sessions = row.sessions.map(turn => {
      if (!turn || typeof turn.user_query !== 'string' || typeof turn.session_id !== 'string' || !/^[a-f0-9]{24}$/i.test(turn.session_id)) {
        throw new Error('TRAE 索引格式不兼容：缺少轮次 ID 或用户提问');
      }
      return { session_id: turn.session_id, user_query: turn.user_query };
    });
    // Persist only the documented question-index fields, not arbitrary reader data.
    return { conversation_id: row.conversation_id, sessions };
  });
}

function parseIndex(text) {
  return validateRows(text.split('\n').filter(line => line.trim()).map(line => JSON.parse(line)));
}

const defaultCacheDir = () => join(homedir(), process.platform === 'darwin' ? 'Library/Caches' : '.cache', 'session-finder', 'trae-query-index');

export function loadQueryIndex(reader, { refresh = false, cacheDir = defaultCacheDir(), env = process.env } = {}) {
  reader = realpathSync(reader);
  cacheDir = resolve(cacheDir);
  const key = createHash('sha256').update(reader).digest('hex');
  const cachePath = join(cacheDir, `${key}.json`);
  const refreshHint = '请在 TRAE Agent 环境中运行同一命令并加 --refresh，之后普通终端可搜索缓存。';
  if (env.CFS_TOKEN) {
    if (env.CFS_TRANSPORT !== 'ipc' || !env.CFS_IPC_NAME) {
      throw new Error(`ContextFS 环境不完整：需要 TRAE Agent 注入的 IPC 配置。${refreshHint}`);
    }
    if (env.ICUBE_APP_ROOT) {
      const runtimeReader = join(env.ICUBE_APP_ROOT, 'modules/ai-agent/bin/ctx-cli');
      if (!existsSync(runtimeReader) || realpathSync(runtimeReader) !== reader) {
        throw new Error('指定的 ctx-cli 与当前 TRAE Agent 所属应用不一致，无法用当前凭据刷新它的索引。');
      }
    }
    const rows = parseIndex(run(reader, ['cat', indexPath], 20_000, env));
    const indexedAt = new Date().toISOString();
    const snapshot = { schemaVersion: 1, reader, indexedAt, rows };
    mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    chmodSync(cacheDir, 0o700);
    const temporary = join(cacheDir, `.${key}.${randomUUID()}.tmp`);
    try {
      writeFileSync(temporary, JSON.stringify(snapshot), { mode: 0o600, flag: 'wx' });
      renameSync(temporary, cachePath);
    } finally {
      rmSync(temporary, { force: true });
    }
    return { reader, rows, indexSource: 'live', indexedAt, cacheAgeMs: 0 };
  }
  if (refresh) throw new Error(`无法刷新：当前环境没有 ContextFS 凭据。${refreshHint}`);
  let snapshot;
  try {
    snapshot = JSON.parse(readFileSync(cachePath, 'utf8'));
    if (!snapshot || snapshot.schemaVersion !== 1 || snapshot.reader !== reader ||
        typeof snapshot.indexedAt !== 'string' || !Number.isFinite(Date.parse(snapshot.indexedAt))) {
      throw new Error('缓存版本或元数据不兼容');
    }
    snapshot.rows = validateRows(snapshot.rows);
  } catch (error) {
    if (error.code === 'ENOENT') throw new Error(`当前环境没有 ContextFS 凭据，也没有本地索引。${refreshHint}`);
    throw new Error(`本地索引缓存无效或无法读取。${refreshHint}`);
  }
  return {
    reader, rows: snapshot.rows, indexSource: 'cache', indexedAt: snapshot.indexedAt,
    cacheAgeMs: Math.max(0, Date.now() - Date.parse(snapshot.indexedAt)),
  };
}

function searchRows(rows, query, { exact = false, limit = 10 } = {}) {
  const conversations = new Map();
  let indexedTurns = 0;
  for (const row of rows) {
    const turns = conversations.get(row.conversation_id) ?? new Map();
    for (const turn of row.sessions) turns.set(turn.session_id, turn.user_query);
    conversations.set(row.conversation_id, turns);
  }
  const matches = [];
  for (const [id, turns] of conversations) {
    indexedTurns += turns.size;
    const fields = [id, ...turns.values()];
    const matchScore = score(query, fields, exact);
    if (!matchScore) continue;
    const ranked = [...turns].map(([turnId, text]) => ({ turnId, text, score: score(query, [text], exact) }))
      .sort((a, b) => b.score - a.score);
    const best = ranked[0]?.score > 0 ? ranked[0] : null;
    matches.push({
      provider: 'trae', id, matchScore,
      matchedTurnId: best?.turnId ?? null,
      preview: printable(best?.text ?? id),
      indexedTurns: turns.size,
      historyPath: `context://history/${id}`,
      matchedTurnPath: best ? `context://history/${id}/${best.turnId}.jsonl` : null,
    });
  }
  matches.sort((a, b) => b.matchScore - a.matchScore || a.id.localeCompare(b.id));
  return { indexedConversations: conversations.size, indexedTurns, total: matches.length, sessions: matches.slice(0, limit) };
}

export function searchIndex(text, query, options) {
  return searchRows(parseIndex(text), query, options);
}

export function main(args = process.argv.slice(2)) {
  const started = performance.now();
  const { values, positionals } = parseArgs({ args, allowPositionals: true, options: {
    ctx: { type: 'string' }, limit: { type: 'string', short: 'n', default: '10' },
    exact: { type: 'boolean' }, json: { type: 'boolean' }, help: { type: 'boolean', short: 'h' },
    refresh: { type: 'boolean' }, 'cache-dir': { type: 'string' },
  } });
  if (values.help) {
    console.log(`用法：node scripts/trae-search.mjs "关键词" [-n 10] [--exact] [--json]
  --ctx /path/to/ctx-cli  指定 TRAE IDE 的读取器
  --refresh              必须从当前 TRAE Agent 的 ContextFS 刷新，缺少凭据时报错
  --cache-dir /path      指定专用缓存目录（目录权限设为 0700，文件为 0600）
有 ContextFS 凭据时读取并缓存索引；普通终端搜索本地缓存，并显示更新时间。
默认缓存目录：${defaultCacheDir()}
缓存仅含用户提问和会话/轮次 ID，不保存 ContextFS 凭据。
只搜索索引中的用户提问；索引不保证覆盖全部历史，不搜索助手正文。`);
    return;
  }
  const query = positionals.join(' ').trim();
  if (!normalize(query)) throw new Error('请输入搜索关键词');
  const limit = Number(values.limit);
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new Error('-n 必须是 1–100');
  const reader = discoverReader(values.ctx);
  const readStarted = performance.now();
  const { rows, ...source } = loadQueryIndex(reader, { refresh: values.refresh, cacheDir: values['cache-dir'] });
  const readFinished = performance.now();
  const result = searchRows(rows, query, { exact: values.exact, limit });
  const finished = performance.now();
  const stats = {
    elapsedMs: Math.round(finished - started),
    discoveryMs: Math.round(readStarted - started),
    readMs: Math.round(readFinished - readStarted),
    matchMs: Math.round(finished - readFinished),
  };
  if (values.json) {
    console.log(JSON.stringify({ provider: 'trae', scope: 'indexed_user_queries', query, ...source, stats, ...result }, null, 2));
  } else {
    console.log(`TRAE IDE · 搜索索引中的用户提问 · 匹配 ${result.total} 个会话\n`);
    console.log(source.indexSource === 'cache'
      ? `索引来源：本地缓存 · 缓存更新时间：${source.indexedAt} · 距今 ${Math.floor(source.cacheAgeMs / 1000)} 秒\n`
      : `索引来源：ContextFS 实时读取 · 已更新缓存：${source.indexedAt}\n`);
    result.sessions.forEach((s, i) => console.log(`${i + 1}. ${s.id} · 匹配度 ${s.matchScore.toFixed(2)}\n   ${s.preview}\n   ${s.matchedTurnPath ?? s.historyPath}\n`));
    console.error(`耗时 ${stats.elapsedMs} ms · 定位 ${stats.discoveryMs} · 读取 ${stats.readMs} · 匹配 ${stats.matchMs} ms`);
    console.error(`索引范围：${result.indexedConversations} 个会话 / ${result.indexedTurns} 条提问；不保证覆盖全部历史。`);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(); } catch (error) { console.error(printable(error.message, 2000)); process.exitCode = 1; }
}
