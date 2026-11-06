use clap::Parser;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use session_finder::{
    cli::{Cli, Request},
    parser, project,
    query::{Output, execute},
    text, time,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const A: &str = "01000000-0000-4000-8000-000000000001";
const B: &str = "01000000-0000-4000-8000-000000000002";
const C: &str = "02000000-0000-4000-8000-000000000003";

fn meta(id: &str, date: &str, cwd: &str) -> Value {
    json!({"type":"session_meta","timestamp":date,"payload":{"id":id,"timestamp":date,"cwd":cwd,"source":"vscode"}})
}
fn msg(role: &str, text: &str, date: &str) -> Value {
    json!({"type":"response_item","timestamp":date,"payload":{"type":"message","role":role,"content":[{"type":"text","text":text}]}})
}
fn event(role: &str, text: &str, date: &str) -> Value {
    json!({"type":"event_msg","timestamp":date,"payload":{"type":if role=="user" {"user_message"}else{"agent_message"},"message":text}})
}
fn write(path: &Path, records: &[Value]) {
    let data = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, data).unwrap();
}
struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("sessions")).unwrap();
        fs::create_dir(temp.path().join("archived_sessions")).unwrap();
        Self { temp }
    }
    fn root(&self) -> &Path {
        self.temp.path()
    }
    fn path(&self, id: &str, archive: bool) -> PathBuf {
        self.root()
            .join(if archive {
                "archived_sessions"
            } else {
                "sessions"
            })
            .join(format!("rollout-2026-11-01T09-00-00-{id}.jsonl"))
    }
    fn add(&self, id: &str, cwd: &str, date: &str) -> PathBuf {
        let path = self.path(id, false);
        write(
            &path,
            &[
                meta(id, "2026-11-01T01:00:00Z", cwd),
                msg("user", "first question", date),
                msg("assistant", "answer", date),
            ],
        );
        path
    }
    fn req(&self, args: &[&str]) -> Request {
        let mut all = vec![
            "session-finder".into(),
            "--provider".into(),
            "codex".into(),
            "--home".into(),
            self.root().to_string_lossy().into_owned(),
            "--cache-dir".into(),
            self.root().join("cache").to_string_lossy().into_owned(),
        ];
        all.extend(args.iter().map(|s| s.to_string()));
        Request::new(Cli::try_parse_from(all).unwrap(), false).unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        execute(&self.req(args)).unwrap()
    }
    fn db(&self) -> Connection {
        let db = Connection::open(self.root().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, created_at INTEGER, created_at_ms INTEGER,
            cwd TEXT, source TEXT, git_origin_url TEXT, git_branch TEXT, title TEXT, updated_at INTEGER)").unwrap();
        db
    }
}
fn db_row(db: &Connection, id: &str, cwd: &str, source: &str, created: i64) {
    db.execute(
        "INSERT INTO threads VALUES(?1,?2,NULL,?3,?4,NULL,'main','db title',9999999999)",
        params![id, created, cwd, source],
    )
    .unwrap();
}

#[test]
fn reply_time_ignores_tools_mtime_and_later_questions() {
    let f = Fixture::new();
    let path = f.path(A, false);
    write(
        &path,
        &[
            meta(A, "2026-01-01T00:00:00Z", "/old"),
            msg("assistant", "recent reply", "2026-11-03T00:00:00Z"),
            msg("user", "later question", "2026-11-04T00:00:00Z"),
            json!({"type":"response_item","timestamp":"2026-11-05T00:00:00Z","payload":{"type":"function_call_output","output":"secret tool"}}),
        ],
    );
    let out = f.run(&["recent", "-p", "old", "--since", "2026-11-02T00:00:00Z"]);
    assert_eq!(out.total, 1);
    assert_eq!(
        out.sessions[0].last_reply_at.as_deref(),
        Some("2026-11-03T00:00:00.000Z")
    );
    assert_eq!(out.sessions[0].message_count, Some(2));
    assert_eq!(
        f.run(&["recent", "--sort", "created", "--since", "2026-11-02"])
            .total,
        0
    );
}

