use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Codex,
    Trae,
    Traecli,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Trae => "trae",
            Self::Traecli => "traecli",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    LocalConversation,
    IndexedUserQueries,
    IndexedConversation,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub key: String,
    pub name: String,
    pub root: Option<String>,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_source: Option<String>,
    pub id: String,
    pub path: String,
    pub paths: Vec<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    pub created_at: Option<String>,
    pub last_reply_at: Option<String>,
    pub last_user_at: Option<String>,
    pub cwd: Option<String>,
    pub source: String,
    pub is_subagent: bool,
    pub parent_session_id: Option<String>,
    pub forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub project: Project,
    pub native_url: String,
    pub compressed: bool,
    pub archived: bool,
    pub first_user_preview: String,
    pub last_user_preview: String,
    pub last_reply_preview: String,
    pub message_count: Option<usize>,
    pub user_message_count: Option<usize>,
    pub reply_count: Option<usize>,
    pub malformed_lines: usize,
    pub incomplete_tail: bool,
    pub match_score: f64,
    pub details_available: bool,
    #[serde(skip)]
    pub matches: Vec<SearchHit>,
}

impl Session {
    /// External application navigation, distinct from a source ContextFS URI.
    /// Keep source locators and the existing JSON nativeUrl contract intact.
    pub fn open_url(&self) -> Option<String> {
        let id = crate::text::session_id(&self.id)?;
        if id != self.id.to_ascii_lowercase() {
            return None;
        }
        match self.provider {
            Provider::Codex if id.len() == 36 => Some(format!("codex://threads/{id}")),
            Provider::Trae
                if id.len() == 24 && self.index_source.as_deref() == Some("native_database") =>
            {
                let cwd = self.cwd.as_deref()?;
                if !std::path::Path::new(cwd).is_absolute() || crate::text::safe(cwd, false) != cwd
                {
                    return None;
                }
                let mut url =
                    url::Url::parse("trae-cn://session-finder.trae-session-link/open").ok()?;
                url.query_pairs_mut()
                    .append_pair("id", &id)
                    .append_pair("cwd", cwd);
                Some(url.into())
            }
            _ => None,
        }
    }

    pub fn public(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("session is serializable");
        value.as_object_mut().unwrap().remove("repositoryUrl");
        value
    }

    pub fn list_json(&self, verbose: bool, search: bool) -> serde_json::Value {
        let mut value = if verbose {
            self.public()
        } else {
            serde_json::json!({
                "id": self.id,
                "provider": self.provider,
                "title": crate::text::printable(&self.title, 160),
                "project": {"key": self.project.key, "name": self.project.name},
                "createdAt": self.created_at,
                "lastReplyAt": self.last_reply_at,
                "nativeUrl": self.native_url,
                "scope": self.scope,
                "detailsAvailable": self.details_available,
                "isSubagent": self.is_subagent,
            })
        };
        if search {
            value["matchScore"] = self.match_score.into();
            value["matches"] = serde_json::to_value(&self.matches).unwrap();
        } else if !verbose {
            let preview = if self.last_reply_preview.is_empty() {
                &self.last_user_preview
            } else {
                &self.last_reply_preview
            };
            value["preview"] = crate::text::printable(preview, 160).into();
        }
        value
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub phase: Option<String>,
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip)]
    pub format: String,
    #[serde(skip)]
    pub line: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub mode: String,
    pub total_files: usize,
    pub candidate_sessions: usize,
    pub candidate_files: usize,
    pub parsed_files: usize,
    pub cached_files: usize,
    pub header_files: usize,
    pub database_sessions: usize,
    pub indexed_sessions: usize,
    pub elapsed_ms: u128,
    pub body_read_ms: f64,
    pub metadata_read_ms: f64,
    pub cache_read_ms: f64,
    pub other_ms: f64,
}

impl Stats {
    pub(crate) fn finish(&mut self, start: std::time::Instant) {
        let elapsed = start.elapsed();
        self.elapsed_ms = elapsed.as_millis();
        self.other_ms = (elapsed.as_secs_f64() * 1000.0
            - self.body_read_ms
            - self.metadata_read_ms
            - self.cache_read_ms)
            .max(0.0);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DbMeta {
    pub created_at: String,
    pub cwd: Option<String>,
    pub source: serde_json::Value,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub thread_source: Option<String>,
    pub name: Option<String>,
    pub archived: Option<bool>,
}

impl DbMeta {
    pub fn apply(&self, session: &mut Session) {
        session.created_at = Some(self.created_at.clone());
        session.cwd.clone_from(&self.cwd);
        session.source = source_name(&self.source);
        session.is_subagent = session.source == "subagent";
        if let Some((parent, _)) = parent_links(&self.source).first() {
            session.parent_session_id = Some(parent.clone());
        }
        session.repository_url.clone_from(&self.repository_url);
        session.branch.clone_from(&self.branch);
    }
}

/// Only structured source fields identify a caller; UUID mentions in text do not.
pub fn parent_links(source: &serde_json::Value) -> Vec<(String, String)> {
    let mut links = Vec::new();
    for base in ["/subagent/thread_spawn", "/subagent/spawn", "/subagent"] {
        for field in ["parent_thread_id", "parentThreadId"] {
            let pointer = format!("{base}/{field}");
            if let Some(id) = source
                .pointer(&pointer)
                .and_then(|v| v.as_str())
                .and_then(crate::text::session_id)
                .filter(|id| id.len() == 36)
            {
                links.push((id, pointer));
            }
        }
    }
    links
}

pub fn source_name(value: &serde_json::Value) -> String {
    if value.get("subagent").is_some() {
        "subagent".into()
    } else {
        value.as_str().unwrap_or("unknown").to_string()
    }
}

pub struct Parsed {
    pub session: Session,
    pub search_text: Option<String>,
    pub search_messages: Vec<SearchMessage>,
    pub messages: Vec<Message>,
}
