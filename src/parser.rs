use crate::{
    model::{DbMeta, Message, Parsed, Session, source_name},
    text::{clean_user, normalize, preview, session_id},
    time::{iso, ms},
};
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    sync::LazyLock,
};

static FILE_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^rollout-(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})-(\d{2})").unwrap());

fn string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

pub fn metadata(path: &Path, meta: &Value, db: Option<&DbMeta>) -> Result<Session> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let file_id = session_id(&name).filter(|id| id.len() == 36);
    let id = file_id
        .clone()
        .or_else(|| meta["id"].as_str().and_then(session_id))
        .or_else(|| meta["session_id"].as_str().and_then(session_id));
    let Some(id) = id.filter(|id| id.len() == 36) else {
        bail!("No valid session UUID")
    };
    let own_meta = file_id.is_none()
        || meta
            .get("id")
            .or_else(|| meta.get("session_id"))
            .and_then(Value::as_str)
            .and_then(session_id)
            == file_id;
    let created_at = own_meta
        .then(|| iso(&meta["timestamp"]))
        .flatten()
        .or_else(|| {
            FILE_DATE.captures(&name).and_then(|m| {
                iso(&Value::String(format!(
                    "{}T{}:{}:{}",
                    &m[1], &m[2], &m[3], &m[4]
                )))
            })
        });
    let sub = &meta["source"]["subagent"];
    let spawned = sub
        .get("thread_spawn")
        .or_else(|| sub.get("spawn"))
        .unwrap_or(sub);
    let source = source_name(&meta["source"]);
    let mut session = Session {
        native_url: format!("codex://threads/{id}"),
        id,
        path: path.to_string_lossy().into_owned(),
        created_at,
        cwd: string(&meta["cwd"]),
        is_subagent: source == "subagent",
        source,
        parent_session_id: own_meta
            .then(|| {
                string(&spawned["parent_thread_id"])
                    .or_else(|| string(&spawned["parentThreadId"]))
                    .or_else(|| string(&meta["parent_thread_id"]))
            })
            .flatten(),
        forked_from_id: own_meta.then(|| string(&meta["forked_from_id"])).flatten(),
        repository_url: string(&meta["git"]["repository_url"]),
        branch: string(&meta["git"]["branch"]),
        compressed: name.ends_with(".zst"),
        archived: path
            .components()
            .any(|p| p.as_os_str() == "archived_sessions"),
        details_available: false,
        ..Default::default()
    };
    if let Some(db) = db {
        db.apply(&mut session);
    }
    Ok(session)
}

