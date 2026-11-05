use crate::{
    model::DbMeta,
    text::session_id,
    time::{iso, millis_iso, ms},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

pub fn walk(directory: &Path, paths: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    let entries = match fs::read_dir(directory) {
        Ok(v) => v,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warnings.push(format!("无法读取 {}: {e}", directory.display()));
            }
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("目录读取失败：{e}"));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&entry.path(), paths, warnings),
            Ok(kind) if kind.is_file() => {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rollout-")
                    && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
                {
                    paths.push(entry.path());
                }
            }
            Err(e) => warnings.push(format!("无法读取 {}: {e}", entry.path().display())),
            _ => {}
        }
    }
}

pub fn titles(home: &Path, warnings: &mut Vec<String>) -> HashMap<String, String> {
    let path = home.join("session_index.jsonl");
    let mut titles: HashMap<String, (String, Option<String>)> = HashMap::new();
    match fs::File::open(&path) {
        Ok(file) => {
            for line in BufReader::new(file).lines() {
                let line = match line {
                    Ok(v) => v,
                    Err(e) => {
                        warnings.push(format!("标题索引读取失败：{e}"));
                        break;
                    }
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value["id"].as_str().and_then(session_id) else {
                    continue;
                };
                let Some(title) = value["thread_name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                else {
                    continue;
                };
                let updated = iso(&value["updated_at"]);
                if titles
                    .get(&id)
                    .is_none_or(|(_, old)| ms(&updated) >= ms(old))
                {
                    titles.insert(id, (title.to_owned(), updated));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warnings.push(format!("标题索引不可读：{e}")),
    }
    titles
        .into_iter()
        .map(|(id, (title, _))| (id, title))
        .collect()
}

fn database_file(path: &Path) -> anyhow::Result<HashMap<String, DbMeta>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(500))?;
    let columns: HashSet<String> = conn
        .prepare("PRAGMA table_info(threads)")?
        .query_map([], |r| r.get(1))?
        .collect::<Result<_, _>>()?;
    let required = ["id", "cwd", "source", "git_origin_url", "git_branch"];
    if required.iter().any(|c| !columns.contains(*c))
        || (!columns.contains("created_at") && !columns.contains("created_at_ms"))
    {
        anyhow::bail!("threads 表结构不兼容");
    }
    let created = match (
        columns.contains("created_at_ms"),
        columns.contains("created_at"),
    ) {
        (true, true) => "COALESCE(created_at_ms, created_at * 1000)",
        (true, false) => "created_at_ms",
        _ => "created_at * 1000",
    };
    let title = if columns.contains("title") {
        "title"
    } else {
        "NULL"
    };
    let thread_source = if columns.contains("thread_source") {
        "thread_source"
    } else {
        "NULL"
    };
    let name = if columns.contains("name") {
        "name"
    } else {
        "NULL"
    };
    let archived = if columns.contains("archived") {
        "archived"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT id,cwd,source,git_origin_url,git_branch,{created},{title},{thread_source},{name},{archived} FROM threads"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<bool>>(9)?,
        ))
    })?;
    let mut records = HashMap::new();
    for row in rows {
        let (
            id,
            cwd,
            source,
            repository_url,
            branch,
            created_at,
            title,
            thread_source,
            name,
            archived,
        ) = row?;
        let Some(id) = session_id(&id).filter(|id| id.len() == 36) else {
            continue;
        };
        let Some(created_at) = created_at.and_then(millis_iso) else {
            continue;
        };
        let source = source
            .map(|s| serde_json::from_str(&s).unwrap_or(Value::String(s)))
            .unwrap_or(Value::Null);
        records.insert(
            id,
            DbMeta {
                created_at,
                cwd,
                source,
                repository_url,
                branch,
                title: title.filter(|s| !s.trim().is_empty()),
                thread_source,
                name: name.filter(|s| !s.trim().is_empty()),
                archived,
            },
        );
    }
    Ok(records)
}

pub fn database(home: &Path, warnings: &mut Vec<String>) -> HashMap<String, DbMeta> {
    database_catalog(home, warnings).threads
}

#[derive(Default)]
pub struct Database {
    pub path: Option<PathBuf>,
    pub threads: HashMap<String, DbMeta>,
}

pub fn database_catalog(home: &Path, warnings: &mut Vec<String>) -> Database {
    let mut paths: Vec<_> = fs::read_dir(home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((version, e.path()))
        })
        .collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.0));
    let mut failure = None;
    for (_, path) in paths {
        match database_file(&path) {
            Ok(threads) => {
                return Database {
                    path: Some(path),
                    threads,
                };
            }
            Err(e) => failure = Some(e),
        }
    }
    if let Some(e) = failure {
        warnings.push(format!(
            "{} 元数据不可用，已回退到日志：{e}",
            home.display()
        ));
    }
    Database::default()
}
