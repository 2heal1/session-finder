use crate::{
    model::{Project, Session},
    text::{normalize, score},
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

pub fn repository(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (host, path) = if !raw.contains("://") {
        let (_, rest) = raw.split_once('@')?;
        let (host, path) = rest.split_once(':')?;
        (host.to_ascii_lowercase(), path.to_owned())
    } else {
        let url = url::Url::parse(raw).ok()?;
        if !matches!(url.scheme(), "https" | "http" | "ssh" | "git") {
            return None;
        }
        let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
        (
            format!("{}{port}", url.host_str()?.to_ascii_lowercase()),
            url.path().trim_start_matches('/').to_owned(),
        )
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    Some(format!("{host}/{path}"))
}

fn absolute_from(base: &Path, value: &str) -> PathBuf {
    let path = base.join(value);
    fs::canonicalize(&path).unwrap_or(path)
}

pub fn resolve(session: &Session, memo: &mut HashMap<String, Project>) -> Project {
    let repository = session.repository_url.as_deref().and_then(repository);
    let key = format!(
        "{}\0{}",
        session.cwd.as_deref().unwrap_or(""),
        repository.as_deref().unwrap_or("")
    );
    if let Some(project) = memo.get(&key) {
        return project.clone();
    }
    let mut root = None;
    if let Some(cwd) = session
        .cwd
        .as_deref()
        .map(Path::new)
        .filter(|p| p.is_absolute())
    {
        for directory in cwd.ancestors() {
            let dotgit = directory.join(".git");
            if dotgit.is_dir() {
                root = Some(directory.to_path_buf());
                break;
            }
            if dotgit.is_file()
                && let Ok(text) = fs::read_to_string(&dotgit)
                && let Some(gitdir) = text
                    .lines()
                    .find_map(|l| l.strip_prefix("gitdir:").map(str::trim))
            {
                let location = absolute_from(directory, gitdir);
                root = Some(directory.to_path_buf());
                if let Ok(common) = fs::read_to_string(location.join("commondir")) {
                    let shared = absolute_from(&location, common.trim());
                    if shared.file_name().is_some_and(|n| n == ".git") {
                        root = shared.parent().map(Path::to_path_buf);
                    }
                }
                break;
            }
        }
    }
    let name = root
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| {
            repository
                .as_deref()
                .and_then(|s| s.rsplit('/').next())
                .map(str::to_owned)
        })
        .or_else(|| {
            session
                .cwd
                .as_deref()
                .and_then(|p| Path::new(p).file_name())
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "(no project)".into());
    let root = root
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| session.cwd.clone());
    let project = Project {
        key: repository
            .clone()
            .or_else(|| root.clone())
            .unwrap_or_else(|| "(unknown)".into()),
        name,
        root,
        repository,
    };
    memo.insert(key, project.clone());
    project
}

/// Match against the whole catalog before hiding subagents or applying dates.
pub fn filter(records: Vec<Session>, query: Option<&str>, exact: bool) -> Vec<Session> {
    let Some(query) = query.filter(|s| !s.is_empty()) else {
        return records;
    };
    let value = repository(query).unwrap_or_else(|| query.to_owned());
    let name = normalize(&value);
    let path = if let Some(rest) = value.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_default()
            .join(rest)
            .to_string_lossy()
            .into_owned()
    } else {
        value.trim_end_matches('/').to_owned()
    };
    let direct = |r: &&Session| {
        normalize(&r.project.name) == name
            || normalize(&r.project.key) == name
            || r.cwd.as_deref() == Some(&path)
            || r.project.root.as_deref() == Some(&path)
    };
    if records.iter().any(|r| direct(&r)) {
        return records.into_iter().filter(|r| direct(&r)).collect();
    }
    if records
        .iter()
        .any(|r| score(&value, &[&r.project.name], exact) > 0.0)
    {
        return records
            .into_iter()
            .filter(|r| score(&value, &[&r.project.name], exact) > 0.0)
            .collect();
    }
    records
        .into_iter()
        .filter(|r| {
            score(
                &value,
                &[
                    &r.project.key,
                    r.project.root.as_deref().unwrap_or(""),
                    r.cwd.as_deref().unwrap_or(""),
                ],
                exact,
            ) > 0.0
        })
        .collect()
}