fn content(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_owned();
    }
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| match p["type"].as_str()? {
                    "input_text" | "output_text" | "text" => p["text"].as_str(),
                    "image" | "input_image" | "output_image" => Some("[图片]"),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub(crate) fn message(record: &Value, line: usize) -> Option<Message> {
    let p = &record["payload"];
    let (role, text, format, phase, id) = match record["type"].as_str()? {
        "response_item" if p["type"] == "message" => (
            p["role"].as_str()?,
            content(&p["content"]),
            "response",
            string(&p["phase"]),
            string(&p["id"]),
        ),
        "event_msg" => {
            let role = match p["type"].as_str()? {
                "user_message" => "user",
                "agent_message" | "assistant_message" => "assistant",
                _ => return None,
            };
            (
                role,
                p["message"]
                    .as_str()
                    .or_else(|| record["message"].as_str())
                    .unwrap_or("")
                    .to_owned(),
                "event",
                string(&p["phase"]),
                None,
            )
        }
        "message" => (
            record["role"].as_str()?,
            content(&record["content"]),
            "legacy",
            None,
            string(&record["id"]),
        ),
        _ => return None,
    };
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let text = if role == "user" {
        clean_user(&text)
    } else {
        text.trim().to_owned()
    };
    if text.is_empty() {
        return None;
    }
    Some(Message {
        role: role.into(),
        text,
        timestamp: iso(record
            .get("timestamp")
            .filter(|v| !v.is_null())
            .unwrap_or(&p["timestamp"])),
        format: format.into(),
        phase,
        id,
        turn_id: None,
        line,
    })
}

pub(crate) fn deduplicate(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut recent: HashMap<[u8; 32], usize> = HashMap::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    for m in messages {
        if let Some(index) = m.id.as_ref().and_then(|id| ids.get(id)).copied() {
            if m.text.len() >= out[index].text.len() {
                out[index] = m;
            }
            continue;
        }
        let mut hash = Sha256::new();
        hash.update(&m.role);
        hash.update([0]);
        hash.update(&m.text);
        let key: [u8; 32] = hash.finalize().into();
        if let Some(&index) = recent.get(&key) {
            let previous = &out[index];
            let close = m.line.saturating_sub(previous.line) <= 4
                || (m.timestamp.is_some()
                    && previous.timestamp.is_some()
                    && ms(&m.timestamp).abs_diff(ms(&previous.timestamp)) <= 2000);
            let same_turn =
                previous.turn_id.is_none() || m.turn_id.is_none() || previous.turn_id == m.turn_id;
            if close && same_turn && previous.format != m.format {
                if m.format == "canonical"
                    || (m.format == "response" && previous.format != "canonical")
                {
                    if let Some(id) = &m.id {
                        ids.insert(id.clone(), index);
                    }
                    out[index] = m;
                }
                recent.remove(&key);
                continue;
            }
        }
        recent.insert(key, out.len());
        if let Some(id) = &m.id {
            ids.insert(id.clone(), out.len());
        }
        out.push(m);
    }
    out
}

/// Decode incrementally, without a decompressed file or a whole-file JSON buffer.
pub fn parse(
    path: &Path,
    db: Option<&DbMeta>,
    header: bool,
    full: bool,
    keep_messages: bool,
) -> Result<Parsed> {
    parse_impl(path, db, header, full, keep_messages, false)
}

pub(crate) fn parse_traecli(
    path: &Path,
    db: Option<&DbMeta>,
    full: bool,
    keep_messages: bool,
) -> Result<Parsed> {
    parse_impl(path, db, false, full, keep_messages, true)
}

fn parse_impl(
    path: &Path,
    db: Option<&DbMeta>,
    header: bool,
    full: bool,
    keep_messages: bool,
    traecli: bool,
) -> Result<Parsed> {
    let file = File::open(path).with_context(|| format!("Reading {}", path.display()))?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::with_capacity(64 * 1024, input);
    let file_id = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(session_id);
    let mut meta = Value::Null;
    let mut messages = Vec::new();
    let mut cli_history = crate::traecli::Transcript::default();
    let mut malformed = 0;
    let mut incomplete = false;
    let mut line = Vec::new();
    let mut number = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record: Value = match serde_json::from_slice(&line) {
            Ok(v) if matches!(v, Value::Object(_)) => v,
            _ => {
                if line.last() == Some(&b'\n') {
                    malformed += 1;
                } else {
                    incomplete = true;
                }
                continue;
            }
        };
        if record["type"] == "session_meta" && record["payload"].is_object() {
            let mut candidate = record["payload"].clone();
            if candidate["timestamp"].is_null() {
                candidate["timestamp"] = record["timestamp"].clone();
            }
            if meta.is_null()
                || (candidate["id"].as_str().and_then(session_id) == file_id
                    && meta["id"].as_str().and_then(session_id) != file_id)
            {
                meta = candidate;
            }
            if header && (file_id.is_none() || meta["id"].as_str().and_then(session_id) == file_id)
            {
                break;
            }
        }
        if !header {
            if traecli {
                cli_history.push(&record, number)?;
            } else if let Some(message) = message(&record, number) {
                messages.push(message);
            }
        }
    }
    let mut session = metadata(path, &meta, db)?;
    session.malformed_lines = malformed;
    session.incomplete_tail = incomplete;
    if header {
        return Ok(Parsed {
            session,
            search_text: None,
            search_messages: Vec::new(),
            messages: Vec::new(),
        });
    }
    Ok(summarize(
        session,
        if traecli {
            cli_history.finish()
        } else {
            deduplicate(messages)
        },
        full,
        keep_messages,
    ))
}

/// Provider adapters feed the same message model into summary and search indexing.
/// Deduplication is provider-specific and happens before this boundary.
pub fn summarize(
    mut session: Session,
    messages: Vec<Message>,
    full: bool,
    keep_messages: bool,
) -> Parsed {
    let is_fork = session.forked_from_id.is_some() || session.is_subagent;
    let own = |m: &&Message| {
        !is_fork
            || session.created_at.is_none()
            || (m.timestamp.is_some() && ms(&m.timestamp) >= ms(&session.created_at))
    };
    let users: Vec<_> = messages.iter().filter(|m| m.role == "user").collect();
    let replies: Vec<_> = messages.iter().filter(|m| m.role == "assistant").collect();
    let own_users: Vec<_> = users.iter().copied().filter(own).collect();
    let last_user = own_users.iter().max_by_key(|m| ms(&m.timestamp));
    let last_reply = replies
        .iter()
        .copied()
        .filter(own)
        .max_by_key(|m| ms(&m.timestamp));
    session.first_user_preview = own_users
        .first()
        .map(|m| preview(&m.text))
        .unwrap_or_default();
    session.last_user_preview = last_user.map(|m| preview(&m.text)).unwrap_or_default();
    session.last_reply_preview = last_reply.map(|m| preview(&m.text)).unwrap_or_default();
    session.last_user_at = last_user.and_then(|m| m.timestamp.clone());
    session.last_reply_at = last_reply.and_then(|m| m.timestamp.clone());
    session.message_count = Some(messages.len());
    session.user_message_count = Some(users.len());
    session.reply_count = Some(replies.len());
    session.details_available = true;
    let search_text = full.then(|| {
        let mut text = String::new();
        for message in &messages {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&normalize(&message.text));
        }
        text
    });
    Parsed {
        session,
        search_text,
        search_messages: if full {
            messages
                .iter()
                .map(|m| crate::model::SearchMessage {
                    role: m.role.clone(),
                    text: m.text.clone(),
                })
                .collect()
        } else {
            Vec::new()
        },
        messages: if keep_messages { messages } else { Vec::new() },
    }
}
