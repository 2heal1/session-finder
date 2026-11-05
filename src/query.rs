use crate::{
    cache::{self, Cache, Loaded, Write},
    catalog,
    cli::{Action, Providers, Request, Sort},
    model::{DbMeta, Message, Project, Provider, Session, Stats},
    parser, project,
    text::{normalize, score_normalized, session_id},
    time::ms,
};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::PathBuf,
    time::Instant,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGroup {
    #[serde(flatten)]
    pub project: Project,
    pub session_count: usize,
    pub latest_created_at: Option<String>,
    pub latest_reply_at: Option<String>,
    pub cwds: BTreeSet<String>,
}

pub struct Output {
    pub home: PathBuf,
    pub warnings: Vec<String>,
    pub stats: Stats,
    pub total: usize,
    pub sessions: Vec<Session>,
    pub projects: Vec<ProjectGroup>,
    pub messages: Vec<Message>,
    pub transcript_file: Option<crate::transcript::TranscriptFile>,
    pub relations: Option<crate::relations::Graph>,
}

impl Output {
    pub fn json(&self, request: &Request) -> serde_json::Value {
        let mut value = serde_json::json!({
            "schemaVersion": if request.action == Action::Search { 2 } else { 1 },
            "source": match request.options.provider {
                Providers::Codex => "local_codex_rollout",
                Providers::Trae => "local_trae",
                Providers::Traecli => "local_traecli_rollout",
                Providers::All => "local_session_providers",
            },
            "home": self.home,
            "timeZone": iana_time_zone::get_timezone().unwrap_or_else(|_| "local".into()),
            "warnings": self.warnings, "stats": self.stats,
        });
        let object = value.as_object_mut().unwrap();
        match request.action {
            Action::Relations => {
                let graph = serde_json::to_value(self.relations.as_ref().unwrap()).unwrap();
                object.extend(graph.as_object().unwrap().clone());
                object.insert("source".into(), "local_codex_metadata".into());
            }
            Action::Show => {
                if let Some(file) = &self.transcript_file {
                    let s = &self.sessions[0];
                    object.insert(
                        "session".into(),
                        serde_json::json!({
                            "id": s.id,
                            "provider": s.provider,
                            "title": s.title,
                            "titleSource": s.title_source,
                            "scope": s.scope,
                            "messageCount": self.messages.len(),
                            "indexSource": s.index_source,
                            "indexedAt": s.indexed_at,
                            "syncedAt": s.synced_at,
                        }),
                    );
                    object.insert("file".into(), serde_json::to_value(file).unwrap());
                } else {
                    object.insert("session".into(), self.sessions[0].public());
                    object.insert(
                        "messages".into(),
                        serde_json::to_value(&self.messages).unwrap(),
                    );
                }
            }
            Action::Projects => {
                object.insert("total".into(), self.total.into());
                object.insert(
                    "projects".into(),
                    serde_json::to_value(&self.projects).unwrap(),
                );
            }
            _ => {
                object.insert("total".into(), self.total.into());
                object.insert("query".into(), request.query.clone().into());
                object.insert(
                    "project".into(),
                    serde_json::to_value(&request.options.project).unwrap(),
                );
                object.insert("sort".into(), request.options.sort.name().into());
                object.insert(
                    "sessions".into(),
                    self.sessions
                        .iter()
                        .map(|s| {
                            if request.action == Action::Search {
                                s.list_json(request.options.verbose, !request.query.is_empty())
                            } else {
                                s.public()
                            }
                        })
                        .collect(),
                );
            }
        }
        value
    }
}

struct Engine<'a> {
    request: &'a Request,
    database: HashMap<String, DbMeta>,
    titles: HashMap<String, String>,
    cache: Option<Cache>,
    writes: Vec<Write>,
    loaded: HashMap<(Provider, String), Loaded>,
    messages: HashMap<String, Vec<Message>>,
    projects: HashMap<String, Project>,
    warnings: Vec<String>,
    stats: Stats,
    pool: rayon::ThreadPool,
}