#[test]
fn dedup_preserves_later_repeated_turns_and_updates_message_ids() {
    let f = Fixture::new();
    let path = f.path(A, false);
    let mut a = msg("assistant", "answer", "2026-11-02T00:00:00Z");
    a["payload"]["id"] = "m1".into();
    let mut b = a.clone();
    b["payload"]["content"][0]["text"] = "answer expanded".into();
    write(
        &path,
        &[
            meta(A, "2026-11-01T00:00:00Z", "/a"),
            event("user", "repeat", "2026-11-02T00:00:00Z"),
            msg("user", "repeat", "2026-11-02T00:00:00Z"),
            a,
            b,
            msg("user", "repeat", "2026-11-03T00:00:00Z"),
            json!({"type":"message","role":"assistant","timestamp":"2026-11-03T00:00:00Z","content":"legacy"}),
        ],
    );
    let out = f.run(&["show", A]);
    assert_eq!(out.messages.len(), 4);
    assert_eq!(out.messages[1].text, "answer expanded");
    assert_eq!(out.messages[3].text, "legacy");
}

#[test]
fn system_injections_and_tools_are_excluded() {
    assert_eq!(text::clean_user("# AGENTS.md instructions"), "");
    assert_eq!(text::clean_user("<INSTRUCTIONS>secret</INSTRUCTIONS>"), "");
    assert_eq!(
        text::clean_user(
            "<system-reminder>secret</system-reminder>\n<user_input>hello</user_input>"
        ),
        "hello"
    );
    assert_eq!(
        text::clean_user("literal <user_input>x</user_input>"),
        "literal <user_input>x</user_input>"
    );
    assert_eq!(
        text::clean_user("<external_abc>secret</external_abc>用户"),
        "用户"
    );
    let f = Fixture::new();
    write(
        &f.path(A, false),
        &[
            meta(A, "2026-11-01T00:00:00Z", "/a"),
            msg(
                "user",
                "<environment_context>hiddencontext</environment_context>hello",
                "2026-11-02T00:00:00Z",
            ),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"hiddentool"}}),
        ],
    );
    assert_eq!(f.run(&["search", "hiddencontext"]).total, 0);
    assert_eq!(f.run(&["search", "hiddentool"]).total, 0);
    assert_eq!(f.run(&["search", "hello"]).total, 1);
}

#[test]
fn fuzzy_matching_handles_transpositions_chinese_and_normalization() {
    assert!(text::score("modren federation", &["Modern module federation"], false) > 0.0);
    assert_eq!(
        text::score("modren federation", &["Modern federation"], true),
        0.0
    );
    assert!(text::score("路由导航", &["路由和页面导航"], false) > 0.0);
    assert_eq!(text::score("路由导航", &["路由和页面导航"], true), 0.0);
    assert_eq!(
        text::score("missing federation", &["module federation"], false),
        0.0
    );
    assert_eq!(text::normalize("ＡＢＣ 你好，世界!"), "abc 你好 世界");
}

#[test]
fn chinese_single_omission_preserves_endpoints_span_and_ranking() {
    let query = "切换分支";
    let literal = text::score(query, &["如何切换分支"], false);
    let skipped = text::score(query, &["切换到主分支"], false);
    let omitted = text::score(query, &["我想切到主分支"], false);
    assert!(literal > skipped && skipped > omitted && omitted > 0.0);
    // Prefer a complete subsequence, regardless of the fields' ordering.
    assert_eq!(
        text::score(query, &["切到主分支", "切换到主分支"], false),
        skipped
    );
    for (query, text) in [
        ("切换分支", "切到主分支"),
        ("删除文件", "删掉这个文件"),
        ("搜索消息记录", "搜这些消息记录"),
        ("甲乙乙丙", "甲乙丙"),
        ("甲甲乙丙", "甲乙丙"),
    ] {
        assert!(text::score(query, &[text], false) > 0.0, "{query} / {text}");
        assert_eq!(text::score(query, &[text], true), 0.0);
    }
    for (query, text) in [
        ("切换分支", "切主支"),   // two missing query characters
        ("切换分支", "换个分支"), // missing first character
        ("切换分支", "切换部分"), // missing final character
        ("切换分支", "支分换切"), // wrong order
        ("切分支", "切主支"),     // short queries remain strict
        ("删除文件", "我是普通文件"),
        ("切换分支", "切一二三四五六七八九十分支"), // span too long
        ("git切换分支", "git切到主分支"),           // mixed-script terms remain strict
    ] {
        assert_eq!(text::score(query, &[text], false), 0.0, "{query} / {text}");
    }
    assert_eq!(text::score("切换分支 不存在", &["切到主分支"], false), 0.0);
}

