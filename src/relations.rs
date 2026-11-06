use crate::{
    cache::{self, Cache},
    catalog,
    cli::{Direction, Request},
    model::{Stats, parent_links, source_name},
    query::Output,
    text::session_id,
    time::{iso, measure, union_ms},
};
use anyhow::{Result, bail};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub id: String,
    pub line: usize,
    pub created_at: Option<String>,
    pub cwd: Option<String>,
    pub source: String,
    pub thread_source: Option<String>,
    pub parents: Vec<(String, String)>,
    pub forks: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HeaderScan {
    pub own: Option<Header>,
    pub malformed_lines: usize,
    pub incomplete_tail: bool,
}

fn uuid(value: &Value) -> Option<String> {
    value
        .as_str()
        .and_then(session_id)
        .filter(|id| id.len() == 36)
}

fn file_id(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(session_id)
        .filter(|id| id.len() == 36)
}

/// Stop at the first header belonging to this file's session. Fork logs may
/// contain inherited metadata first; never attribute that metadata to the child.
/// Only this small projection is cached, never instructions or message bodies.
pub fn read_header(path: &Path) -> Result<HeaderScan> {
    let file = File::open(path)?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::with_capacity(16 * 1024, input);
    let expected = file_id(path);
    let mut scan = HeaderScan::default();
    let mut line = Vec::new();
    let mut number = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(scan);
        }
        number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                if line.last() == Some(&b'\n') {
                    scan.malformed_lines += 1;
                } else {
                    scan.incomplete_tail = true;
                }
                continue;
            }
        };
        if record["type"] != "session_meta" {
            continue;
        }
        let meta = &record["payload"];
        let Some(id) = meta
            .get("id")
            .or_else(|| meta.get("session_id"))
            .and_then(uuid)
        else {
            continue;
        };
        if expected.as_ref().is_some_and(|e| *e != id) {
            continue;
        }
        let mut parents: Vec<_> = parent_links(&meta["source"])
            .into_iter()
            .map(|(id, field)| (id, format!("/payload/source{field}")))
            .collect();
        for field in ["parent_thread_id", "parentThreadId"] {
            if let Some(parent) = uuid(&meta[field]) {
                parents.push((parent, format!("/payload/{field}")));
            }
        }
        let mut forks = Vec::new();
        if let Some(parent) = uuid(&meta["forked_from_id"]) {
            forks.push((parent, "/payload/forked_from_id".into()));
        }
        scan.own = Some(Header {
            id,
            line: number,
            created_at: iso(&meta["timestamp"]).or_else(|| iso(&record["timestamp"])),
            cwd: meta["cwd"].as_str().map(str::to_owned),
            source: source_name(&meta["source"]),
            thread_source: meta["thread_source"].as_str().map(str::to_owned),
            parents,
            forks,
        });
        return Ok(scan);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Fork,
    Spawn,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fork => "fork",
            Self::Spawn => "spawn",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub kind: String,
    pub path: String,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: Kind,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub title: String,
    pub created_at: Option<String>,
    pub cwd: Option<String>,
    pub source: String,
    pub thread_source: Option<String>,
    pub paths: Vec<String>,
    pub native_url: String,
    pub in_database: bool,
    pub log_status: String,
    pub metadata_available: bool,
    pub parent_status: String,
}
impl Node {
    fn new(id: String) -> Self {
        Self {
            native_url: format!("codex://threads/{id}"),
            title: id.clone(),
            id,
            created_at: None,
            cwd: None,
            source: "unknown".into(),
            thread_source: None,
            paths: Vec::new(),
            in_database: false,
            log_status: "missing".into(),
            metadata_available: false,
            parent_status: "unknown".into(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub child_id: String,
    pub kind: Kind,
    pub selected_parent_id: Option<String>,
    pub candidates: Vec<Edge>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frontier {
    pub id: String,
    pub direction: Direction,
    pub omitted_edges: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub database_path: Option<PathBuf>,
    pub spawn_table_available: bool,
    pub logs_without_own_metadata: Vec<String>,
    pub unreadable_logs: Vec<String>,
    pub tools_scanned: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub root_id: String,
    pub depth: u32,
    pub direction: Direction,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub conflicts: Vec<Conflict>,
    pub truncated: bool,
    pub frontier: Vec<Frontier>,
    pub coverage: Coverage,
}

// Group by child and relation kind, then candidate parent.
type Claims = BTreeMap<(String, Kind), BTreeMap<String, (u8, Vec<Evidence>)>>;

fn claim(
    claims: &mut Claims,
    parent: String,
    child: String,
    kind: Kind,
    rank: u8,
    evidence: Evidence,
    warnings: &mut Vec<String>,
) {
    if parent == child {
        warnings.push(format!(
            "Ignoring self-relationship: {child} ({}) from {}",
            kind.name(),
            evidence.path
        ));
        return;
    }
    let (priority, proofs) = claims
        .entry((child, kind))
        .or_default()
        .entry(parent)
        .or_default();
    *priority = (*priority).max(rank);
    proofs.push(evidence);
}

fn spawn_edges(path: &Path, claims: &mut Claims, warnings: &mut Vec<String>) -> Result<bool> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(500))?;
    let columns: HashSet<String> = conn
        .prepare("PRAGMA table_info(thread_spawn_edges)")?
        .query_map([], |r| r.get(1))?
        .collect::<Result<_, _>>()?;
    if columns.is_empty() {
        return Ok(false);
    }
    if !columns.contains("parent_thread_id") || !columns.contains("child_thread_id") {
        bail!("Incompatible thread_spawn_edges table schema");
    }
    let status = if columns.contains("status") {
        "status"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT parent_thread_id,child_thread_id,{status} FROM thread_spawn_edges"
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (parent, child, status) in rows {
        let (Some(parent), Some(child)) =
            (uuid(&Value::String(parent)), uuid(&Value::String(child)))
        else {
            warnings.push("thread_spawn_edges contains an invalid UUID; row skipped".into());
            continue;
        };
        claim(
            claims,
            parent,
            child,
            Kind::Spawn,
            3,
            Evidence {
                kind: "database".into(),
                path: path.to_string_lossy().into_owned(),
                table: Some("thread_spawn_edges".into()),
                field: "parent_thread_id -> child_thread_id".into(),
                line: None,
                status,
            },
            warnings,
        );
    }
    Ok(true)
}

fn resolve(claims: Claims) -> (Vec<Edge>, Vec<Conflict>) {
    let mut edges = Vec::new();
    let mut conflicts = Vec::new();
    for ((child, kind), parents) in claims {
        let highest = parents.values().map(|(rank, _)| *rank).max().unwrap();
        let winners: Vec<_> = parents
            .iter()
            .filter(|(_, (rank, _))| *rank == highest)
            .map(|(id, _)| id.clone())
            .collect();
        let selected = (winners.len() == 1).then(|| winners[0].clone());
        let candidates: Vec<_> = parents
            .into_iter()
            .map(|(parent, (_, mut evidence))| {
                evidence.sort();
                evidence.dedup();
                Edge {
                    id: format!("{}:{parent}:{child}", kind.name()),
                    from: parent,
                    to: child.clone(),
                    kind,
                    evidence,
                }
            })
            .collect();
        if let Some(winner) = candidates
            .iter()
            .find(|e| Some(&e.from) == selected.as_ref())
        {
            edges.push(winner.clone());
        }
        if candidates.len() > 1 {
            conflicts.push(Conflict {
                child_id: child,
                kind,
                selected_parent_id: selected,
                candidates,
            });
        }
    }
    (edges, conflicts)
}

type Boundary = Vec<(String, Direction, Vec<usize>)>;

fn traverse(
    root: &str,
    edges: &[Edge],
    direction: Direction,
    depth: u32,
) -> (BTreeSet<usize>, Boundary) {
    let mut adjacency: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, edge) in edges.iter().enumerate() {
        let key = if direction == Direction::Parents {
            &edge.to
        } else {
            &edge.from
        };
        adjacency.entry(key).or_default().push(index);
    }
    let mut selected = BTreeSet::new();
    let mut visited = BTreeSet::from([root.to_owned()]);
    let mut queue = VecDeque::from([(root.to_owned(), 0)]);
    let mut frontier = Vec::new();
    while let Some((id, distance)) = queue.pop_front() {
        let Some(indices) = adjacency.get(id.as_str()) else {
            continue;
        };
        if distance == depth {
            frontier.push((id, direction, indices.clone()));
            continue;
        }
        for &index in indices {
            selected.insert(index);
            let edge = &edges[index];
            let next = if direction == Direction::Parents {
                &edge.from
            } else {
                &edge.to
            };
            if visited.insert(next.clone()) {
                queue.push_back((next.clone(), distance + 1));
            }
        }
    }
    (selected, frontier)
}

pub fn execute(request: &Request) -> Result<Output> {
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
    let mut paths = Vec::new();
    catalog::walk(&home.join("sessions"), &mut paths, &mut warnings);
    catalog::walk(&home.join("archived_sessions"), &mut paths, &mut warnings);
    paths.sort();
    let database = catalog::database_catalog(&home, &mut warnings);
    let titles = catalog::titles(&home, &mut warnings);
    let mut stats = Stats {
        mode: "relations".into(),
        total_files: paths.len(),
        database_sessions: database.threads.len(),
        metadata_read_ms: start.elapsed().as_secs_f64() * 1000.0,
        ..Default::default()
    };
    let mut coverage = Coverage {
        database_path: database.path.clone(),
        ..Default::default()
    };
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut claims = Claims::new();
    for (id, db) in &database.threads {
        let mut node = Node::new(id.clone());
        node.created_at = Some(db.created_at.clone());
        node.cwd.clone_from(&db.cwd);
        node.source = source_name(&db.source);
        node.thread_source.clone_from(&db.thread_source);
        node.title = db.title.clone().unwrap_or_else(|| id.clone());
        node.in_database = true;
        node.metadata_available = true;
        nodes.insert(id.clone(), node);
        for (parent, pointer) in parent_links(&db.source) {
            claim(
                &mut claims,
                parent,
                id.clone(),
                Kind::Spawn,
                2,
                Evidence {
                    kind: "database".into(),
                    path: database
                        .path
                        .as_ref()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    field: format!("source{pointer}"),
                    table: Some("threads".into()),
                    line: None,
                    status: None,
                },
                &mut warnings,
            );
        }
    }
    if let Some(path) = &database.path {
        match measure(&mut stats.metadata_read_ms, || {
            spawn_edges(path, &mut claims, &mut warnings)
        }) {
            Ok(available) => coverage.spawn_table_available = available,
            Err(e) => warnings.push(format!(
                "Sub-session relationship table unavailable; reading source metadata and logs: {e}"
            )),
        }
    }
    let mut cache = if options.no_cache {
        None
    } else {
        match measure(&mut stats.cache_read_ms, || {
            Cache::open(
                &options.cache_dir.clone().unwrap_or_else(cache::directory),
                &home,
            )
        }) {
            Ok(c) => Some(c),
            Err(e) => {
                warnings.push(format!("Cache unavailable; reading logs: {e}"));
                None
            }
        }
    };
    let mut loaded = Vec::new();
    let mut pending = Vec::new();
    for path in &paths {
        if let Some(id) = file_id(path) {
            let node = nodes.entry(id.clone()).or_insert_with(|| Node::new(id));
            node.paths.push(path.to_string_lossy().into_owned());
            // Available means the metadata prefix was readable, not that the whole body was validated.
            if node.log_status == "missing" {
                node.log_status = "unreadable".into();
            }
        }
        let signature = match cache::fingerprint(path, None) {
            Ok(s) => s,
            Err(e) => {
                coverage
                    .unreadable_logs
                    .push(path.to_string_lossy().into_owned());
                warnings.push(format!("Cannot read {}: {e}", path.display()));
                continue;
            }
        };
        let key = path.to_string_lossy().into_owned();
        if !options.refresh
            && let Some(cache) = &cache
        {
            match measure(&mut stats.cache_read_ms, || {
                cache.read_relation_header(&key, &signature)
            }) {
                Ok(Some(scan)) => {
                    stats.cached_files += 1;
                    loaded.push((key, scan));
                    continue;
                }
                Ok(None) => {}
                Err(e) => warnings.push(format!(
                    "Relationship cache record unavailable; reading logs: {e}"
                )),
            }
        }
        pending.push((path, signature));
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(2)
                .min(4),
        )
        .build()?;
    let parsed: Vec<_> = pool.install(|| {
        pending
            .par_iter()
            .map(|(path, signature)| {
                let start = Instant::now();
                let scan = read_header(path);
                let stable = cache::fingerprint(path, None).ok().as_ref() == Some(signature);
                (*path, signature, scan, stable, (start, Instant::now()))
            })
            .collect()
    });
    stats.metadata_read_ms += union_ms(parsed.iter().map(|record| record.4).collect());
    let mut writes = Vec::new();
    for (path, signature, scan, stable, _) in parsed {
        stats.header_files += 1;
        let key = path.to_string_lossy().into_owned();
        match scan {
            Ok(scan) => {
                if !stable {
                    warnings.push(format!(
                        "Log is being written; relationships reflect this read snapshot: {}",
                        path.display()
                    ));
                } else if cache.is_some() {
                    writes.push((key.clone(), signature.clone(), scan.clone()));
                }
                loaded.push((key, scan));
            }
            Err(e) => {
                coverage.unreadable_logs.push(key);
                warnings.push(format!(
                    "Cannot read relationship header {}: {e}",
                    path.display()
                ));
            }
        }
    }
    loaded.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, scan) in loaded {
        let id = scan
            .own
            .as_ref()
            .map(|h| h.id.clone())
            .or_else(|| file_id(Path::new(&path)));
        if let Some(id) = id {
            let node = nodes.entry(id.clone()).or_insert_with(|| Node::new(id));
            if !node.paths.contains(&path) {
                node.paths.push(path.clone());
            }
            node.log_status = "available".into();
        }
        if scan.malformed_lines > 0 || scan.incomplete_tail {
            warnings.push(format!(
                "{}: {} invalid JSON lines before relationship header, incomplete trailing record={}",
                path, scan.malformed_lines, scan.incomplete_tail
            ));
        }
        let Some(header) = scan.own else {
            coverage.logs_without_own_metadata.push(path);
            continue;
        };
        let node = nodes.get_mut(&header.id).unwrap();
        if !node.metadata_available {
            node.created_at.clone_from(&header.created_at);
            node.cwd.clone_from(&header.cwd);
            node.source.clone_from(&header.source);
        }
        if node.thread_source.is_none() {
            node.thread_source.clone_from(&header.thread_source);
        }
        node.metadata_available = true;
        for (kind, links) in [(Kind::Spawn, header.parents), (Kind::Fork, header.forks)] {
            for (parent, field) in links {
                claim(
                    &mut claims,
                    parent,
                    header.id.clone(),
                    kind,
                    1,
                    Evidence {
                        kind: "session_meta".into(),
                        path: path.clone(),
                        field,
                        table: None,
                        line: Some(header.line),
                        status: None,
                    },
                    &mut warnings,
                );
            }
        }
    }
    if let Some(cache) = &mut cache
        && let Err(e) = cache.save_relation_headers(&writes, &paths)
    {
        warnings.push(format!(
            "Cannot save relationship cache; results remain available: {e}"
        ));
    }
    if !coverage.logs_without_own_metadata.is_empty() {
        warnings.push(format!(
            "{} logs lack their own session_meta; relationship coverage may be incomplete; see coverage",
            coverage.logs_without_own_metadata.len()
        ));
    }
    // Keep referenced nodes even when both their log and threads row are absent.
    for ((child, _), parents) in &claims {
        for id in std::iter::once(child).chain(parents.keys()) {
            nodes
                .entry(id.clone())
                .or_insert_with(|| Node::new(id.clone()));
        }
    }
    let (edges, conflicts) = resolve(claims);
    let parented: BTreeSet<_> = edges.iter().map(|e| e.to.as_str()).collect();
    let conflicted: BTreeSet<_> = conflicts.iter().map(|c| c.child_id.as_str()).collect();
    for node in nodes.values_mut() {
        if let Some(title) = titles.get(&node.id) {
            node.title.clone_from(title);
        }
        node.paths.sort();
        node.paths.dedup();
        node.parent_status = if conflicted.contains(node.id.as_str()) {
            "conflicting"
        } else if parented.contains(node.id.as_str()) {
            "linked"
        } else if !node.metadata_available
            || matches!(
                node.source.as_str(),
                "unknown" | "subagent" | "agent_created_thread"
            )
            || node
                .thread_source
                .as_deref()
                .is_some_and(|s| matches!(s, "subagent" | "agent_created_thread"))
        {
            "unknown"
        } else {
            "not_recorded"
        }
        .into();
    }
    let prefix = options
        .id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("relations requires a session ID"))?;
    let matches: Vec<_> = nodes
        .keys()
        .filter(|id| id.starts_with(prefix))
        .cloned()
        .collect();
    if matches.is_empty() {
        bail!("No local session or relationship record found: {prefix}");
    }
    if matches.len() != 1 {
        bail!(
            "ID prefix matches {} sessions; specify a full ID:\n{}",
            matches.len(),
            matches
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let root_id = matches[0].clone();
    let mut selected = BTreeSet::new();
    let mut boundary = Vec::new();
    for direction in [Direction::Parents, Direction::Children] {
        if request.direction == Direction::Both || request.direction == direction {
            let (indices, frontier) = traverse(&root_id, &edges, direction, request.depth);
            selected.extend(indices);
            boundary.extend(frontier);
        }
    }
    let frontier: Vec<_> = boundary
        .into_iter()
        .filter_map(|(id, direction, indices)| {
            let omitted_edges = indices.iter().filter(|i| !selected.contains(i)).count();
            (omitted_edges > 0).then_some(Frontier {
                id,
                direction,
                omitted_edges,
            })
        })
        .collect();
    let edges: Vec<_> = edges
        .into_iter()
        .enumerate()
        .filter(|(i, _)| selected.contains(i))
        .map(|(_, e)| e)
        .collect();
    let mut selected_nodes = BTreeSet::from([root_id.clone()]);
    for edge in &edges {
        selected_nodes.extend([edge.from.clone(), edge.to.clone()]);
    }
    let conflicts: Vec<_> = conflicts
        .into_iter()
        .filter(|c| {
            selected_nodes.contains(&c.child_id)
                || c.candidates
                    .iter()
                    .any(|e| selected_nodes.contains(&e.from))
        })
        .collect();
    if !conflicts.is_empty() {
        warnings.push(format!("{} parent relationship conflicts: relationship tables take priority over database source metadata, then logs; conflicts of equal priority produce no edge; see conflicts", conflicts.len()));
    }
    let nodes: Vec<_> = nodes
        .into_values()
        .filter(|n| selected_nodes.contains(&n.id))
        .collect();
    stats.candidate_sessions = nodes.len();
    stats.candidate_files = nodes.iter().map(|n| n.paths.len()).sum();
    stats.indexed_sessions = nodes.len();
    stats.finish(start);
    warnings.sort();
    warnings.dedup();
    coverage.unreadable_logs.sort();
    coverage.logs_without_own_metadata.sort();
    let total = nodes.len();
    let graph = Graph {
        root_id,
        depth: request.depth,
        direction: request.direction,
        nodes,
        edges,
        conflicts,
        truncated: !frontier.is_empty(),
        frontier,
        coverage,
    };
    Ok(Output {
        home,
        warnings,
        stats,
        total,
        sessions: Vec::new(),
        projects: Vec::new(),
        messages: Vec::new(),
        transcript_file: None,
        relations: Some(graph),
    })
}
