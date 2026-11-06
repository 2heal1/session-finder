//! Read-only TRAE IDE database adapter. Credentials are derived in memory from
//! a verified installed native library and are never part of cache identities.
use crate::{
    cache::{self, Cache, Loaded, Write},
    cli::Options,
    model::{Message, Parsed, Provider, Scope, Session, Stats},
    parser::summarize,
    time::{iso, measure, millis_iso},
    trae::user_text,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const ADAPTER: &str = "trae-native-v1";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
mod cipher {
    use super::*;
    use std::io::{Seek, SeekFrom};
    use zeroize::Zeroizing;

    // LC_UUID of the verified arm64 library shipped in TRAE CN 3.4.15.
    // Offsets are used only after this exact build guard succeeds.
    const UUID: [u8; 16] = [
        0xb4, 0x4c, 0x0b, 0xfc, 0x90, 0xb7, 0x35, 0x66, 0x84, 0x72, 0x82, 0x7d, 0x16, 0x05, 0x71,
        0xa2,
    ];

    fn read_at<const N: usize>(file: &mut fs::File, offset: u64) -> Result<Zeroizing<[u8; N]>> {
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Zeroizing::new([0; N]);
        file.read_exact(bytes.as_mut())?;
        Ok(bytes)
    }

    pub fn key(library: &Path) -> Result<Zeroizing<Vec<u8>>> {
        let mut file = fs::File::open(library).context("Cannot read TRAE native library")?;
        let header = read_at::<32>(&mut file, 0)?;
        ensure!(
            header[..4] == [0xcf, 0xfa, 0xed, 0xfe],
            "Unsupported TRAE native library format"
        );
        let count = u32::from_le_bytes(header[16..20].try_into()?);
        ensure!(count < 1024, "Invalid TRAE native library header");
        let mut offset = 32;
        let mut supported = false;
        for _ in 0..count {
            let command = read_at::<8>(&mut file, offset)?;
            let kind = u32::from_le_bytes(command[..4].try_into()?);
            let size = u32::from_le_bytes(command[4..].try_into()?) as u64;
            ensure!(
                (8..=65536).contains(&size),
                "Invalid TRAE native library load command"
            );
            if kind == 0x1b && size == 24 {
                supported = *read_at::<16>(&mut file, offset + 8)? == UUID;
            }
            offset += size;
        }
        ensure!(
            supported,
            "Unsupported TRAE native library version; TRAE CN 3.4.15 arm64 is verified. Use --trae-index or --trae-ctx explicitly"
        );
        let mut input = read_at::<32>(&mut file, 0x6b29073)?;
        for (offset, size) in [(0x6b29020, 4), (0x6b29030, 3), (0x6cbe038, 8)] {
            let mask = read_at::<8>(&mut file, offset)?;
            for (index, byte) in input.iter_mut().enumerate() {
                *byte ^= mask[index % size];
            }
        }
        let salt = read_at::<16>(&mut file, 0x99c711e)?;
        let mut material = Zeroizing::new([0u8; 32]);
        pbkdf2::pbkdf2_hmac::<Sha256>(input.as_ref(), salt.as_ref(), 100_000, material.as_mut());
        let mut key = Zeroizing::new(Vec::with_capacity(67));
        key.extend_from_slice(b"x'");
        const HEX: &[u8] = b"0123456789abcdef";
        for byte in material.iter() {
            key.push(HEX[(byte >> 4) as usize]);
            key.push(HEX[(byte & 15) as usize]);
        }
        key.push(b'\'');
        Ok(key)
    }

    pub fn unlock(conn: &Connection, key: &[u8]) -> Result<()> {
        // sqlite3_key copies the buffer; Zeroizing erases our temporary on drop.
        let status = unsafe {
            rusqlite::ffi::sqlite3_key(conn.handle(), key.as_ptr().cast(), key.len() as i32)
        };
        ensure!(
            status == rusqlite::ffi::SQLITE_OK,
            "TRAE database key initialization failed"
        );
        Ok(())
    }
}

fn encrypted(path: &Path) -> Result<bool> {
    let mut header = [0u8; 16];
    fs::File::open(path)?.read_exact(&mut header)?;
    Ok(&header != b"SQLite format 3\0")
}

fn open_readonly(path: &Path, library: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(2))?;
    if encrypted(path)? {
        #[cfg(target_os = "macos")]
        cipher::unlock(&conn, &cipher::key(library)?)?;
        #[cfg(not(target_os = "macos"))]
        {
            let _ = library;
            bail!("Encrypted TRAE databases currently require macOS; use --trae-index");
        }
    }
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .context("Cannot read TRAE database; the native library and database may not match")?;
    // A transaction gives metadata and bodies a consistent view including WAL.
    // Do not use immutable=1 (it would ignore live WAL records).
    conn.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED;")?;
    Ok(conn)
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    session: Session,
    signature: String,
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    stamp: String,
    entries: Vec<Entry>,
}