#[test]
fn own_fork_header_survives_embedded_parent_metadata() {
    let f = Fixture::new();
    let mut own = meta(A, "2026-11-03T00:00:00Z", "/child");
    own["payload"]["session_id"] = B.into();
    own["payload"]["forked_from_id"] = B.into();
    write(
        &f.path(A, false),
        &[
            meta(B, "2026-11-01T00:00:00Z", "/parent"),
            msg("user", "inherited search phrase", "2026-11-01T00:00:00Z"),
            msg("assistant", "old answer", "2026-11-01T00:00:00Z"),
            own,
            meta(B, "2026-11-01T00:00:00Z", "/parent"),
        ],
    );
    let out = f.run(&["show", A]);
    assert_eq!(out.sessions[0].id, A);
    assert_eq!(out.sessions[0].cwd.as_deref(), Some("/child"));
    assert!(out.sessions[0].last_reply_at.is_none());
    assert!(out.sessions[0].first_user_preview.is_empty());
    assert_eq!(f.run(&["search", "inherited search phrase"]).total, 1);
}

#[test]
fn database_creation_is_applied_before_fork_reply_classification() {
    let f = Fixture::new();
    let mut own = meta(A, "2026-11-01T00:00:00Z", "/child");
    own["payload"]["forked_from_id"] = B.into();
    write(
        &f.path(A, false),
        &[own, msg("assistant", "inherited", "2026-11-02T00:00:00Z")],
    );
    let db = f.db();
    db_row(&db, A, "/child", "vscode", 1793664000); // November 3 UTC
    assert!(f.run(&["show", A]).sessions[0].last_reply_at.is_none());
}

#[test]
fn compressed_archive_and_id_links_are_supported() {
    let f = Fixture::new();
    let path = f.add(A, "/a", "2026-11-02T00:00:00Z");
    let data = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let compressed = f.path(A, true).with_extension("jsonl.zst");
    fs::write(
        &compressed,
        zstd::stream::encode_all(data.as_slice(), 1).unwrap(),
    )
    .unwrap();
    let out = f.run(&["show", &format!("codex://threads/{A}")]);
    assert!(out.sessions[0].compressed && out.sessions[0].archived);
    assert_eq!(out.messages.len(), 2);
    assert!(compressed.exists());
}

#[test]
fn incomplete_tail_is_retried_and_complete_bad_lines_warn() {
    let f = Fixture::new();
    let path = f.add(A, "/a", "2026-11-02T00:00:00Z");
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    write!(file, "not json\n{{\"type\":").unwrap();
    let out = f.run(&["recent"]);
    assert!(out.sessions[0].incomplete_tail);
    assert_eq!(out.sessions[0].malformed_lines, 1);
    assert_eq!(out.warnings.len(), 1);
    writeln!(file,"\"event_msg\",\"timestamp\":\"2026-11-04T00:00:00Z\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"new answer\"}}}}").unwrap();
    let out = f.run(&["recent"]);
    assert!(!out.sessions[0].incomplete_tail);
    assert_eq!(out.sessions[0].last_reply_preview, "new answer");
}

#[test]
fn show_does_not_truncate_messages_and_prefix_ambiguity_fails() {
    let f = Fixture::new();
    let mut records = vec![meta(A, "2026-11-01T00:00:00Z", "/a")];
    for n in 0..60 {
        records.push(msg(
            "user",
            &format!("{n}:{}", "长".repeat(11000)),
            "2026-11-02T00:00:00Z",
        ));
    }
    write(&f.path(A, false), &records);
    f.add(B, "/b", "2026-11-02T00:00:00Z");
    let out = f.run(&["show", A]);
    assert_eq!(out.messages.len(), 60);
    assert!(out.messages[59].text.chars().count() > 11000);
    assert!(
        execute(&f.req(&["show", "01000000"]))
            .unwrap_err_string()
            .contains("matches 2")
    );
    assert_eq!(f.run(&["--id", "01000000"]).total, 2);
}