impl Engine<'_> {
    fn decorate(&mut self, mut s: Session) -> Session {
        if s.provider != Provider::Codex {
            s.project = project::resolve(&s, &mut self.projects);
            return s;
        }
        let db = self.database.get(&s.id);
        if let Some(db) = db {
            db.apply(&mut s);
        }
        s.title = self
            .titles
            .get(&s.id)
            .cloned()
            .or_else(|| db.and_then(|d| d.title.clone()))
            .unwrap_or_else(|| {
                if s.first_user_preview.is_empty() {
                    s.id.clone()
                } else {
                    s.first_user_preview.clone()
                }
            });
        s.project = project::resolve(&s, &mut self.projects);
        if s.paths.is_empty() {
            s.paths.push(s.path.clone());
        }
        s
    }

    fn load(&mut self, paths: &[PathBuf], header: bool, full: bool) -> Vec<Session> {
        let mut records = Vec::new();
        let mut pending = Vec::new();
        for path in paths {
            let key = path.to_string_lossy().into_owned();
            if let Some(saved) = self.loaded.get(&(Provider::Codex, key.clone()))
                && (header || saved.session.message_count.is_some())
                && (!full || saved.search_text.is_some())
            {
                records.push(self.decorate(saved.session.clone()));
                continue;
            }
            let id = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(session_id);
            let db = id.as_ref().and_then(|id| self.database.get(id)).cloned();
            let signature = match cache::fingerprint(path, db.as_ref()) {
                Ok(v) => v,
                Err(e) => {
                    self.warnings.push(format!("跳过 {}: {e}", path.display()));
                    continue;
                }
            };
            if !self.request.options.refresh
                && let Some(cache) = &self.cache
            {
                match cache.read(&key, &signature, header, full) {
                    Ok(Some(loaded)) => {
                        self.stats.cached_files += 1;
                        self.warn_record(&loaded.session);
                        records.push(self.decorate(loaded.session.clone()));
                        self.loaded.insert((Provider::Codex, key), loaded);
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => self
                        .warnings
                        .push(format!("缓存记录不可用，回退到日志：{e}")),
                }
            }
            pending.push((path.clone(), db, signature));
        }
        let keep_messages = self.request.action == Action::Show;
        let parsed: Vec<_> = self.pool.install(|| {
            pending
                .par_iter()
                .map(|(path, db, signature)| {
                    let result = parser::parse(path, db.as_ref(), header, full, keep_messages);
                    let stable =
                        cache::fingerprint(path, db.as_ref()).ok().as_ref() == Some(signature);
                    (path, signature, result, stable)
                })
                .collect()
        });
        for (path, signature, result, stable) in parsed {
            match result {
                Ok(parsed) => {
                    if header {
                        self.stats.header_files += 1;
                    } else {
                        self.stats.parsed_files += 1;
                    }
                    self.warn_record(&parsed.session);
                    let key = path.to_string_lossy().into_owned();
                    if keep_messages {
                        self.messages.insert(key.clone(), parsed.messages);
                    }
                    let loaded = Loaded {
                        session: parsed.session,
                        search_text: parsed.search_text,
                        search_messages: parsed.search_messages,
                    };
                    if !stable {
                        self.warnings.push(format!(
                            "会话正在写入，结果为本次读取的快照：{}",
                            loaded.session.id
                        ));
                    } else if self.cache.is_some() {
                        self.writes.push(Write {
                            path: key.clone(),
                            signature: signature.clone(),
                            loaded: loaded.clone(),
                        });
                    }
                    records.push(self.decorate(loaded.session.clone()));
                    self.loaded.insert((Provider::Codex, key), loaded);
                }
                Err(e) => self
                    .warnings
                    .push(format!("无法读取正文 {}: {e}", path.display())),
            }
        }
        records
    }

    fn hydrate(&mut self, sessions: &[Session], full: bool) -> Vec<Session> {
        let paths: Vec<_> = sessions
            .iter()
            .filter(|s| s.provider == Provider::Codex)
            .flat_map(|s| s.paths.iter().map(PathBuf::from))
            .collect();
        let mut records = self.load(&paths, false, full);
        for session in sessions.iter().filter(|s| s.provider != Provider::Codex) {
            records.push(self.decorate(session.clone()));
        }
        records
    }

    fn warn_record(&mut self, session: &Session) {
        if session.malformed_lines > 0 {
            self.warnings.push(format!(
                "{}: 跳过 {} 行无效 JSON",
                session.id, session.malformed_lines
            ));
        }
    }
}

