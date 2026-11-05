//! TRAE IDE adapter: source snapshots -> shared Session/Message -> shared search cache.
use crate::{
    cache::{self, Cache, Loaded, Write as CacheWrite},
    cli::Options,
    contextfs::{self, Client, Reader},
    model::{Message, Parsed, Provider, Scope, Session, Stats},
    parser::summarize,
    text::clean_user,
    time::iso,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const SCHEMA: u32 = 1;
const TRANSCRIPT_SCHEMA: u32 = 2;
const MAX_FILE: u64 = 64 * 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Deserialize)]
pub struct Turn {
    pub session_id: String,
    pub user_query: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Row {
    pub conversation_id: String,
    pub sessions: Vec<Turn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// Normalized source metadata. Acquisition is separate from conversion: missing
/// fields remain unknown, and neither ObjectIDs nor index times supply dates.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub title: Option<String>,
    pub created_at: Option<String>,
    pub cwd: Option<String>,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: u32,
    reader: String,
    indexed_at: Option<String>,
    rows: Vec<Row>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transcript {
    schema_version: u32,
    conversation_id: String,
    index_signature: String,
    synced_at: String,
    messages: Vec<Message>,
}

fn valid_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn row_signature(row: &Row) -> Result<String> {
    Ok(hash(&serde_json::to_vec(row)?))
}

fn transcript_signature(row: &Row) -> Result<String> {
    // Preserve schema-2 transcript signatures from before metadata support.
    // A title rename must invalidate search fields without losing synced text.
    let turns = Row {
        conversation_id: row.conversation_id.clone(),
        sessions: row.sessions.clone(),
        metadata: None,
    };
    row_signature(&turns)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn read_text(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_FILE, "TRAE 快照超过大小限制");
    String::from_utf8(bytes).context("TRAE 快照不是 UTF-8 文本")
}

pub(crate) fn atomic_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("快照路径缺少父目录")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = parent.join(format!(
        ".snapshot-{}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, data)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn validate(rows: Vec<Row>) -> Result<Vec<Row>> {
    let mut out: Vec<Row> = Vec::new();
    let mut conversations = HashMap::new();
    let mut turns: HashMap<String, (usize, usize)> = HashMap::new();
    for mut row in rows {
        ensure!(
            valid_id(&row.conversation_id),
            "TRAE 索引 conversation_id 无效"
        );
        row.conversation_id.make_ascii_lowercase();
        if let Some(metadata) = &mut row.metadata {
            validate_time(&metadata.created_at)?;
            if let Some(time) = &mut metadata.created_at {
                *time = iso(&Value::String(time.clone())).context("TRAE 元数据时间无效")?;
            }
            for field in [
                &mut metadata.title,
                &mut metadata.cwd,
                &mut metadata.repository_url,
                &mut metadata.branch,
            ] {
                *field = field
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
        }
        let index = *conversations
            .entry(row.conversation_id.clone())
            .or_insert_with(|| {
                out.push(Row {
                    conversation_id: row.conversation_id.clone(),
                    sessions: Vec::new(),
                    metadata: None,
                });
                out.len() - 1
            });
        // The latest supplied metadata record wins, including an explicit
        // empty title. A query-only duplicate does not erase metadata.
        if row.metadata.is_some() {
            out[index].metadata = row.metadata;
        }
        for mut turn in row.sessions {
            ensure!(valid_id(&turn.session_id), "TRAE 索引轮次 ID 无效");
            turn.session_id.make_ascii_lowercase();
            // Never retain injected context in source snapshots.
            turn.user_query = user_text(&turn.user_query);
            if let Some(&(owner, position)) = turns.get(&turn.session_id) {
                ensure!(owner == index, "TRAE 轮次同时属于多个会话");
                out[index].sessions[position] = turn;
            } else {
                turns.insert(turn.session_id.clone(), (index, out[index].sessions.len()));
                out[index].sessions.push(turn);
            }
        }
    }
    // ContextFS can serialize its turn map in a different order on every read.
    // Canonicalize IDs for stable fingerprints/presentation, without inferring dates.
    for row in &mut out {
        row.sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    }
    Ok(out)
}

pub fn parse_index(text: &str) -> Result<Vec<Row>> {
    let rows = text
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|_| anyhow::anyhow!("TRAE 索引行格式无效")))
        .collect::<Result<Vec<_>>>()?;
    validate(rows)
}

/// TRAE supports string content, arrays of strings and typed text parts.
fn content(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    part.as_str().or_else(|| {
                        matches!(
                            part["type"].as_str(),
                            Some("text" | "input_text" | "output_text")
                        )
                        .then(|| part["text"].as_str())
                        .flatten()
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn approval_request(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with(
        "The following is the Trae agent history added since your last approval assessment.",
    ) || text.starts_with(
        "The following is the Trae agent history whose request action you are assessing.",
    )
}

pub(crate) fn user_text(text: &str) -> String {
    if approval_request(text) {
        return String::new();
    }
    let cleaned = clean_user(text);
    // IDE wraps the actual user input after skills/collaboration/attachment
    // instructions. Extract only when that known envelope is present; ordinary
    // user text that quotes a tag retains its content.
    if (cleaned.starts_with("<collaboration_mode>") || cleaned.starts_with("<skills_instructions>"))
        && let Some(body) = cleaned.strip_suffix("</user_input>")
        && let Some((_, input)) = body.split_once("<user_input>")
    {
        return clean_user(input);
    }
    if cleaned.starts_with("<collaboration_mode>") || cleaned.starts_with("<skills_instructions>") {
        return String::new();
    }
    cleaned
}

fn approval_result(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .is_ok_and(|v| matches!(v["outcome"].as_str(), Some("allow" | "deny" | "ask")))
}

pub fn convert_turn(text: &str, turn_id: &str) -> Result<Vec<Message>> {
    ensure!(valid_id(turn_id), "无效的 TRAE 轮次 ID");
    let mut out: Vec<Message> = Vec::new();
    let mut ids: HashMap<(String, String), usize> = HashMap::new();
    let mut pending_approvals = 0usize;
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .map_err(|_| anyhow::anyhow!("TRAE 正文第 {} 行无效；未更新快照", number + 1))?;
        ensure!(record.is_object(), "TRAE 正文行不是对象");
        let role = match record["role"].as_str() {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let text = content(&record["content"]);
        if role == "user" && approval_request(&text) {
            pending_approvals += 1;
            continue;
        }
        let text = if role == "user" {
            user_text(&text)
        } else {
            text.trim().to_owned()
        };
        if text.is_empty() {
            continue;
        }
        if role == "user" {
            pending_approvals = 0;
        } else if pending_approvals > 0 && approval_result(&text) {
            // Concurrent tool reviews can appear as request, request, reply, reply.
            pending_approvals -= 1;
            continue;
        }
        let message = Message {
            role: role.into(),
            text,
            timestamp: iso(&record["timestamp"]),
            phase: record["phase"].as_str().map(str::to_owned),
            id: record["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            turn_id: Some(turn_id.into()),
            format: "trae".into(),
            line: number + 1,
        };
        // Revisions with a stable message ID can replace earlier versions.
        // Equal text alone is insufficient evidence: repeated questions are legitimate.
        if let Some(id) = &message.id {
            let key = (message.role.clone(), id.clone());
            if let Some(&index) = ids.get(&key) {
                out[index] = message;
                continue;
            }
            ids.insert(key, out.len());
        }
        out.push(message);
    }
    Ok(out)
}

fn base(row: &Row) -> Session {
    let path = format!("context://history/{}", row.conversation_id);
    let metadata = row.metadata.clone().unwrap_or_default();
    Session {
        provider: Provider::Trae,
        scope: Scope::IndexedUserQueries,
        id: row.conversation_id.clone(),
        path: path.clone(),
        paths: vec![path.clone()],
        native_url: path,
        source: "trae_ide".into(),
        created_at: metadata.created_at,
        cwd: metadata.cwd,
        repository_url: metadata.repository_url,
        branch: metadata.branch,
        ..Default::default()
    }
}

fn title(session: &mut Session, row: &Row) {
    let (value, source) = if let Some(title) = row
        .metadata
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        (title, "metadata")
    } else if session.first_user_preview.is_empty() {
        (row.conversation_id.as_str(), "id")
    } else {
        (session.first_user_preview.as_str(), "first_user")
    };
    session.title = value.to_owned();
    session.title_source = Some(source.into());
}

pub fn convert_queries(row: &Row) -> Parsed {
    let messages: Vec<_> = row
        .sessions
        .iter()
        .filter_map(|turn| {
            let text = user_text(&turn.user_query);
            (!text.is_empty()).then(|| Message {
                role: "user".into(),
                text,
                timestamp: None,
                phase: None,
                id: None,
                turn_id: Some(turn.session_id.clone()),
                format: "trae_index".into(),
                line: 0,
            })
        })
        .collect();
    let mut parsed = summarize(base(row), messages, true, false);
    parsed.session.details_available = false;
    parsed.session.message_count = None;
    parsed.session.reply_count = None;
    title(&mut parsed.session, row);
    parsed
}

pub struct Store {
    native: Option<crate::trae_db::Store>,
    snapshot: Option<SnapshotStore>,
}

impl Store {
    pub fn open(options: &Options) -> Result<Self> {
        if options.trae_index.is_none()
            && options.trae_ctx.is_none()
            && let Some(native) = crate::trae_db::Store::discover(options)?
        {
            return Ok(Self {
                native: Some(native),
                snapshot: None,
            });
        }
        Ok(Self {
            native: None,
            snapshot: Some(SnapshotStore::open(options)?),
        })
    }

    pub fn records(
        &self,
        options: &Options,
        stats: &mut Stats,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Loaded>> {
        match &self.native {
            Some(store) => store.records(options, stats, warnings),
            None => self
                .snapshot
                .as_ref()
                .unwrap()
                .records(options, stats, warnings),
        }
    }

    pub fn show(&self, id: &str, refresh: bool) -> Result<Parsed> {
        match &self.native {
            Some(store) => store.show(id),
            None => self.snapshot.as_ref().unwrap().show(id, refresh),
        }
    }

    pub(crate) fn source_identity(&self) -> String {
        match &self.native {
            Some(store) => store.source_identity(),
            None => self.snapshot.as_ref().unwrap().identity.clone(),
        }
    }

    pub fn sync(&self, id: Option<&str>) -> Result<usize> {
        match &self.native {
            Some(store) => store.sync(id),
            None => self.snapshot.as_ref().unwrap().sync(id),
        }
    }
}

struct SnapshotStore {
    rows: Vec<Row>,
    root: PathBuf,
    identity: String,
    reader: Option<PathBuf>,
    indexed_at: Option<String>,
    source: String,
}

impl SnapshotStore {
    pub fn open(options: &Options) -> Result<Self> {
        let root = options.cache_dir.clone().unwrap_or_else(cache::directory);
        if let Some(path) = &options.trae_index {
            ensure!(
                !options.refresh,
                "--trae-index 是离线导出，不支持 --refresh"
            );
            let path = path.canonicalize().context("TRAE 离线索引不存在")?;
            let text = read_text(&path)?;
            let (rows, indexed_at) = if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&text) {
                ensure!(snapshot.schema_version == SCHEMA, "TRAE 索引快照版本不兼容");
                validate_time(&snapshot.indexed_at)?;
                (validate(snapshot.rows)?, snapshot.indexed_at)
            } else {
                (parse_index(&text)?, None)
            };
            return Ok(Self {
                rows,
                root,
                identity: format!("export:{}", path.display()),
                reader: None,
                indexed_at,
                source: "export".into(),
            });
        }
        let reader = contextfs::discover(options.trae_ctx.as_deref())?;
        let identity = reader.to_string_lossy().into_owned();
        let path = root
            .join("trae-query-index")
            .join(format!("{}.json", hash(identity.as_bytes())));
        let mut source = "cache";
        let snapshot = if options.refresh || !path.exists() {
            let client = Client::from_env(&reader)?;
            let rows = parse_index(&client.read(contextfs::INDEX)?)?;
            let snapshot = Snapshot {
                schema_version: SCHEMA,
                reader: identity.clone(),
                indexed_at: Some(now()),
                rows,
            };
            atomic_json(&path, &snapshot)?;
            source = "live";
            snapshot
        } else {
            let snapshot: Snapshot = serde_json::from_str(&read_text(&path)?).map_err(|_| {
                anyhow::anyhow!("TRAE 查询快照无效，请在 Agent 环境加 --refresh 重建")
            })?;
            ensure!(
                snapshot.schema_version == SCHEMA && snapshot.reader == identity,
                "TRAE 查询快照来源或版本不符，请加 --refresh"
            );
            validate_time(&snapshot.indexed_at)?;
            snapshot
        };
        Ok(Self {
            rows: validate(snapshot.rows)?,
            root,
            identity,
            reader: Some(reader),
            indexed_at: snapshot.indexed_at,
            source: source.into(),
        })
    }

    fn transcript_path(&self, row: &Row) -> PathBuf {
        self.root
            .join("trae-transcripts")
            .join(hash(self.identity.as_bytes()))
            .join(format!("{}.json", row.conversation_id))
    }

    fn transcript(&self, row: &Row) -> Result<Option<Transcript>> {
        let path = self.transcript_path(row);
        if !path.exists() {
            return Ok(None);
        }
        let saved: Transcript = serde_json::from_str(&read_text(&path)?)
            .map_err(|_| anyhow::anyhow!("TRAE 正文快照格式无效"))?;
        ensure!(
            saved.schema_version == TRANSCRIPT_SCHEMA
                && saved.conversation_id == row.conversation_id,
            "TRAE 正文快照版本或会话不符"
        );
        validate_time(&Some(saved.synced_at.clone()))?;
        if saved.index_signature != transcript_signature(row)? {
            return Ok(None);
        }
        let turns: HashSet<_> = row.sessions.iter().map(|t| t.session_id.as_str()).collect();
        ensure!(
            saved
                .messages
                .iter()
                .all(|m| matches!(m.role.as_str(), "user" | "assistant")
                    && m.turn_id.as_deref().is_some_and(|id| turns.contains(id))),
            "TRAE 正文快照包含无效消息"
        );
        Ok(Some(saved))
    }

    fn parsed(&self, row: &Row, transcript: Option<Transcript>, keep: bool) -> Parsed {
        let mut parsed = if let Some(transcript) = transcript {
            let mut session = base(row);
            session.scope = Scope::IndexedConversation;
            session.synced_at = Some(transcript.synced_at);
            let mut parsed = summarize(session, transcript.messages, true, keep);
            // The index can retain a question omitted from a turn snapshot.
            let queries = convert_queries(row);
            parsed.search_messages.extend(queries.search_messages);
            if let Some(queries) = queries.search_text {
                parsed.search_text.as_mut().unwrap().push(' ');
                parsed.search_text.as_mut().unwrap().push_str(&queries);
            }
            parsed
        } else {
            convert_queries(row)
        };
        title(&mut parsed.session, row);
        parsed.session.indexed_at.clone_from(&self.indexed_at);
        parsed.session.index_source = Some(self.source.clone());
        parsed
    }

    /// Re-read all indexed turns in the selected conversation: turn IDs alone do
    /// not indicate whether an assistant reply has finished streaming.
    fn sync_row(&self, row: &Row, reader: &impl Reader) -> Result<()> {
        let mut messages = Vec::new();
        for turn in &row.sessions {
            let path = format!(
                "context://history/{}/{}.jsonl",
                row.conversation_id, turn.session_id
            );
            messages.extend(convert_turn(&reader.read(&path)?, &turn.session_id)?);
        }
        let snapshot = Transcript {
            schema_version: TRANSCRIPT_SCHEMA,
            conversation_id: row.conversation_id.clone(),
            index_signature: transcript_signature(row)?,
            synced_at: now(),
            messages,
        };
        // Whole conversation atomicity: preserve previous snapshot on partial fetch failure.
        atomic_json(&self.transcript_path(row), &snapshot)
    }

    pub fn sync(&self, id: Option<&str>) -> Result<usize> {
        let rows: Vec<_> = self
            .rows
            .iter()
            .filter(|row| id.is_none_or(|prefix| row.conversation_id.starts_with(prefix)))
            .collect();
        ensure!(!rows.is_empty(), "没有找到可同步的 TRAE 会话");
        ensure!(
            id.is_none() || rows.len() == 1,
            "ID 前缀匹配多个 TRAE 会话，请使用完整 ID"
        );
        let reader = self
            .reader
            .as_ref()
            .context("离线导出不可实时同步，请移除 --trae-index 并选择 --trae-ctx")?;
        let client = Client::from_env(reader)?;
        for (i, row) in rows.iter().enumerate() {
            eprintln!(
                "同步 TRAE {}/{} · {} · {} 个轮次",
                i + 1,
                rows.len(),
                row.conversation_id,
                row.sessions.len()
            );
            self.sync_row(row, &client)?;
        }
        Ok(rows.len())
    }

    pub fn show(&self, id: &str, refresh: bool) -> Result<Parsed> {
        let row = self
            .rows
            .iter()
            .find(|r| r.conversation_id == id)
            .context("TRAE 会话不在索引内")?;
        let mut transcript = if refresh { None } else { self.transcript(row)? };
        if refresh || transcript.is_none() {
            self.sync(Some(id))?;
            transcript = self.transcript(row)?;
        }
        Ok(self.parsed(row, transcript, true))
    }

    pub fn records(
        &self,
        options: &Options,
        stats: &mut Stats,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Loaded>> {
        let namespace = PathBuf::from(format!("trae-adapter-v3:{}", self.identity));
        let mut cache = if options.no_cache {
            None
        } else {
            match Cache::open(&self.root, &namespace) {
                Ok(c) => Some(c),
                Err(_) => {
                    warnings.push("TRAE 派生索引不可用，直接转换本地快照".into());
                    None
                }
            }
        };
        let mut records = Vec::new();
        let mut writes = Vec::new();
        let mut paths = Vec::new();
        for row in &self.rows {
            let path = format!("context://history/{}", row.conversation_id);
            paths.push(PathBuf::from(&path));
            if options
                .id
                .as_ref()
                .is_some_and(|id| !row.conversation_id.starts_with(id))
            {
                continue;
            }
            let transcript_path = self.transcript_path(row);
            let signature = format!(
                "{}:{}:{}",
                row_signature(row)?,
                self.indexed_at.as_deref().unwrap_or(""),
                if transcript_path.exists() {
                    cache::fingerprint(&transcript_path, None)?
                } else {
                    String::new()
                }
            );
            let mut loaded = if !options.refresh {
                cache
                    .as_ref()
                    .and_then(|c| c.read(&path, &signature, true, true).ok().flatten())
            } else {
                None
            };
            if loaded.is_some() {
                stats.cached_files += 1;
            } else {
                let (transcript, cacheable) = match self.transcript(row) {
                    Ok(t) => (t, true),
                    Err(_) => {
                        warnings.push(format!(
                            "{} 正文快照无效，搜索仅覆盖索引提问；请重新 sync",
                            row.conversation_id
                        ));
                        (None, false)
                    }
                };
                let parsed = self.parsed(row, transcript, false);
                loaded = Some(Loaded {
                    session: parsed.session,
                    search_text: parsed.search_text,
                    search_messages: parsed.search_messages,
                });
                // A corrupt transcript is not cached as a successful full conversion.
                if cacheable {
                    writes.push(CacheWrite {
                        path: path.clone(),
                        signature,
                        loaded: loaded.clone().unwrap(),
                    });
                }
                stats.parsed_files += 1;
            }
            let mut loaded = loaded.unwrap();
            loaded.session.index_source = Some(self.source.clone());
            records.push(loaded);
        }
        if let Some(cache) = &mut cache
            && cache.save(&writes, &paths).is_err()
        {
            warnings.push("TRAE 派生索引写入失败，本次搜索结果仍可用".into());
        }
        stats.total_files += paths.len();
        Ok(records)
    }
}

fn validate_time(value: &Option<String>) -> Result<()> {
    if let Some(value) = value {
        ensure!(
            chrono::DateTime::parse_from_rfc3339(value).is_ok(),
            "TRAE 快照时间无效"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "010000000000400080000001";
    const TURN: &str = "010000000000400080000002";
    const TURN2: &str = "010000000000400080000003";

    struct Fake {
        fail: bool,
    }
    impl Reader for Fake {
        fn read(&self, path: &str) -> Result<String> {
            if self.fail && path.contains(TURN2) {
                anyhow::bail!("simulated failure");
            }
            Ok(concat!(
                "{\"role\":\"user\",\"content\":[\"question\"]}\n",
                "{\"role\":\"assistant\",\"content\":[\"unique-answer\"]}\n",
                "{\"role\":\"tool\",\"content\":[\"tool-secret\"]}\n"
            )
            .into())
        }
    }

    #[test]
    fn sync_atomicity_permissions_cache_invalidation_and_offline_show() {
        let dir = tempfile::tempdir().unwrap();
        let row = Row {
            conversation_id: ID.into(),
            metadata: None,
            sessions: [TURN, TURN2]
                .into_iter()
                .map(|id| Turn {
                    session_id: id.into(),
                    user_query: "question".into(),
                })
                .collect(),
        };
        let store = SnapshotStore {
            rows: vec![row.clone()],
            root: dir.path().into(),
            identity: "fixture".into(),
            reader: None,
            indexed_at: Some(now()),
            source: "cache".into(),
        };
        let options = Options::default();
        let mut stats = Stats::default();
        let mut warnings = Vec::new();
        let before = store.records(&options, &mut stats, &mut warnings).unwrap();
        assert_eq!(before[0].session.scope, Scope::IndexedUserQueries);
        store.sync_row(&row, &Fake { fail: false }).unwrap();
        let path = store.transcript_path(&row);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let bytes = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("tool-secret"));
        let after = store.records(&options, &mut stats, &mut warnings).unwrap();
        assert_eq!(after[0].session.scope, Scope::IndexedConversation);
        assert!(
            after[0]
                .search_text
                .as_ref()
                .unwrap()
                .contains("unique answer")
        );
        assert_eq!(after[0].session.message_count, Some(4)); // repeated legitimate text retained across turns
        let show = store.show(ID, false).unwrap(); // no credentials needed
        assert_eq!(show.messages.len(), 4);
        assert_eq!(show.messages[2].turn_id.as_deref(), Some(TURN2));
        assert!(store.sync_row(&row, &Fake { fail: true }).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mut changed = row.clone();
        changed.sessions.pop();
        assert!(store.transcript(&changed).unwrap().is_none());
        // Cache cleanup must not leave temporary files.
        assert!(
            fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|e| { e.unwrap().path().extension().is_some_and(|s| s == "json") })
        );
    }
}