trait ErrorString {
    fn unwrap_err_string(self) -> String;
}
impl ErrorString for anyhow::Result<Output> {
    fn unwrap_err_string(self) -> String {
        match self {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected error"),
        }
    }
}

#[test]
fn full_text_searches_beyond_previews_and_never_outputs_cache_text() {
    let f = Fixture::new();
    write(
        &f.path(A, false),
        &[
            meta(A, "2026-11-01T00:00:00Z", "/a"),
            msg(
                "user",
                &format!("{} hiddenneedle {}", "x".repeat(1200), "y".repeat(1200)),
                "2026-11-02T00:00:00Z",
            ),
        ],
    );
    let req = f.req(&["search", "hiddenneedle", "--json"]);
    let out = execute(&req).unwrap();
    assert_eq!(out.total, 1);
    assert!(!out.json(&req).to_string().contains("hiddenneedle hidden"));
    assert!(out.json(&req)["sessions"][0].get("searchText").is_none());
    assert!(out.json(&req)["sessions"][0].get("repositoryUrl").is_none());
    assert!(out.sessions[0].first_user_preview.chars().count() <= 1000);
    assert!(
        out.json(&req)["sessions"][0]["matches"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hiddenneedle")
    );
}

#[test]
fn compact_lists_verbose_metadata_and_original_hits_share_results() {
    let f = Fixture::new();
    write(
        &f.path(A, false),
        &[
            meta(A, "2026-11-01T00:00:00Z", "/fixture/project"),
            msg(
                "user",
                &"Start: long initial request. ".repeat(90),
                "2026-11-01T00:00:01Z",
            ),
            msg(
                "assistant",
                &format!(
                    "{} Fixed: Audit_Unique_Middle_7391! {}",
                    "context ".repeat(400),
                    "tail ".repeat(400)
                ),
                "2026-11-01T00:00:02Z",
            ),
            msg(
                "user",
                &"Next question. ".repeat(100),
                "2026-11-01T00:00:03Z",
            ),
            msg(
                "assistant",
                &"Completed! ".repeat(150),
                "2026-11-01T00:00:04Z",
            ),
            json!({"type":"message","role":"system","content":"excluded_system_marker"}),
            json!({"type":"response_item","payload":{"type":"function_call_output","output":"excluded_tool_marker"}}),
        ],
    );
    let compact_req = f.req(&["recent", "--json"]);
    let verbose_req = f.req(&["recent", "--json", "--verbose"]);
    let compact = execute(&compact_req).unwrap().json(&compact_req);
    let verbose = execute(&verbose_req).unwrap().json(&verbose_req);
    assert_eq!(compact["schemaVersion"], 2);
    assert_eq!(compact["sessions"][0]["id"], verbose["sessions"][0]["id"]);
    assert_eq!(
        compact["sessions"][0]["project"]["key"],
        verbose["sessions"][0]["project"]["key"]
    );
    assert_eq!(compact["sessions"][0].as_object().unwrap().len(), 11);
    assert!(compact["sessions"][0].get("path").is_none());
    assert!(verbose["sessions"][0]["paths"].is_array());
    assert!(verbose["sessions"][0]["messageCount"].is_number());
    assert!(compact.to_string().len() * 2 < verbose.to_string().len());
    assert!(
        compact["sessions"][0]["preview"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
    assert!(
        compact["sessions"][0]["title"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );

    let req = f.req(&["search", "audit_unique_middle_7391", "--json"]);
    let cold = execute(&req).unwrap();
    assert_eq!(cold.stats.parsed_files, 1);
    let warm = execute(&req).unwrap();
    assert_eq!(warm.stats.parsed_files, 0);
    assert_eq!(warm.stats.header_files, 0);
    let uncached_req = f.req(&["search", "audit_unique_middle_7391", "--json", "--no-cache"]);
    let uncached = execute(&uncached_req).unwrap();
    assert_eq!(cold.json(&req)["sessions"], warm.json(&req)["sessions"]);
    assert_eq!(
        cold.json(&req)["sessions"],
        uncached.json(&uncached_req)["sessions"]
    );
    let session = &cold.json(&req)["sessions"][0];
    assert_eq!(session["matches"][0]["role"], "assistant");
    let hit = session["matches"][0]["text"].as_str().unwrap();
    assert!(hit.contains("Audit_Unique_Middle_7391!"));
    assert!(hit.chars().count() <= 180);
    assert!(session.get("preview").is_none());
    for query in ["excluded_system_marker", "excluded_tool_marker"] {
        assert_eq!(f.run(&["search", query]).total, 0);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_session-finder"))
        .args(["--provider", "codex"])
        .args(["search", "audit_unique_middle_7391", "--home"])
        .arg(f.root())
        .arg("--no-cache")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("↳ Assistant:")
    );
    let show_req = f.req(&["show", A, "--json"]);
    let shown = execute(&show_req).unwrap().json(&show_req);
    assert_eq!(shown["schemaVersion"], 1);
    assert!(shown["messages"][1]["text"].as_str().unwrap().len() > 1000);
}

#[test]
fn legacy_search_cache_upgrades_lazily_without_invalidating_summary() {
    let f = Fixture::new();
    f.add(A, "/project", "2026-11-01T00:00:00Z");
    f.run(&["search", "answer"]);
    let path = fs::read_dir(f.root().join("cache"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
        .unwrap();
    let db = Connection::open(path).unwrap();
    let old = zstd::stream::encode_all(std::io::Cursor::new("first question answer"), 1).unwrap();
    db.execute("UPDATE texts SET body=?1", [old]).unwrap();
    db.execute("DELETE FROM search_messages_v1", []).unwrap();
    assert_eq!(f.run(&["recent"]).stats.parsed_files, 0);
    let refreshed = f.run(&["search", "answer"]);
    assert_eq!(refreshed.stats.parsed_files, 1);
    assert!(!refreshed.sessions[0].matches.is_empty());
    let warm = f.run(&["search", "answer"]);
    assert_eq!(warm.stats.parsed_files, 0);
    let body: Vec<u8> = db
        .query_row("SELECT body FROM texts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        String::from_utf8(zstd::stream::decode_all(std::io::Cursor::new(body)).unwrap()).unwrap(),
        "first question answer"
    );
    let summary: String = db
        .query_row("SELECT summary FROM files", [], |r| r.get(0))
        .unwrap();
    assert!(!summary.contains("\"matches\""));
    assert!(!summary.contains("searchMessages"));
}

#[test]
fn ambiguous_id_candidates_keep_separate_stderr_lines() {
    let f = Fixture::new();
    f.add(A, "/project", "2026-11-01T00:00:00Z");
    f.add(B, "/project", "2026-11-01T00:00:00Z");
    for action in ["show", "relations"] {
        let out = Command::new(env!("CARGO_BIN_EXE_session-finder"))
            .args(["--provider", "codex"])
            .args([action, "01000000", "--home"])
            .arg(f.root())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let stderr = String::from_utf8(out.stderr).unwrap();
        let lines: Vec<_> = stderr.lines().collect();
        assert!(lines.len() >= 3, "{stderr}");
        assert!(
            lines.iter().skip(1).any(|line| line.ends_with(A)),
            "{stderr}"
        );
        assert!(
            lines.iter().skip(1).any(|line| line.ends_with(B)),
            "{stderr}"
        );
    }
    assert_eq!(
        text::safe("message\u{1b}\nA\u{202e}\nB\u{7}", true),
        "message\nA\nB"
    );
}

#[test]
fn verbose_requires_json_list_commands() {
    assert!(Cli::try_parse_from(["sf", "recent", "--verbose"]).is_err());
    for args in [
        vec!["sf", "projects", "--json", "--verbose"],
        vec!["sf", "show", A, "--json", "--verbose"],
    ] {
        assert!(Request::new(Cli::try_parse_from(args).unwrap(), false).is_err());
    }
    let f = Fixture::new();
    assert!(f.req(&["--json", "--verbose"]).options.verbose);
}

#[test]
fn duplicate_copies_choose_newest_reply_then_message_count() {
    let f = Fixture::new();
    f.add(A, "/old", "2026-11-02T00:00:00Z");
    write(
        &f.path(A, true),
        &[
            meta(A, "2026-11-01T00:00:00Z", "/new"),
            msg("assistant", "newer", "2026-11-03T00:00:00Z"),
        ],
    );
    let out = f.run(&["recent", "-p", "new"]);
    assert_eq!(out.total, 1);
    assert_eq!(out.sessions[0].paths.len(), 2);
    assert_eq!(out.sessions[0].last_reply_preview, "newer");
    assert_eq!(f.run(&["recent", "-p", "old"]).total, 0);
    assert_eq!(
        f.run(&["recent", "--sort", "created"]).sessions[0].last_reply_preview,
        "newer"
    );
}

#[test]
fn exact_project_precedes_incidental_path_and_hidden_subagents() {
    let f = Fixture::new();
    let mut a = meta(A, "2026-11-01T00:00:00Z", "/threadline");
    a["payload"]["source"] = json!({"subagent":{"thread_spawn":{"parent_thread_id":B}}});
    write(
        &f.path(A, false),
        &[a, msg("assistant", "subagent", "2026-11-02T00:00:00Z")],
    );
    f.add(
        B,
        "/x/.threadline-native/workspaces/modern",
        "2026-11-02T00:00:00Z",
    );
    assert_eq!(f.run(&["recent", "-p", "threadline"]).total, 0);
    assert_eq!(
        f.run(&["recent", "-p", "threadline", "--include-subagents"])
            .total,
        1
    );
    assert_eq!(
        f.run(&["show", A]).sessions[0].parent_session_id.as_deref(),
        Some(B)
    );
}

#[test]
fn worktree_common_dir_and_repository_identity_group_correctly() {
    let f = Fixture::new();
    let root = f.root().join("main-project");
    let work = f.root().join("worktree");
    fs::create_dir_all(root.join(".git/worktrees/w")).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::write(
        work.join(".git"),
        format!("gitdir: {}\n", root.join(".git/worktrees/w").display()),
    )
    .unwrap();
    fs::write(root.join(".git/worktrees/w/commondir"), "../..\n").unwrap();
    f.add(A, work.to_str().unwrap(), "2026-11-02T00:00:00Z");
    let out = f.run(&["recent", "-p", "main-project"]);
    assert_eq!(out.total, 1);
    assert_eq!(
        out.sessions[0].project.root.as_deref(),
        fs::canonicalize(root).unwrap().to_str()
    );
    assert_eq!(
        project::repository("git@github.com:foo/bar.git"),
        project::repository("https://github.com/foo/bar.git")
    );
}

#[test]
fn created_page_only_parses_selected_bodies_and_supports_offset() {
    let f = Fixture::new();
    f.add(A, "/a", "2026-11-02T00:00:00Z");
    f.add(B, "/b", "2026-11-02T00:00:00Z");
    let db = f.db();
    db_row(&db, A, "/a", "vscode", 1793664000);
    db_row(&db, B, "/b", "vscode", 1793577600);
    let out = f.run(&["recent", "--sort", "created", "-n", "1"]);
    assert_eq!(out.total, 2);
    assert_eq!(out.stats.parsed_files, 1);
    assert_eq!(out.stats.header_files, 0);
    assert_eq!(out.sessions[0].id, A);
    let out = f.run(&["recent", "--sort", "created", "-n", "1", "--offset", "1"]);
    assert_eq!(out.sessions[0].id, B);
    assert_eq!(out.stats.parsed_files, 1);
}

#[test]
fn created_metadata_survives_unreadable_selected_body() {
    let f = Fixture::new();
    fs::write(f.path(A, false).with_extension("jsonl.zst"), b"not zstd").unwrap();
    let db = f.db();
    db_row(&db, A, "/a", "vscode", 1793664000);
    let out = f.run(&["recent", "--sort", "created"]);
    assert_eq!(out.total, 1);
    assert!(!out.sessions[0].details_available);
    assert!(!out.warnings.is_empty());
}

#[test]
fn project_and_subagent_filter_happen_before_body_reads() {
    let f = Fixture::new();
    f.add(A, "/wrong", "2026-11-02T00:00:00Z");
    fs::write(f.path(B, false).with_extension("jsonl.zst"), b"broken").unwrap();
    fs::write(f.path(C, false).with_extension("jsonl.zst"), b"broken").unwrap();
    let db = f.db();
    db_row(&db, A, "/target", "vscode", 1793491200);
    db_row(&db, B, "/other", "vscode", 1793491200);
    db_row(&db, C, "/target", "subagent", 1793491200);
    for args in [
        vec!["recent", "-p", "target"],
        vec!["search", "answer", "-p", "target"],
    ] {
        let out = f.run(&args);
        assert_eq!(out.total, 1);
        assert_eq!(out.stats.parsed_files, 1);
        assert_eq!(out.stats.candidate_files, 1);
        assert!(out.warnings.is_empty());
    }
}

#[test]
fn scoped_cache_refresh_preserves_other_projects() {
    let f = Fixture::new();
    f.add(A, "/one", "2026-11-02T00:00:00Z");
    f.add(B, "/two", "2026-11-02T00:00:00Z");
    assert_eq!(f.run(&["recent", "-p", "one"]).stats.parsed_files, 1);
    assert_eq!(f.run(&["recent", "-p", "two"]).stats.parsed_files, 1);
    assert_eq!(
        f.run(&["recent", "-p", "one", "--refresh"])
            .stats
            .parsed_files,
        1
    );
    assert_eq!(f.run(&["recent", "-p", "two"]).stats.parsed_files, 0);
}

#[test]
fn changed_logs_invalidate_both_summary_and_full_text() {
    let f = Fixture::new();
    let path = f.add(A, "/one", "2026-11-02T00:00:00Z");
    assert_eq!(f.run(&["search", "answer"]).total, 1);
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        msg("assistant", "freshneedle", "2026-11-03T00:00:00Z")
    )
    .unwrap();
    assert_eq!(
        f.run(&["recent"]).sessions[0].last_reply_preview,
        "freshneedle"
    );
    assert_eq!(f.run(&["search", "freshneedle"]).total, 1);
    assert_eq!(f.run(&["search", "freshneedle"]).stats.parsed_files, 0);
}

#[test]
fn deleted_logs_disappear_and_titles_update_without_body_reads() {
    let f = Fixture::new();
    let path = f.add(A, "/one", "2026-11-02T00:00:00Z");
    f.run(&["recent"]);
    fs::write(
        f.root().join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"id":A,"thread_name":"new title","updated_at":"2026-11-04T00:00:00Z"})
        ),
    )
    .unwrap();
    let out = f.run(&["recent"]);
    assert_eq!(out.sessions[0].title, "new title");
    assert_eq!(out.stats.parsed_files, 0);
    fs::remove_file(path).unwrap();
    assert_eq!(f.run(&["recent"]).total, 0);
}

#[test]
fn database_changes_are_consistent_in_all_query_modes() {
    let f = Fixture::new();
    f.add(A, "/original", "2026-11-02T00:00:00Z");
    let db = f.db();
    db_row(&db, A, "/new", "vscode", 1793491200);
    db.execute("UPDATE threads SET created_at_ms=1793500000123", [])
        .unwrap();
    for args in [
        vec!["recent", "--sort", "created"],
        vec!["recent"],
        vec!["search", "answer"],
        vec!["show", A],
    ] {
        let out = f.run(&args);
        assert_eq!(out.sessions[0].cwd.as_deref(), Some("/new"));
        assert_eq!(out.sessions[0].created_at, time::millis_iso(1793500000123));
        assert_eq!(out.sessions[0].title, "db title");
    }
    db.execute(
        "UPDATE threads SET cwd='/changed', title='changed title'",
        [],
    )
    .unwrap();
    assert_eq!(
        f.run(&["recent", "-p", "changed"]).sessions[0].title,
        "changed title"
    );
}

#[test]
fn incompatible_database_falls_back_and_newest_compatible_is_used() {
    let f = Fixture::new();
    f.add(A, "/a", "2026-11-02T00:00:00Z");
    let bad = Connection::open(f.root().join("state_99.sqlite")).unwrap();
    bad.execute_batch("CREATE TABLE threads(id TEXT)").unwrap();
    assert!(
        f.run(&["recent"])
            .warnings
            .iter()
            .any(|w| w.contains("reading logs"))
    );
    let db = f.db();
    db_row(&db, A, "/database", "vscode", 1793491200);
    assert_eq!(
        f.run(&["recent"]).sessions[0].cwd.as_deref(),
        Some("/database")
    );
}

#[test]
fn cache_unavailable_does_not_prevent_results_and_no_cache_creates_nothing() {
    let f = Fixture::new();
    f.add(A, "/a", "2026-11-02T00:00:00Z");
    assert_eq!(f.run(&["recent", "--no-cache"]).total, 1);
    assert!(!f.root().join("cache").exists());
    fs::write(f.root().join("cache"), "not a directory").unwrap();
    let out = f.run(&["recent"]);
    assert_eq!(out.total, 1);
    assert!(out.warnings.iter().any(|w| w.contains("Cache")));
}

#[test]
fn local_dates_full_day_relative_bounds_and_invalid_arguments() {
    let a = time::boundary(Some("2026-11-04"), false, 0)
        .unwrap()
        .unwrap();
    let b = time::boundary(Some("2026-11-04"), true, 0)
        .unwrap()
        .unwrap();
    assert_eq!(b - a, 86400000);
    assert_eq!(
        time::boundary(Some("24h"), false, 100000000).unwrap(),
        Some(13600000)
    );
    assert!(time::boundary(Some("2026-02-30"), false, 0).is_err());
    assert!(time::boundary(Some("yesterday"), false, 0).is_err());
    assert!(time::boundary(Some("昨天"), false, 0).is_err());
    assert!(time::iso(&json!(i64::MIN)).is_none());
    assert!(
        Request::new(
            Cli::try_parse_from([
                "sf",
                "recent",
                "--since",
                "2026-11-04",
                "--until",
                "2026-11-01"
            ])
            .unwrap(),
            false
        )
        .is_err()
    );
    assert!(
        Request::new(
            Cli::try_parse_from(["sf", "search", "a", "--sort", "relevance", "--since", "7d"])
                .unwrap(),
            false
        )
        .is_err()
    );
    assert!(Cli::try_parse_from(["sf", "recent", "-n", "0"]).is_err());
    assert_eq!(
        text::session_id(&format!("/tmp/rollout-{A}.jsonl")).as_deref(),
        Some(A)
    );
    assert!(text::session_id("0100000").is_none());
}

#[test]
fn concurrent_cli_writers_keep_both_project_indexes_and_emit_valid_json() {
    let f = Fixture::new();
    f.add(A, "/one", "2026-11-02T00:00:00Z");
    f.add(B, "/two", "2026-11-02T00:00:00Z");
    let mut children = Vec::new();
    for project in ["one", "two", "one", "two"] {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_session-finder"))
                .args(["--provider", "codex"])
                .args(["recent", "-p", project, "--json", "--home"])
                .arg(f.root())
                .arg("--cache-dir")
                .arg(f.root().join("cache"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["warnings"], json!([]));
    }
    assert_eq!(f.run(&["recent", "-p", "one"]).stats.parsed_files, 0);
    assert_eq!(f.run(&["recent", "-p", "two"]).stats.parsed_files, 0);
}

#[test]
fn terminal_controls_are_removed_but_json_text_preserved() {
    assert_eq!(text::safe("\u{1b}[31mhello\u{202e}\n", true), "[31mhello\n");
    let f = Fixture::new();
    let path = f.path(A, false);
    write(
        &path,
        &[
            meta(A, "2026-11-01T00:00:00Z", "/a"),
            msg("assistant", "hello\n世界\u{1b}", "2026-11-02T00:00:00Z"),
        ],
    );
    assert_eq!(
        parser::parse(&path, None, false, false, true)
            .unwrap()
            .messages[0]
            .text,
        "hello\n世界\u{1b}"
    );
}