pub struct Store {
    conn: Connection,
    entries: Vec<Entry>,
    root: PathBuf,
    identity: PathBuf,
}

impl Store {
    pub fn discover(options: &Options) -> Result<Option<Self>> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let variants = ["Trae CN", "Trae"];
        let (path, app) = if let Some(path) = &options.trae_db {
            (path.clone(), "Trae CN")
        } else if let Some((path, app)) = variants.iter().find_map(|app| {
            let path = home
                .join("Library/Application Support")
                .join(app)
                .join("ModularData/ai-agent/database.db");
            path.is_file().then_some((path, *app))
        }) {
            (path, app)
        } else {
            ensure!(
                options.trae_library.is_none(),
                "TRAE database not found; specify --trae-db"
            );
            return Ok(None);
        };
        let library = options.trae_library.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "/Applications/{app}.app/Contents/Resources/app/modules/ai-agent/libai_agent.dylib"
            ))
        });
        let path = path
            .canonicalize()
            .context("TRAE database does not exist")?;
        let conn = open_readonly(&path, &library)?;
        Ok(Some(Self::from_connection(conn, &path, options)?))
    }

    fn from_connection(conn: Connection, path: &Path, options: &Options) -> Result<Self> {
        let root = options.cache_dir.clone().unwrap_or_else(cache::directory);
        let identity = PathBuf::from(format!("{ADAPTER}:{}", path.display()));
        let stamp = source_stamp(path)?;
        // Pin the read transaction before consulting a filesystem-stamped
        // catalog. A concurrent WAL write disables reuse/publication this time.
        conn.query_row("SELECT count(*) FROM chat_session", [], |_| Ok(()))?;
        let stable = source_stamp(path)? == stamp;
        let catalog_path = root.join("trae-native-catalog").join(format!(
            "{}.json",
            hash(identity.to_string_lossy().as_bytes())
        ));
        if stable
            && !options.no_cache
            && !options.refresh
            && let Ok(file) = fs::File::open(&catalog_path)
            && let Ok(catalog) = serde_json::from_reader::<_, Catalog>(file.take(16 * 1024 * 1024))
            && catalog.stamp == stamp
        {
            return Ok(Self {
                conn,
                entries: catalog.entries,
                root,
                identity,
            });
        }
        // These small columns can be read without materializing tool output blobs.
        let mut revisions: HashMap<String, (Sha256, i64)> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT m.session_id,m.message_id,m.message_index,m.message_role,m.message_type,
                 m.created_at,m.updated_at,m.deleted_at,m.is_archived,
                 g.updated_at,g.deleted_at,t.updated_at,t.deleted_at,c.updated_at,c.deleted_at
                 FROM chat_message m
                 LEFT JOIN chat_message_general g ON g.message_id=m.message_id
                 LEFT JOIN chat_message_task t ON t.message_id=m.message_id
                 LEFT JOIN chat_message_chat c ON c.message_id=m.message_id
                 ORDER BY m.session_id,m.message_index,m.message_id"
            ).context("Unsupported TRAE database message schema")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let entry = revisions.entry(id).or_insert_with(|| (Sha256::new(), 0));
                for i in 1..15 {
                    let value = row.get_ref(i)?;
                    match value {
                        rusqlite::types::ValueRef::Text(s) => entry.0.update(s),
                        rusqlite::types::ValueRef::Integer(n) => {
                            entry.0.update(n.to_le_bytes());
                            if [6, 9, 11, 13].contains(&i) {
                                entry.1 = entry.1.max(n);
                            }
                        }
                        _ => entry.0.update([0]),
                    }
                    entry.0.update([0xff]);
                }
            }
        }
        let mut entries = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT s.session_id,s.session_title,s.created_at,s.updated_at,s.session_type,
                 COALESCE(NULLIF(w.worktree_path,''),NULLIF(p.absolute_path,'')),w.worktree_branch
                 FROM chat_session s
                 LEFT JOIN project p ON p.project_id=COALESCE(NULLIF(s.project_id,''),
                     (SELECT sp.project_id FROM session_project sp WHERE sp.session_id=s.session_id ORDER BY sp.update_at DESC,sp.id DESC LIMIT 1))
                 LEFT JOIN worktree w ON w.worktree_id=s.worktree_id
                 WHERE COALESCE(s.deleted_at,0)=0 ORDER BY s.session_id"
            ).context("Unsupported TRAE database session schema")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let title = nonempty(row.get(1)?);
                let created: Option<i64> = row.get(2)?;
                let updated: Option<i64> = row.get(3)?;
                let kind: Option<String> = row.get(4)?;
                let native_url = format!("context://history/{id}");
                let locator = format!("{}#{id}", path.display());
                let session = Session {
                    provider: Provider::Trae,
                    scope: Scope::LocalConversation,
                    id: id.clone(),
                    path: locator.clone(),
                    paths: vec![locator],
                    title: title.clone().unwrap_or_default(),
                    title_source: title.map(|_| "metadata".into()),
                    created_at: created.and_then(|n| iso(&Value::from(n))),
                    cwd: nonempty(row.get(5)?),
                    branch: nonempty(row.get(6)?),
                    native_url,
                    source: "trae_ide".into(),
                    is_subagent: kind.as_deref() == Some("assistant_sub_chat"),
                    index_source: Some("native_database".into()),
                    ..Default::default()
                };
                let (revision, last_message) = revisions
                    .remove(&id)
                    .map(|(h, t)| (format!("{:x}", h.finalize()), t))
                    .unwrap_or_default();
                let latest = updated.unwrap_or(0).max(last_message);
                // Native timestamps have second precision. While a message is
                // changing, include DB/WAL fingerprints to detect same-second writes.
                let active = latest >= chrono::Utc::now().timestamp() - 2;
                let signature = hash(&serde_json::to_vec(&(
                    &session,
                    updated,
                    revision,
                    active.then_some(&stamp),
                ))?);
                entries.push(Entry { session, signature });
            }
        }
        if stable && !options.no_cache && source_stamp(path)? == stamp {
            // This stores normalized metadata and opaque revision hashes only.
            // Failing to cache cannot prevent reading the original database.
            let _ = crate::trae::atomic_json(
                &catalog_path,
                &Catalog {
                    stamp,
                    entries: entries.clone(),
                },
            );
        }
        Ok(Self {
            conn,
            entries,
            root,
            identity,
        })
    }

    fn parsed(&self, entry: &Entry, keep: bool) -> Result<Parsed> {
        let mut stmt = self.conn.prepare(
            "SELECT m.message_id,m.message_role,m.message_type,m.created_at,
             m.reply_to_message_id,g.content,t.content,c.content
             FROM chat_message m
             LEFT JOIN chat_message_general g ON g.message_id=m.message_id AND COALESCE(g.deleted_at,0)=0
             LEFT JOIN chat_message_task t ON t.message_id=m.message_id AND COALESCE(t.deleted_at,0)=0
             LEFT JOIN chat_message_chat c ON c.message_id=m.message_id AND COALESCE(c.deleted_at,0)=0
             WHERE m.session_id=?1 AND COALESCE(m.deleted_at,0)=0
             ORDER BY m.message_index,m.created_at,m.id"
        )?;
        let mut rows = stmt.query([&entry.session.id])?;
        let mut messages = Vec::new();
        let mut seen = HashMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let role: String = row.get(1)?;
            let kind: String = row.get(2)?;
            if !matches!(role.as_str(), "user" | "assistant") {
                continue;
            }
            let timestamp = row
                .get::<_, Option<i64>>(3)?
                .and_then(|n| iso(&Value::from(n)));
            let reply: Option<String> = row.get(4)?;
            let turn = if role == "user" {
                Some(id.clone())
            } else {
                reply
            };
            let data: Option<String> = row.get(match kind.as_str() {
                "general" => 5,
                "task" => 6,
                "chat" => 7,
                _ => bail!("Unsupported TRAE message type: {kind}"),
            })?;
            let Some(data) = data else { continue };
            let pieces = decode(&kind, &data)
                .with_context(|| format!("Invalid body format for TRAE message {id}"))?;
            for piece in pieces {
                let piece_role = piece.role.as_deref().unwrap_or(&role);
                let text = if piece_role == "user" {
                    user_text(&piece.text)
                } else {
                    piece.text.trim().into()
                };
                if text.is_empty() {
                    continue;
                }
                let message = Message {
                    role: piece_role.into(),
                    text,
                    timestamp: piece.timestamp.or_else(|| timestamp.clone()),
                    id: piece.id.or_else(|| Some(id.clone())),
                    turn_id: turn.clone(),
                    phase: piece.phase,
                    format: "trae_native".into(),
                    line: 0,
                };
                let key = (message.role.clone(), message.id.clone());
                // Stable native IDs denote revisions, not repeated questions.
                if let Some(&index) = seen.get(&key) {
                    messages[index] = message;
                } else {
                    seen.insert(key, messages.len());
                    messages.push(message);
                }
            }
        }
        let mut parsed = summarize(entry.session.clone(), messages, true, keep);
        if parsed.session.title.is_empty() {
            let (title, source) = if parsed.session.first_user_preview.is_empty() {
                (parsed.session.id.clone(), "id")
            } else {
                (parsed.session.first_user_preview.clone(), "first_user")
            };
            parsed.session.title = title;
            parsed.session.title_source = Some(source.into());
        }
        Ok(parsed)
    }

    pub fn records(
        &self,
        options: &Options,
        stats: &mut Stats,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Loaded>> {
        self.records_observed(options, stats, warnings, &mut |_| Ok(()))
    }

    pub fn records_observed(
        &self,
        options: &Options,
        stats: &mut Stats,
        warnings: &mut Vec<String>,
        observer: &mut impl FnMut(&Loaded) -> Result<()>,
    ) -> Result<Vec<Loaded>> {
        let mut cache = if options.no_cache {
            None
        } else {
            match measure(&mut stats.cache_read_ms, || {
                Cache::open(&self.root, &self.identity)
            }) {
                Ok(cache) => Some(cache),
                Err(_) => {
                    warnings.push(
                        "TRAE derived cache unavailable; reading the native database directly"
                            .into(),
                    );
                    None
                }
            }
        };
        let paths = self
            .entries
            .iter()
            .map(|e| PathBuf::from(&e.session.path))
            .collect::<Vec<_>>();
        let mut writes = Vec::new();
        let mut records = Vec::new();
        for entry in &self.entries {
            if options
                .id
                .as_ref()
                .is_some_and(|prefix| !entry.session.id.starts_with(prefix))
            {
                continue;
            }
            let saved = if options.refresh {
                None
            } else if let Some(cache) = &cache {
                measure(&mut stats.cache_read_ms, || {
                    cache
                        .read(&entry.session.path, &entry.signature, false, true)
                        .ok()
                        .flatten()
                })
            } else {
                None
            };
            let loaded = if let Some(saved) = saved {
                stats.cached_files += 1;
                saved
            } else {
                let parsed = measure(&mut stats.body_read_ms, || self.parsed(entry, false))?;
                let loaded = Loaded {
                    session: parsed.session,
                    search_text: parsed.search_text,
                    search_messages: parsed.search_messages,
                };
                writes.push(Write {
                    path: entry.session.path.clone(),
                    signature: entry.signature.clone(),
                    loaded: loaded.clone(),
                });
                stats.parsed_files += 1;
                loaded
            };
            observer(&loaded)?;
            records.push(loaded);
        }
        if let Some(cache) = &mut cache
            && cache.save(&writes, &paths).is_err()
        {
            warnings.push("TRAE derived cache write failed; results remain available".into());
        }
        stats.total_files += paths.len();
        Ok(records)
    }

    pub fn show(&self, id: &str) -> Result<Parsed> {
        self.parsed(
            self.entries
                .iter()
                .find(|e| e.session.id == id)
                .context("TRAE conversation does not exist")?,
            true,
        )
    }

    pub(crate) fn source_identity(&self) -> String {
        self.identity.to_string_lossy().into_owned()
    }

    pub fn sync(&self, prefix: Option<&str>) -> Result<usize> {
        let count = self
            .entries
            .iter()
            .filter(|e| prefix.is_none_or(|p| e.session.id.starts_with(p)))
            .count();
        ensure!(count > 0, "No TRAE conversations found");
        ensure!(
            prefix.is_none() || count == 1,
            "ID prefix matches multiple TRAE conversations"
        );
        // records() performs the forced conversion immediately after this call.
        Ok(count)
    }
}