fn unique(records: Vec<Session>) -> Vec<Session> {
    let mut records = records;
    records.sort_by(|a, b| a.path.cmp(&b.path));
    let mut map: BTreeMap<(Provider, String), Session> = BTreeMap::new();
    for mut record in records {
        let key = (record.provider, record.id.clone());
        if let Some(previous) = map.get_mut(&key) {
            let prefer = ms(&record.last_reply_at) > ms(&previous.last_reply_at)
                || (ms(&record.last_reply_at) == ms(&previous.last_reply_at)
                    && record.message_count > previous.message_count);
            if prefer {
                record.paths.extend(previous.paths.iter().cloned());
                *previous = record;
            } else {
                previous.paths.extend(record.paths);
            }
        } else {
            map.insert(key, record);
        }
    }
    map.into_values()
        .map(|mut s| {
            s.paths.sort();
            s.paths.dedup();
            s
        })
        .collect()
}

fn in_time(s: &Session, request: &Request) -> bool {
    let time = ms(if request.options.sort == Sort::Created {
        &s.created_at
    } else {
        &s.last_reply_at
    });
    request.since.is_none_or(|v| time >= v)
        && request.until.is_none_or(|v| time != i64::MIN && time < v)
}

fn sort(records: &mut [Session], order: Sort) {
    records.sort_by(|a, b| {
        let score = if order == Sort::Relevance {
            b.match_score.total_cmp(&a.match_score)
        } else {
            std::cmp::Ordering::Equal
        };
        score
            .then_with(|| {
                if order == Sort::Created {
                    ms(&b.created_at).cmp(&ms(&a.created_at))
                } else {
                    ms(&b.last_reply_at).cmp(&ms(&a.last_reply_at))
                }
            })
            .then_with(|| ms(&b.created_at).cmp(&ms(&a.created_at)))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.provider.cmp(&b.provider))
    });
}

