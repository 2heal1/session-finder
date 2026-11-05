//! TRAE CLI rollouts, including canonical history mutations, into shared models.
use crate::{
    cache::{self, Cache, Loaded, Write},
    catalog,
    cli::Options,
    model::{DbMeta, Message, Parsed, Provider, Session, Stats},
    parser,
    text::session_id,
    time::{iso, millis_iso},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const SCHEMA: &str = "traecli-rollout-v1";

pub struct Store {
    pub home: PathBuf,
    paths: Vec<PathBuf>,
    database: HashMap<String, DbMeta>,
    titles: HashMap<String, String>,
}

impl Store {
    pub fn open(options: &Options, warnings: &mut Vec<String>) -> Result<Self> {
        let home = options
            .traecli_home
            .clone()
            .or_else(|| std::env::var_os("TRAECLI_HOME").map(PathBuf::from))
            .unwrap_or_else(|| {
                std::env::var_os("TRAE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".trae"))
                    .join("cli")
            });
        let home = fs::canonicalize(&home)
            .with_context(|| format!("无法读取 TRAE CLI 目录 {}", home.display()))?;
        ensure!(home.is_dir(), "TRAE CLI 来源必须是目录");
        let mut paths = Vec::new();
        catalog::walk(&home.join("sessions"), &mut paths, warnings);
        catalog::walk(&home.join("archived_sessions"), &mut paths, warnings);
        paths.sort();
        if paths.is_empty() {
            warnings.push(format!("未找到 TRAE CLI rollout 日志：{}", home.display()));
        }
        let database = catalog::database(&home, warnings);
        let titles = catalog::titles(&home, warnings);
        Ok(Self {
            home,
            paths,
            database,
            titles,
        })
    }

    fn metadata(&self, path: &Path) -> Option<&DbMeta> {
        let id = path.file_name()?.to_str().and_then(session_id)?;
        self.database.get(&id)
    }

    fn decorate(&self, s: &mut Session) {
        s.provider = Provider::Traecli;
        // No verified native URI scheme; callers can use `traecli resume <id>`.
        s.native_url.clear();
        s.index_source = Some("local_rollout".into());
        if s.paths.is_empty() {
            s.paths.push(s.path.clone());
        }
        let db = self.database.get(&s.id);
        if let Some(db) = db {
            db.apply(s);
            if let Some(archived) = db.archived {
                s.archived = archived;
            }
        }
        let title = db
            .and_then(|d| d.name.as_ref())
            .or_else(|| self.titles.get(&s.id))
            .or_else(|| db.and_then(|d| d.title.as_ref()));
        let (title, source) = if let Some(title) = title {
            (title.clone(), "metadata")
        } else if !s.first_user_preview.is_empty() {
            (s.first_user_preview.clone(), "first_user")
        } else {
            (s.id.clone(), "id")
        };
        s.title = title;
        s.title_source = Some(source.into());
    }

    pub fn records(
        &self,
        options: &Options,
        stats: &mut Stats,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Loaded>> {
        stats.total_files += self.paths.len();
        let mut cache = if options.no_cache {
            None
        } else {
            match Cache::open(
                &options.cache_dir.clone().unwrap_or_else(cache::directory),
                &self.home.join(SCHEMA),
            ) {
                Ok(cache) => Some(cache),
                Err(e) => {
                    warnings.push(format!("TRAE CLI 缓存不可用，回退到日志：{e}"));
                    None
                }
            }
        };
        let mut records = Vec::new();
        let mut writes = Vec::new();
        for path in &self.paths {
            let id = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(session_id);
            if options
                .id
                .as_deref()
                .is_some_and(|prefix| id.as_deref().is_none_or(|id| !id.starts_with(prefix)))
            {
                continue;
            }
            let key = path.to_string_lossy().into_owned();
            let db = self.metadata(path);
            let signature = match cache::fingerprint(path, db) {
                Ok(s) => s,
                Err(e) => {
                    warnings.push(format!("TRAE CLI 无法读取 {}：{e}", path.display()));
                    continue;
                }
            };
            let cached = if !options.refresh
                && let Some(cache) = &cache
            {
                match cache.read(&key, &signature, false, true) {
                    Ok(v) => v,
                    Err(e) => {
                        warnings.push(format!("TRAE CLI 缓存记录不可用：{e}"));
                        None
                    }
                }
            } else {
                None
            };
            let mut loaded = if let Some(loaded) = cached {
                stats.cached_files += 1;
                loaded
            } else {
                let parsed = match parser::parse_traecli(path, db, true, false) {
                    Ok(p) => p,
                    Err(e) => {
                        warnings.push(format!("无法读取 TRAE CLI 正文 {}：{e}", path.display()));
                        continue;
                    }
                };
                stats.parsed_files += 1;
                let mut loaded = Loaded {
                    session: parsed.session,
                    search_text: parsed.search_text,
                    search_messages: parsed.search_messages,
                };
                self.decorate(&mut loaded.session);
                if cache::fingerprint(path, db).ok().as_ref() == Some(&signature) {
                    writes.push(Write {
                        path: key,
                        signature,
                        loaded: loaded.clone(),
                    });
                } else {
                    warnings.push(format!(
                        "TRAE CLI 会话正在写入，未缓存本次快照：{}",
                        loaded.session.id
                    ));
                }
                loaded
            };
            self.decorate(&mut loaded.session);
            if loaded.session.malformed_lines > 0 {
                warnings.push(format!(
                    "{}: 跳过 {} 行无效 JSON",
                    loaded.session.id, loaded.session.malformed_lines
                ));
            }
            records.push(loaded);
        }
        if let Some(cache) = &mut cache
            && let Err(e) = cache.save(&writes, &self.paths)
        {
            warnings.push(format!("TRAE CLI 缓存保存失败，结果仍可用：{e}"));
        }
        Ok(records)
    }

    pub fn show(&self, path: &str) -> Result<Parsed> {
        let path = Path::new(path);
        ensure!(
            self.paths.iter().any(|p| p == path),
            "未找到 TRAE CLI rollout"
        );
        let mut parsed = parser::parse_traecli(path, self.metadata(path), false, true)?;
        self.decorate(&mut parsed.session);
        Ok(parsed)
    }
}

#[derive(Default)]
pub(crate) struct Transcript {
    messages: Vec<Message>,
    commits: HashMap<String, usize>,
    turns: Vec<(String, usize)>,
    active_turn: Option<String>,
    rollback_event: Option<u64>,
}

impl Transcript {
    fn turn(&mut self, id: &str, line: usize) {
        self.active_turn = Some(id.to_owned());
        if !self.turns.iter().any(|(turn, _)| turn == id) {
            self.turns.push((id.to_owned(), line));
        }
    }

    fn truncate(&mut self, first_removed: usize) {
        self.messages.retain(|m| m.line < first_removed);
        self.turns.retain(|(_, line)| *line < first_removed);
        self.commits.retain(|_, line| *line < first_removed);
        self.active_turn = self.turns.last().map(|(id, _)| id.clone());
    }

    fn rollback(&mut self, count: u64) {
        if count == 0 {
            return;
        }
        let user_turns: Vec<_> = self
            .turns
            .iter()
            .filter(|(id, _)| {
                self.messages
                    .iter()
                    .any(|m| m.role == "user" && m.turn_id.as_ref() == Some(id))
            })
            .map(|(_, line)| *line)
            .collect();
        let index = user_turns.len().saturating_sub(count as usize);
        if let Some(line) = user_turns.get(index) {
            self.truncate(*line);
        }
    }

    fn add(&mut self, mut m: Message) {
        if m.turn_id.is_none() {
            m.turn_id.clone_from(&self.active_turn);
        }
        self.messages.push(m);
    }

    pub(crate) fn push(&mut self, record: &Value, line: usize) -> Result<()> {
        let p = &record["payload"];
        if record["type"] == "event_msg" && p["type"] == "task_started" {
            if let Some(id) = p["turn_id"].as_str() {
                self.turn(id, line);
            }
        } else if record["type"] == "event_msg" && p["type"] == "thread_rolled_back" {
            let count = p["num_turns"].as_u64().unwrap_or(0);
            if self.rollback_event.take() != Some(count) {
                self.rollback(count);
            }
        } else if record["type"] == "history_mutation" {
            ensure!(p["version"] == 1, "不支持的 TRAE CLI history_mutation 版本");
            match p["operation"].as_str() {
                Some("append") => {
                    self.rollback_event = None;
                    if let Some(id) = p["turn_id"].as_str() {
                        self.turn(id, line);
                    }
                    let completions = p["display_completions"].as_array();
                    let display_agent = completions.is_some_and(|items| {
                        items.iter().any(|v| v["item"]["type"] == "AgentMessage")
                    });
                    if let Some(items) = p["items"].as_array() {
                        for item in items {
                            if display_agent && item["role"] == "assistant" {
                                continue;
                            }
                            let mut item = item.clone();
                            // Canonical user items also carry environment/context blocks.
                            // The producer labels parts when this information is available.
                            if item["role"] == "user"
                                && let Some(kinds) = item.pointer(
                                    "/internal_chat_message_metadata_passthrough/content_item_kinds"
                                ).and_then(Value::as_array).cloned()
                                && let Some(parts) = item["content"].as_array_mut()
                                && kinds.len() == parts.len()
                            {
                                let mut i = 0;
                                parts.retain(|_| {
                                    let keep = kinds[i].as_str().is_some_and(|k| k.starts_with("user."));
                                    i += 1;
                                    keep
                                });
                            }
                            let synthetic = json!({
                                "type": "response_item", "timestamp": record["timestamp"], "payload": item
                            });
                            if let Some(mut m) = parser::message(&synthetic, line) {
                                m.format = "canonical".into();
                                self.add(m);
                            }
                        }
                    }
                    if let Some(completions) = completions {
                        for completion in completions {
                            if let Some(m) = display_message(completion, record, line) {
                                self.add(m);
                            }
                        }
                    }
                }
                Some("rollback") => {
                    let count = p["num_turns"].as_u64().unwrap_or(0);
                    if let Some(target) = p["target_commit_id"].as_str() {
                        let target_line = self
                            .commits
                            .get(target)
                            .copied()
                            .context("TRAE CLI rollback 的目标 commit 不在本地日志中")?;
                        self.truncate(target_line.saturating_add(1));
                    } else {
                        self.rollback(count);
                    }
                    self.rollback_event = Some(count);
                }
                _ => bail!("不支持的 TRAE CLI history_mutation 操作，未索引此会话"),
            }
            if let Some(commit) = p["commit_id"].as_str() {
                self.commits.insert(commit.into(), line);
            }
        } else if record["type"] == "event_msg" && p["type"] == "item_completed" {
            if let Some(m) = display_message(p, record, line) {
                self.add(m);
            }
        } else if let Some(m) = parser::message(record, line) {
            // Old rollouts can have no task_started/turn_id events.
            if m.role == "user"
                && self
                    .active_turn
                    .as_deref()
                    .is_none_or(|t| t.starts_with("legacy:"))
            {
                let duplicate = self.messages.last().is_some_and(|previous| {
                    previous.role == "user"
                        && previous.text == m.text
                        && previous.format != m.format
                        && line.saturating_sub(previous.line) <= 4
                });
                if !duplicate {
                    self.turn(&format!("legacy:{line}"), line);
                }
            }
            self.add(m);
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<Message> {
        parser::deduplicate(self.messages)
    }
}

fn display_message(completion: &Value, record: &Value, line: usize) -> Option<Message> {
    let item = &completion["item"];
    if item["type"] != "AgentMessage" {
        return None;
    }
    let text = item["content"]
        .as_array()?
        .iter()
        .filter(|p| p["type"] == "Text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(Message {
        role: "assistant".into(),
        text,
        timestamp: completion["completed_at_ms"]
            .as_i64()
            .and_then(millis_iso)
            .or_else(|| iso(&record["timestamp"])),
        phase: item["phase"].as_str().map(str::to_owned),
        id: item["id"].as_str().map(str::to_owned),
        turn_id: completion["turn_id"].as_str().map(str::to_owned),
        format: "canonical".into(),
        line,
    })
}