fn source_stamp(path: &Path) -> Result<String> {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    Ok(format!(
        "{}:{}",
        cache::fingerprint(path, None)?,
        if wal.exists() {
            cache::fingerprint(&wal, None)?
        } else {
            String::new()
        }
    ))
}

#[derive(Default)]
struct Piece {
    text: String,
    role: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    phase: Option<String>,
}

// Typed deserialization skips the large tool result/reasoning fields entirely.
#[derive(Deserialize, Default)]
struct Part {
    #[serde(default)]
    r#type: String,
    text_content: Option<String>,
}
fn parts(parts: Vec<Part>) -> String {
    parts
        .into_iter()
        .filter(|p| p.r#type == "text")
        .filter_map(|p| p.text_content)
        .collect::<Vec<_>>()
        .join("\n")
}
#[derive(Deserialize)]
struct Task {
    // Newly created or cancelled tasks can legitimately persist {}.
    #[serde(default)]
    messages: Vec<TaskMessage>,
}
#[derive(Deserialize)]
struct TaskMessage {
    id: Option<String>,
    r#type: String,
    plan_item: Option<Plan>,
    append_input: Option<Append>,
}
#[derive(Deserialize)]
struct Plan {
    id: Option<String>,
    thought: Option<String>,
    hide: Option<bool>,
    timing: Option<Timing>,
    tool_call_info: Option<Tool>,
}
#[derive(Deserialize)]
struct Timing {
    generated_at_ms: Option<i64>,
}
#[derive(Deserialize)]
struct Tool {
    name: Option<String>,
    params: Option<ReplyParams>,
}
#[derive(Deserialize)]
struct ReplyParams {
    summary: Option<String>,
    message: Option<String>,
}
#[derive(Deserialize)]
struct Append {
    payload: AppendPayload,
}
#[derive(Deserialize)]
struct AppendPayload {
    data: AppendData,
}
#[derive(Deserialize)]
struct AppendData {
    message_id: Option<String>,
    user_message_id: Option<String>,
    message_contents: Vec<Part>,
}
#[derive(Deserialize)]
struct Chat {
    content: String,
}

fn decode(kind: &str, data: &str) -> Result<Vec<Piece>> {
    match kind {
        "general" => Ok(vec![Piece {
            text: parts(serde_json::from_str(data)?),
            ..Default::default()
        }]),
        "chat" => Ok(vec![Piece {
            text: serde_json::from_str::<Chat>(data)?.content,
            ..Default::default()
        }]),
        "task" => {
            let task: Task = serde_json::from_str(data)?;
            let mut out = Vec::new();
            for (index, message) in task.messages.into_iter().enumerate() {
                match message.r#type.as_str() {
                    "plan_item" => {
                        let plan = message.plan_item.context("plan_item content is missing")?;
                        if plan.hide == Some(true) {
                            continue;
                        }
                        let id = plan
                            .id
                            .or(message.id)
                            .unwrap_or_else(|| format!("part-{index}"));
                        let timestamp = plan
                            .timing
                            .and_then(|t| t.generated_at_ms)
                            .and_then(millis_iso);
                        if let Some(text) = plan.thought.filter(|s| !s.is_empty()) {
                            out.push(Piece {
                                text,
                                id: Some(format!("{id}:thought")),
                                timestamp: timestamp.clone(),
                                phase: Some("commentary".into()),
                                ..Default::default()
                            });
                        }
                        if let Some(tool) = plan.tool_call_info
                            && let Some(params) = tool.params
                        {
                            let text = match tool.name.as_deref() {
                                Some("finish") => params.summary,
                                Some("ReplyUser") => params.message,
                                _ => None,
                            };
                            if let Some(text) = text {
                                out.push(Piece {
                                    text,
                                    id: Some(format!("{id}:reply")),
                                    timestamp,
                                    phase: Some("final".into()),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    "append_input" => {
                        let input = message
                            .append_input
                            .context("append_input content is missing")?
                            .payload
                            .data;
                        out.push(Piece {
                            text: parts(input.message_contents),
                            role: Some("user".into()),
                            id: input.user_message_id.or(input.message_id).or(message.id),
                            ..Default::default()
                        });
                    }
                    other => bail!("Unsupported TRAE task message type: {other}"),
                }
            }
            Ok(out)
        }
        _ => bail!("Unsupported body type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::{Cli, Request},
        query,
    };
    use clap::Parser;
    use serde_json::json;

    const ID: &str = "010000000000400080000001";

    fn fixture(path: &Path, key: Option<&[u8]>) -> Connection {
        let db = Connection::open(path).unwrap();
        #[cfg(target_os = "macos")]
        if let Some(key) = key {
            cipher::unlock(&db, key).unwrap();
        }
        #[cfg(not(target_os = "macos"))]
        assert!(key.is_none());
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE chat_session(session_id TEXT,session_title TEXT,created_at INTEGER,
                 updated_at INTEGER,session_type TEXT,project_id TEXT,worktree_id TEXT,deleted_at INTEGER);
             CREATE TABLE project(project_id TEXT,absolute_path TEXT);
             CREATE TABLE session_project(id INTEGER,session_id TEXT,project_id TEXT,update_at INTEGER);
             CREATE TABLE worktree(worktree_id TEXT,worktree_path TEXT,worktree_branch TEXT);
             CREATE TABLE chat_message(id INTEGER,message_id TEXT PRIMARY KEY,session_id TEXT,
                 message_role TEXT,message_type TEXT,message_index INTEGER,created_at INTEGER,
                 updated_at INTEGER,deleted_at INTEGER,is_archived INTEGER,reply_to_message_id TEXT);
             CREATE TABLE chat_message_general(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             CREATE TABLE chat_message_task(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             CREATE TABLE chat_message_chat(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             INSERT INTO project VALUES('p','/tmp/native-project');
             INSERT INTO session_project VALUES(1,'010000000000400080000001','p',100);
             INSERT INTO chat_session VALUES('010000000000400080000001','原生标题适配原因',1793491200,1793491200,'side_chat',NULL,NULL,0);
             INSERT INTO chat_message VALUES(1,'u','010000000000400080000001','user','general',1,1793491200,1793491200,0,1,NULL);
             INSERT INTO chat_message VALUES(2,'a','010000000000400080000001','assistant','task',2,1793491210,1793491210,0,0,'u');"
        ).unwrap();
        db.execute("INSERT INTO chat_message_general VALUES('u',?1,1793491200,0)",
            [json!([{"type":"text","text_content":"第一条原文问题"},{"type":"image","image_url":"excluded-image"}]).to_string()]).unwrap();
        let task = json!({"messages":[
            {"type":"plan_item","id":"p1","plan_item":{"thought":"进度说明","reasoning_content":"excluded-reasoning",
                "tool_call_info":{"name":"Shell","params":{"command":"excluded-command"},"result":{"stdout":"excluded-output"}}}},
            {"type":"plan_item","id":"p2","plan_item":{"thought":"","timing":{"generated_at_ms":1793491220123i64},
                "tool_call_info":{"name":"finish","params":{"summary":"assistant-only-answer"},"result":{"text":"excluded-result"}}}}
        ]});
        db.execute(
            "INSERT INTO chat_message_task VALUES('a',?1,1793491220,0)",
            [task.to_string()],
        )
        .unwrap();
        db
    }

    fn request(root: &Path, args: &[&str]) -> Request {
        let mut argv = vec![
            "session-finder".into(),
            "--provider".into(),
            "trae".into(),
            "--trae-db".into(),
            root.join("database.db").display().to_string(),
            "--cache-dir".into(),
            root.join("cache").display().to_string(),
        ];
        argv.extend(args.iter().map(|s| s.to_string()));
        Request::new(Cli::try_parse_from(argv).unwrap(), false).unwrap()
    }

    #[test]
    fn native_incremental_matches_preserve_cold_warm_and_uncached_results() {
        let root = tempfile::tempdir().unwrap();
        let _writer = fixture(&root.path().join("database.db"), None);
        for extra in [vec![], vec![], vec!["--no-cache"]] {
            let mut args = vec!["search", "assistant-only-answer", "--sort", "relevance"];
            args.extend(extra);
            let req = request(root.path(), &args);
            let mut candidate = None;
            let mut completed = false;
            let out = query::execute_incremental(&req, &mut |event| {
                match event {
                    query::Event::Candidate { session } => {
                        assert!(!completed);
                        assert_eq!(session.id, ID);
                        assert!(session.match_score > 0.0);
                        assert_eq!(session.project.name, "native-project");
                        candidate = Some((session.id.clone(), session.match_score));
                    }
                    query::Event::Results { .. } => completed = true,
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
            assert!(completed);
            assert!(out.stats.metadata_read_ms > 0.0);
            if out.stats.parsed_files > 0 {
                assert!(out.stats.body_read_ms > 0.0);
            } else {
                assert_eq!(out.stats.body_read_ms, 0.0);
            }
            if req.options.no_cache {
                assert_eq!(out.stats.cache_read_ms, 0.0);
            } else {
                assert!(out.stats.cache_read_ms > 0.0);
            }
            assert_eq!(
                candidate,
                Some((out.sessions[0].id.clone(), out.sessions[0].match_score))
            );
            let final_out = query::execute(&req).unwrap();
            assert_eq!(out.json(&req)["sessions"], final_out.json(&req)["sessions"]);
        }
    }

    #[test]
    fn native_titles_fulltext_filters_cache_and_show_use_shared_pipeline() {
        let root = tempfile::tempdir().unwrap();
        let _writer = fixture(&root.path().join("database.db"), None);
        let req = request(root.path(), &["search", "适配原因", "--exact"]);
        let cold = query::execute(&req).unwrap();
        assert_eq!(cold.total, 1);
        assert_eq!(cold.sessions[0].title, "原生标题适配原因");
        assert_eq!(cold.sessions[0].cwd.as_deref(), Some("/tmp/native-project"));
        assert_eq!(cold.sessions[0].scope, Scope::LocalConversation);
        assert_eq!(
            cold.sessions[0].last_reply_at.as_deref(),
            Some("2026-11-01T00:00:20.123Z")
        );
        assert_eq!(cold.sessions[0].message_count, Some(3));
        assert!(cold.warnings.is_empty());
        let warm = query::execute(&req).unwrap();
        assert_eq!(warm.stats.cached_files, 1);
        assert_eq!(warm.stats.parsed_files, 0);
        assert_eq!(warm.sessions[0].public(), cold.sessions[0].public());
        let uncached = query::execute(&request(
            root.path(),
            &["search", "适配原因", "--exact", "--no-cache"],
        ))
        .unwrap();
        assert_eq!(uncached.sessions[0].public(), cold.sessions[0].public());
        assert_eq!(
            query::execute(&request(
                root.path(),
                &["search", "assistant-only-answer", "--exact"]
            ))
            .unwrap()
            .total,
            1
        );
        assert_eq!(
            query::execute(&request(root.path(), &["search", "excluded"]))
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            query::execute(&request(
                root.path(),
                &[
                    "recent",
                    "-p",
                    "native-project",
                    "--sort",
                    "created",
                    "--since",
                    "2026-11-01T00:00:00Z",
                    "--until",
                    "2026-11-02T00:00:00Z"
                ]
            ))
            .unwrap()
            .total,
            1
        );
        let shown = query::execute(&request(root.path(), &["show", ID])).unwrap();
        assert_eq!(shown.messages.len(), 3);
        assert_eq!(shown.messages[0].text, "第一条原文问题");
        assert_eq!(shown.messages[2].phase.as_deref(), Some("final"));
        assert_eq!(shown.messages[2].turn_id.as_deref(), Some("u"));
        let synced = query::execute(&request(root.path(), &["sync"])).unwrap();
        assert_eq!(synced.stats.parsed_files, 1);
    }

    #[test]
    fn native_wal_rename_body_update_deletion_and_readonly() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database.db");
        let writer = fixture(&path, None);
        let before = fs::read(&path).unwrap();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let before_wal = fs::read(&wal).unwrap();
        let readonly = open_readonly(&path, Path::new("unused")).unwrap();
        assert!(readonly.execute("DELETE FROM chat_session", []).is_err());
        let req = request(root.path(), &["recent"]);
        assert_eq!(query::execute(&req).unwrap().total, 1);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&wal).unwrap(), before_wal);
        writer
            .execute("UPDATE chat_session SET session_title='改名标题'", [])
            .unwrap();
        assert_eq!(query::execute(&req).unwrap().sessions[0].title, "改名标题");
        writer.execute("UPDATE chat_message_task SET content=?1,updated_at=?2",
            rusqlite::params![json!({"messages":[{"id":"x","type":"plan_item","plan_item":{"tool_call_info":{"name":"ReplyUser","params":{"message":"changed-answer"}}}}]}).to_string(),chrono::Utc::now().timestamp()]).unwrap();
        assert_eq!(
            query::execute(&request(
                root.path(),
                &["search", "changed-answer", "--exact"]
            ))
            .unwrap()
            .total,
            1
        );
        // Same-second streaming content changes must not reuse the old body.
        writer.execute("UPDATE chat_message_task SET content=?1",
            [json!({"messages":[{"id":"x","type":"plan_item","plan_item":{"thought":"second-stream-update"}}]}).to_string()]).unwrap();
        assert_eq!(
            query::execute(&request(
                root.path(),
                &["search", "second-stream-update", "--exact"]
            ))
            .unwrap()
            .total,
            1
        );
        writer
            .execute(
                "UPDATE chat_message SET deleted_at=1 WHERE message_id='a'",
                [],
            )
            .unwrap();
        assert_eq!(
            query::execute(&request(
                root.path(),
                &["search", "second-stream-update", "--exact"]
            ))
            .unwrap()
            .total,
            0
        );
        writer
            .execute("UPDATE chat_session SET deleted_at=1", [])
            .unwrap();
        assert_eq!(query::execute(&req).unwrap().total, 0);
        writer
            .execute(
                "UPDATE chat_session SET deleted_at=0,session_title=NULL",
                [],
            )
            .unwrap();
        assert_eq!(
            query::execute(&req).unwrap().sessions[0].title,
            "第一条原文问题"
        );
    }

    #[test]
    fn empty_tasks_and_visible_reply_projection() {
        assert!(decode("task", "{}").unwrap().is_empty());
        let data = json!({"messages":[
            {"id":"h","type":"plan_item","plan_item":{"hide":true,"thought":"hidden"}},
            {"id":"a","type":"append_input","append_input":{"payload":{"data":{
                "user_message_id":"u","message_contents":[{"type":"text","text_content":"追加问题"}]}}}},
            {"id":"c","type":"plan_item","plan_item":{"thought":"可见进度","reasoning_content":"private"}}
        ]});
        let pieces = decode("task", &data.to_string()).unwrap();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].role.as_deref(), Some("user"));
        assert_eq!(pieces[0].id.as_deref(), Some("u"));
        assert_eq!(pieces[1].text, "可见进度");
        assert!(decode("task", r#"{"messages":[{"type":"new-unsupported-type"}]}"#).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn encrypted_fixture_reads_wal_and_rejects_wrong_key_and_unknown_build() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("encrypted.db");
        let writer = fixture(&path, Some(b"test-only-fixture-password"));
        assert!(encrypted(&path).unwrap());
        let before = fs::read(&path).unwrap();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        cipher::unlock(&conn, b"test-only-fixture-password").unwrap();
        conn.execute_batch("PRAGMA query_only=ON; BEGIN;").unwrap();
        let store = Store::from_connection(
            conn,
            &path,
            &Options {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.show(ID).unwrap().session.title, "原生标题适配原因");
        assert!(store.conn.execute("DELETE FROM chat_session", []).is_err());
        let wrong = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        cipher::unlock(&wrong, b"wrong-fixture-password").unwrap();
        assert!(
            wrong
                .query_row("SELECT count(*) FROM chat_session", [], |r| r
                    .get::<_, i64>(0))
                .is_err()
        );
        let fake_lib = root.path().join("unsupported.dylib");
        let mut header = [0u8; 32];
        header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        fs::write(&fake_lib, header).unwrap();
        assert!(
            cipher::key(&fake_lib)
                .unwrap_err()
                .to_string()
                .contains("Unsupported")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        drop(writer);
    }
}