pub fn execute(request: &Request) -> Result<Output> {
    if request.action == Action::Relations {
        if request.options.provider != Providers::Codex {
            bail!("relations 当前只支持 --provider codex");
        }
        return crate::relations::execute(request);
    }
    let start = Instant::now();
    let options = &request.options;
    let home = options
        .home
        .clone()
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".codex"));
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    let home = fs::canonicalize(&home).unwrap_or(home);
    let mut warnings = Vec::new();
    let mut all_paths = Vec::new();
    if options.provider.codex() {
        catalog::walk(&home.join("sessions"), &mut all_paths, &mut warnings);
        catalog::walk(
            &home.join("archived_sessions"),
            &mut all_paths,
            &mut warnings,
        );
    }
    all_paths.sort();
    if options.provider.codex() && all_paths.is_empty() {
        warnings.push(format!("未找到 rollout 日志：{}", home.display()));
    }
    let database = if options.provider.codex() {
        catalog::database(&home, &mut warnings)
    } else {
        HashMap::new()
    };
    let titles = if options.provider.codex() {
        catalog::titles(&home, &mut warnings)
    } else {
        HashMap::new()
    };
    let cache = if !options.provider.codex() || options.no_cache || options.id.is_some() {
        None
    } else {
        match Cache::open(
            &options.cache_dir.clone().unwrap_or_else(cache::directory),
            &home,
        ) {
            Ok(c) => Some(c),
            Err(e) => {
                warnings.push(format!("缓存不可用，回退到日志：{e}"));
                None
            }
        }
    };
    let full = !request.query.is_empty() && request.action != Action::Projects;
    let metadata_mode = !full
        && options.id.is_none()
        && request.action != Action::Sync
        && request.action != Action::Projects
        && options.sort == Sort::Created;
    let mode = if metadata_mode {
        "metadata"
    } else if full {
        "full"
    } else {
        "summary"
    };
    let mut engine = Engine {
        request,
        database,
        titles,
        cache,
        writes: Vec::new(),
        loaded: HashMap::new(),
        messages: HashMap::new(),
        projects: HashMap::new(),
        warnings,
        stats: Stats {
            mode: mode.into(),
            total_files: all_paths.len(),
            ..Default::default()
        },
        pool: rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(2)
                    .min(4),
            )
            .build()?,
    };
    let mut by_id: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in &all_paths {
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(session_id)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        if options
            .id
            .as_deref()
            .is_some_and(|prefix| !id.starts_with(prefix))
        {
            continue;
        }
        by_id.entry(id).or_default().push(path.clone());
    }
    let mut candidates = Vec::new();
    let trae = if options.provider.trae() {
        let mut source_options = options.clone();
        if request.action == Action::Sync {
            source_options.refresh = true;
        }
        match crate::trae::Store::open(&source_options) {
            Ok(store) => Some(store),
            Err(error) if options.provider == Providers::All && !options.refresh => {
                engine.warnings.push(format!("TRAE 来源不可用：{error}"));
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    if let Some(store) = &trae {
        if request.action == Action::Sync {
            store.sync(options.id.as_deref())?;
        }
        let mut read_options = options.clone();
        if request.action == Action::Sync {
            read_options.refresh = true;
        }
        let loaded_records =
            store.records(&read_options, &mut engine.stats, &mut engine.warnings)?;
        if full && request.action == Action::Search {
            let queries_only = loaded_records
                .iter()
                .filter(|l| l.session.scope == crate::model::Scope::IndexedUserQueries)
                .count();
            let partial_records: Vec<_> = loaded_records
                .iter()
                .filter(|l| l.session.scope != crate::model::Scope::LocalConversation)
                .collect();
            let titled = partial_records
                .iter()
                .filter(|l| l.session.title_source.as_deref() == Some("metadata"))
                .count();
            if titled < partial_records.len() || queries_only > 0 {
                let title_coverage = if titled == 0 {
                    "TRAE 搜索未包含 IDE 会话标题".into()
                } else {
                    format!("TRAE 搜索包含 {titled} 个 IDE 会话标题")
                };
                engine.warnings.push(format!(
                    "{title_coverage}；{} 个会话中，{} 个仅有索引提问，{} 个包含已同步正文。{} 个会话缺少标题元数据，列表回退到首条提问或 ID",
                    partial_records.len(),
                    queries_only,
                    partial_records.len() - queries_only,
                    partial_records.len() - titled,
                ));
            }
            if queries_only > 0 {
                engine.warnings.push(if options.trae_index.is_some() {
                    "TRAE 离线导出未覆盖全部正文，未命中不表示会话不存在".into()
                } else {
                    "可在 TRAE Agent 环境运行 sync --provider trae 补充正文；--refresh 只更新提问索引，不补充 IDE 标题".into()
                });
            }
        }
        for loaded in loaded_records {
            candidates.push(engine.decorate(loaded.session.clone()));
            engine
                .loaded
                .insert((Provider::Trae, loaded.session.path.clone()), loaded);
        }
    }
    let traecli = if options.provider.traecli() {
        match crate::traecli::Store::open(options, &mut engine.warnings) {
            Ok(store) => Some(store),
            Err(error) if options.provider == Providers::All && !options.refresh => {
                engine
                    .warnings
                    .push(format!("TRAE CLI 来源不可用：{error}"));
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    if let Some(store) = &traecli {
        for loaded in store.records(options, &mut engine.stats, &mut engine.warnings)? {
            candidates.push(engine.decorate(loaded.session.clone()));
            engine
                .loaded
                .insert((Provider::Traecli, loaded.session.path.clone()), loaded);
        }
    }
    let mut headers = Vec::new();
    let mut conflicting_copies = Vec::new();
    for (id, paths) in by_id {
        if let Some(db) = engine.database.get(&id).cloned() {
            for path in paths {
                engine.stats.database_sessions += 1;
                let session = parser::metadata(&path, &serde_json::Value::Null, Some(&db))?;
                candidates.push(engine.decorate(session));
            }
        } else if paths.len() > 1 {
            // A stale archived copy can belong to a different project/source.
            // Resolve the preferred copy before global exact-project matching.
            conflicting_copies.extend(paths);
        } else {
            headers.extend(paths);
        }
    }
    candidates.extend(engine.load(&headers, true, false));
    candidates.extend(engine.load(&conflicting_copies, false, full));
    let candidates = unique(candidates);
    let candidates = project::filter(candidates, options.project.as_deref(), options.exact);
    let candidates = if request.action == Action::Projects {
        project::filter(candidates, Some(&request.query), options.exact)
    } else {
        candidates
    };
    let mut candidates: Vec<_> = candidates
        .into_iter()
        .filter(|s| options.include_subagents || options.id.is_some() || !s.is_subagent)
        .collect();
    engine.stats.candidate_sessions = candidates.len();
    engine.stats.candidate_files = candidates.iter().map(|s| s.paths.len()).sum();
    if options.sort == Sort::Created {
        candidates.retain(|s| in_time(s, request));
    }
    let mut records;
    let total;
    if metadata_mode {
        for s in &mut candidates {
            s.match_score = 1.0;
        }
        sort(&mut candidates, options.sort);
        total = candidates.len();
        engine.stats.indexed_sessions = total;
        records = Vec::new();
        let page: Vec<_> = candidates
            .into_iter()
            .skip(options.offset as usize)
            .take(options.limit as usize)
            .collect();
        let mut hydrated: HashMap<_, _> = unique(engine.hydrate(&page, false))
            .into_iter()
            .map(|s| ((s.provider, s.id.clone()), s))
            .collect();
        for mut candidate in page {
            if let Some(mut s) = hydrated.remove(&(candidate.provider, candidate.id.clone())) {
                s.paths = candidate.paths;
                s.match_score = 1.0;
                records.push(s);
            } else {
                candidate.details_available = false;
                records.push(candidate);
            }
        }
    } else {
        records = unique(engine.hydrate(&candidates, full));
        engine.stats.indexed_sessions = records.len();
        records.retain(|s| in_time(s, request));
        let query = normalize(&request.query);
        if full {
            records = engine.pool.install(|| {
                records
                    .into_par_iter()
                    .filter_map(|mut s| {
                        let fields = [
                            &s.title,
                            &s.first_user_preview,
                            &s.last_reply_preview,
                            &s.id,
                        ]
                        .map(|s| normalize(s));
                        let text = engine
                            .loaded
                            .get(&(s.provider, s.path.clone()))
                            .and_then(|l| l.search_text.as_deref())
                            .unwrap_or("");
                        let score = score_normalized(
                            &query,
                            &[&fields[0], &fields[1], &fields[2], &fields[3], text],
                            options.exact,
                        );
                        if score == 0.0 {
                            return None;
                        }
                        let title_score = score_normalized(&query, &[&fields[0]], options.exact);
                        s.match_score = ((score + title_score * 0.15) * 1000.0).round() / 1000.0;
                        Some(s)
                    })
                    .collect()
            });
        } else {
            for s in &mut records {
                s.match_score = 1.0;
            }
        }
        sort(&mut records, options.sort);
        total = records.len();
    }
    let mut groups = Vec::new();
    let mut messages = Vec::new();
    let mut transcript_file = None;
    let total = if request.action == Action::Projects {
        let mut map: BTreeMap<String, ProjectGroup> = BTreeMap::new();
        for s in &records {
            let group = map
                .entry(s.project.key.clone())
                .or_insert_with(|| ProjectGroup {
                    project: s.project.clone(),
                    session_count: 0,
                    latest_created_at: None,
                    latest_reply_at: None,
                    cwds: BTreeSet::new(),
                });
            group.session_count += 1;
            if ms(&s.created_at) > ms(&group.latest_created_at) {
                group.latest_created_at.clone_from(&s.created_at);
            }
            if ms(&s.last_reply_at) > ms(&group.latest_reply_at) {
                group.latest_reply_at.clone_from(&s.last_reply_at);
            }
            if let Some(cwd) = &s.cwd {
                group.cwds.insert(cwd.clone());
            }
        }
        groups = map.into_values().collect();
        groups.sort_by(|a, b| {
            let (a_time, b_time) = if options.sort == Sort::Created {
                (&a.latest_created_at, &b.latest_created_at)
            } else {
                (&a.latest_reply_at, &b.latest_reply_at)
            };
            ms(b_time)
                .cmp(&ms(a_time))
                .then_with(|| a.project.name.cmp(&b.project.name))
        });
        let total = groups.len();
        groups = groups
            .into_iter()
            .skip(options.offset as usize)
            .take(options.limit as usize)
            .collect();
        records.clear();
        total
    } else if request.action == Action::Show {
        if records.len() != 1 {
            if records.is_empty() {
                bail!("没有找到本地会话：{}", options.id.as_deref().unwrap_or(""));
            }
            bail!(
                "ID 前缀匹配 {} 个会话，请指定完整 ID 或 --provider：\n{}",
                records.len(),
                records
                    .iter()
                    .take(10)
                    .map(|s| format!("{}:{}", s.provider.name(), s.id))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        if records[0].provider == Provider::Trae {
            let parsed = trae
                .as_ref()
                .unwrap()
                .show(&records[0].id, options.refresh)?;
            records[0] = engine.decorate(parsed.session);
            messages = parsed.messages;
            transcript_file = Some(
                crate::transcript::materialize(
                    &trae.as_ref().unwrap().source_identity(),
                    &records[0],
                    &messages,
                )
                .context("无法生成 TRAE 临时对话文件")?,
            );
        } else if records[0].provider == Provider::Traecli {
            let parsed = traecli.as_ref().unwrap().show(&records[0].path)?;
            records[0] = engine.decorate(parsed.session);
            messages = parsed.messages;
        } else {
            messages = engine.messages.remove(&records[0].path).unwrap_or_default();
        }
        1
    } else if request.action == Action::Sync {
        total
    } else {
        if !metadata_mode {
            records = records
                .into_iter()
                .skip(options.offset as usize)
                .take(options.limit as usize)
                .collect();
        }
        total
    };
    if full && request.action == Action::Search {
        for session in &mut records {
            let messages = engine
                .loaded
                .get(&(session.provider, session.path.clone()))
                .map(|l| l.search_messages.as_slice())
                .unwrap_or_default();
            session.matches =
                crate::snippets::for_session(session, messages, &request.query, options.exact);
        }
    }
    if let Some(cache) = &mut engine.cache
        && let Err(e) = cache.save(&engine.writes, &all_paths)
    {
        engine
            .warnings
            .push(format!("无法保存索引缓存，结果仍可用：{e}"));
    }
    engine.stats.elapsed_ms = start.elapsed().as_millis();
    engine.warnings.sort();
    engine.warnings.dedup();
    Ok(Output {
        home: if options.provider == Providers::Traecli {
            traecli.as_ref().unwrap().home.clone()
        } else {
            home
        },
        warnings: engine.warnings,
        stats: engine.stats,
        total,
        sessions: records,
        projects: groups,
        messages,
        transcript_file,
        relations: None,
    })
}
