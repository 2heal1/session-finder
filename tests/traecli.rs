use clap::Parser;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use session_finder::{
    cli::{Cli, Request},
    model::{Provider, Scope},
    query::{Output, execute},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const A: &str = "01000000-0000-4000-8000-000000000001";
const B: &str = "02000000-0000-4000-8000-000000000002";
const C: &str = "03000000-0000-4000-8000-000000000003";
const START: &str = "2026-11-01T01:00:00Z";
const END: &str = "2026-11-01T01:00:01Z";

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let f = Self(tempfile::tempdir().unwrap());
        for dir in ["cli/sessions", "codex/sessions"] {
            fs::create_dir_all(f.0.path().join(dir)).unwrap();
        }
        fs::write(f.0.path().join("ide.jsonl"), "").unwrap();
        f
    }
    fn home(&self) -> PathBuf {
        self.0.path().join("cli")
    }
    fn req(&self, provider: &str, args: &[&str]) -> Request {
        let mut argv = vec![
            "session-finder".into(),
            "--home".into(),
            self.0.path().join("codex").display().to_string(),
            "--cache-dir".into(),
            self.0.path().join("cache").display().to_string(),
        ];
        if !provider.is_empty() {
            argv.extend(["--provider".into(), provider.into()]);
        }
        if matches!(provider, "" | "traecli" | "all") {
            argv.extend(["--traecli-home".into(), self.home().display().to_string()]);
        }
        if matches!(provider, "" | "all") {
            argv.extend([
                "--trae-index".into(),
                self.0.path().join("ide.jsonl").display().to_string(),
            ]);
        }
        argv.extend(args.iter().map(|s| s.to_string()));
        Request::new(Cli::try_parse_from(argv).unwrap(), false).unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        execute(&self.req("traecli", args)).unwrap()
    }
    fn log(&self, id: &str, rows: &[Value]) -> PathBuf {
        let path = self
            .home()
            .join("sessions")
            .join(format!("rollout-{id}.jsonl"));
        write_rows(&path, rows);
        path
    }
}

fn write_rows(path: &Path, rows: &[Value]) {
    fs::write(
        path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
}

fn meta(id: &str) -> Value {
    json!({"type":"session_meta","timestamp":START,"payload":{
        "id":id,"timestamp":START,"cwd":"/fixture/orchid","source":"cli","history_format":"canonical_v1"
    }})
}

fn item(role: &str, id: &str, text: &str) -> Value {
    json!({"type":"message","id":id,"role":role,"phase":if role=="assistant" {"final_answer"} else {""},
        "content":[{"type":if role=="user" {"input_text"} else {"output_text"},"text":text}]})
}

fn mutation(commit: &str, turn: &str, at: &str, items: Vec<Value>) -> Value {
    json!({"type":"history_mutation","timestamp":at,"payload":{
        "version":1,"operation":"append","commit_id":commit,"turn_id":turn,"items":items
    }})
}

fn canonical(id: &str, prompt: &str, reply: &str) -> Vec<Value> {
    vec![
        meta(id),
        mutation("one", "turn-1", START, vec![item("user", "u1", prompt)]),
        mutation("two", "turn-1", END, vec![item("assistant", "a1", reply)]),
    ]
}

fn database(f: &Fixture) -> Connection {
    let db = Connection::open(f.home().join("state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, source TEXT,
        git_origin_url TEXT, git_branch TEXT, created_at_ms INTEGER, title TEXT, name TEXT, archived INTEGER);
        ").unwrap();
    db.execute("INSERT INTO threads VALUES(?1,'/fixture/orchid','cli',NULL,'main',1793494800000,'数据库旧标题','适配原因原生标题',0)",[A]).unwrap();
    db
}

#[test]
fn canonical_fulltext_matches_codex_with_cache_and_visible_messages() {
    let f = Fixture::new();
    let long = format!("{} 正文深处专用词", "普通文字".repeat(400));
    let mut rows = canonical(A, "查找会话", &long);
    rows.insert(1, mutation("context", "turn-1", START, vec![
        item("developer", "d1", "开发者指令秘密"),
        json!({"type":"message","role":"user","id":"env","content":[{"type":"input_text","text":"注入环境秘密"}],
            "internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"]}}),
        json!({"type":"reasoning","content":[{"type":"reasoning_text","text":"推理秘密"}]}),
        json!({"type":"function_call_output","output":"工具秘密"}),
    ]));
    // The event mirror must not duplicate the canonical answer.
    rows.push(json!({"type":"event_msg","timestamp":END,"payload":{
        "type":"agent_message","message":long,"phase":"final_answer"
    }}));
    f.log(A, &rows);
    let db = database(&f);
    let args = ["search", "正文深处专用词", "--exact"];
    let cold = f.run(&args);
    let warm = f.run(&args);
    let uncached = f.run(&["search", "正文深处专用词", "--exact", "--no-cache"]);
    assert_eq!(cold.total, 1);
    assert_eq!(cold.stats.parsed_files, 1);
    assert_eq!(warm.stats.parsed_files, 0);
    assert_eq!(warm.stats.cached_files, 1);
    assert_eq!(uncached.stats.cached_files, 0);
    assert_eq!(cold.sessions[0].public(), warm.sessions[0].public());
    assert_eq!(cold.sessions[0].public(), uncached.sessions[0].public());
    let s = &cold.sessions[0];
    assert_eq!(s.title, "适配原因原生标题");
    assert_eq!(s.title_source.as_deref(), Some("metadata"));
    assert_eq!(s.provider, Provider::Traecli);
    assert_eq!(s.scope, Scope::LocalConversation);
    assert_eq!(s.message_count, Some(2));
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert!(s.native_url.is_empty());
    assert_eq!(cold.home, f.home().canonicalize().unwrap());
    for excluded in ["推理秘密", "工具秘密", "注入环境秘密", "开发者指令秘密"]
    {
        assert_eq!(f.run(&["search", excluded, "--exact"]).total, 0);
    }
    let shown = f.run(&["show", A]);
    assert_eq!(shown.messages.len(), 2);
    assert_eq!(shown.messages[1].text, long);
    assert_eq!(shown.messages[1].turn_id.as_deref(), Some("turn-1"));
    assert_eq!(shown.messages[1].phase.as_deref(), Some("final_answer"));
    // Renaming in the live WAL changes title search without reparsing the log.
    db.execute("UPDATE threads SET name='改名后原生标题' WHERE id=?1", [A])
        .unwrap();
    assert_eq!(f.run(&["search", "适配原因原生标题", "--exact"]).total, 0);
    let renamed = f.run(&["search", "改名后原生标题", "--exact"]);
    assert_eq!(renamed.total, 1);
    assert_eq!(renamed.stats.parsed_files, 0);
    assert_eq!(
        db.query_row("SELECT name FROM threads", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "改名后原生标题"
    );

    let codex =
        f.0.path()
            .join("codex/sessions")
            .join(format!("rollout-{A}.jsonl"));
    write_rows(
        &codex,
        &[
            meta(A),
            json!({"type":"response_item","timestamp":START,"payload":item("user","u1","查找会话")}),
            json!({"type":"response_item","timestamp":END,"payload":item("assistant","a1",&long)}),
        ],
    );
    let codex = execute(&f.req("codex", &["search", "正文深处专用词", "--exact"])).unwrap();
    assert_eq!(codex.total, 1);
    assert_eq!(codex.sessions[0].match_score, s.match_score);
    assert_eq!(codex.sessions[0].message_count, s.message_count);
    assert_eq!(codex.sessions[0].last_reply_at, s.last_reply_at);
}

#[test]
fn display_completions_are_visible_and_do_not_duplicate_raw_items() {
    let f = Fixture::new();
    let mut rows = canonical(A, "提问", "原始回复应由可见回复替代");
    rows[2]["payload"]["display_completions"] = json!([
        {"turn_id":"turn-1","completed_at_ms":1793494801000i64,"item":{
            "type":"AgentMessage","id":"display-1","phase":"final_answer",
            "content":[{"type":"Text","text":"可见最终回答"}]}},
        {"item":{"type":"Reasoning","raw_content":["推理秘密"]}}
    ]);
    rows.push(json!({"type":"event_msg","timestamp":END,"payload":{
        "type":"item_completed","turn_id":"turn-1","item":{
            "type":"AgentMessage","id":"display-1","phase":"final_answer",
            "content":[{"type":"Text","text":"可见最终回答"}]}}}));
    f.log(A, &rows);
    let shown = f.run(&["show", A]);
    assert_eq!(shown.messages.len(), 2);
    assert_eq!(shown.messages[1].text, "可见最终回答");
    assert_eq!(f.run(&["search", "可见最终回答", "--exact"]).total, 1);
    assert_eq!(
        f.run(&["search", "原始回复应由可见回复替代", "--exact"])
            .total,
        0
    );
}

#[test]
fn canonical_rollback_and_legacy_control_turn_remove_retracted_text() {
    let f = Fixture::new();
    let mut rows = canonical(A, "保留提问", "保留回答");
    rows.extend([
        mutation("three", "turn-2", END, vec![item("user", "u2", "撤回提问")]),
        mutation(
            "four",
            "turn-2",
            END,
            vec![item("assistant", "a2", "撤回回答")],
        ),
        json!({"type":"history_mutation","timestamp":END,"payload":{
            "version":1,"commit_id":"rollback","turn_id":"control","operation":"rollback",
            "num_turns":1,"target_commit_id":"two","target_turn_id":"turn-2"}}),
        json!({"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}),
    ]);
    f.log(A, &rows);
    assert_eq!(f.run(&["show", A]).messages.len(), 2);
    assert_eq!(f.run(&["search", "撤回", "--exact"]).total, 0);
    assert_eq!(f.run(&["search", "保留", "--exact"]).total, 1);

    let mut legacy = vec![meta(B)];
    for (turn, text) in [("first", "保留旧版"), ("second", "撤回旧版")] {
        legacy.extend([
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}}),
            json!({"type":"event_msg","timestamp":START,"payload":{"type":"user_message","message":text}}),
            json!({"type":"event_msg","timestamp":END,"payload":{"type":"agent_message","message":text}}),
        ]);
    }
    legacy.extend([
        json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"empty-control"}}),
        json!({"type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}),
    ]);
    f.log(B, &legacy);
    let shown = f.run(&["show", B]);
    assert_eq!(shown.messages.len(), 2);
    assert_eq!(shown.messages[1].text, "保留旧版");
    assert_eq!(f.run(&["search", "撤回旧版", "--exact"]).total, 0);
}

#[test]
fn legacy_duplicates_repeated_turns_compression_and_incomplete_tail() {
    let f = Fixture::new();
    let mut rows = vec![meta(A)];
    for turn in ["one", "two"] {
        rows.extend([
            json!({"type":"event_msg","timestamp":START,"payload":{"type":"task_started","turn_id":turn}}),
            json!({"type":"event_msg","timestamp":START,"payload":{"type":"user_message","message":"再试一次"}}),
            json!({"type":"response_item","timestamp":START,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"再试一次"}]}}),
            json!({"type":"event_msg","timestamp":END,"payload":{"type":"agent_message","message":"已完成"}}),
            json!({"type":"response_item","timestamp":END,"payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"已完成"}]}}),
        ]);
    }
    let path = f.log(A, &rows);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"bad json\n{\"partial");
    let archive = f.home().join("archived_sessions");
    fs::create_dir(&archive).unwrap();
    let compressed = archive.join(format!("rollout-{A}.jsonl.zst"));
    fs::write(
        &compressed,
        zstd::stream::encode_all(bytes.as_slice(), 1).unwrap(),
    )
    .unwrap();
    fs::remove_file(path).unwrap();
    let shown = f.run(&["show", A]);
    assert_eq!(shown.messages.len(), 4);
    assert!(shown.sessions[0].compressed);
    assert!(shown.sessions[0].archived);
    assert!(shown.sessions[0].incomplete_tail);
    assert_eq!(shown.sessions[0].malformed_lines, 1);
    assert!(!shown.warnings.is_empty());
}

#[test]
fn shared_filters_pagination_subagents_and_projects() {
    let f = Fixture::new();
    f.log(A, &canonical(A, "threadline project", "first reply"));
    let mut b = canonical(B, "threadline project", "second reply");
    b[0]["payload"]["cwd"] = json!("/fixture/maple");
    b[0]["payload"]["timestamp"] = json!("2026-11-02T01:00:00Z");
    b[2]["timestamp"] = json!("2026-11-02T01:00:01Z");
    f.log(B, &b);
    let mut c = canonical(C, "threadline project", "child reply");
    c[0]["payload"]["source"] = json!({"subagent":{"thread_spawn":{"parent_thread_id":A}}});
    f.log(C, &c);
    assert_eq!(f.run(&["search", "thredline"]).total, 2);
    assert_eq!(f.run(&["search", "thredline", "--exact"]).total, 0);
    assert_eq!(
        f.run(&["search", "threadline", "--include-subagents"])
            .total,
        3
    );
    assert_eq!(f.run(&["recent", "-p", "orchid"]).total, 1);
    assert_eq!(
        f.run(&[
            "recent",
            "--sort",
            "created",
            "--since",
            "2026-11-02T00:00:00Z"
        ])
        .total,
        1
    );
    assert_eq!(
        f.run(&[
            "recent",
            "--sort",
            "replied",
            "--until",
            "2026-11-02T00:00:00Z"
        ])
        .total,
        1
    );
    assert_eq!(
        f.run(&["recent", "-n", "1", "--offset", "1"]).sessions[0].id,
        A
    );
    assert_eq!(f.run(&["projects"]).total, 2);
    let child = f.run(&["show", C]);
    assert!(child.sessions[0].is_subagent);
    assert_eq!(child.sessions[0].parent_session_id.as_deref(), Some(A));
    assert_eq!(f.run(&["show", "01000000"]).sessions[0].id, A);
}

#[test]
fn all_merges_three_providers_and_disambiguates_same_uuid() {
    let f = Fixture::new();
    f.log(A, &canonical(A, "共同检索词", "CLI reply"));
    write_rows(
        &f.0.path()
            .join("codex/sessions")
            .join(format!("rollout-{A}.jsonl")),
        &[
            meta(A),
            json!({"type":"message","role":"user","content":"共同检索词"}),
            json!({"type":"message","role":"assistant","timestamp":END,"content":"Codex reply"}),
        ],
    );
    fs::write(
        f.0.path().join("ide.jsonl"),
        json!({
            "conversation_id":"010000000000400080000001","sessions":[{
                "session_id":"010000000000400080000002","user_query":"共同检索词"}]
        })
        .to_string(),
    )
    .unwrap();
    let req = f.req("all", &["search", "共同检索词", "--exact"]);
    let cold = execute(&req).unwrap();
    assert_eq!(cold.total, 3);
    for args in [
        vec!["search", "共同检索词", "--exact"],
        vec!["recent"],
        vec!["projects"],
    ] {
        let explicit = f.req("all", &args);
        let omitted = f.req("", &args);
        let a = execute(&explicit).unwrap().json(&explicit);
        let b = execute(&omitted).unwrap().json(&omitted);
        assert_eq!(a["total"], b["total"]);
        assert_eq!(a["sessions"], b["sessions"]);
        assert_eq!(a["projects"], b["projects"]);
        assert_eq!(b["source"], "local_session_providers");
    }
    assert!(
        cold.sessions
            .iter()
            .any(|s| s.provider == Provider::Traecli)
    );
    let err = execute(&f.req("all", &["show", A]))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("匹配 2 个会话"));
    assert!(err.contains("traecli:"));
    let default_error = execute(&f.req("", &["show", A])).err().unwrap().to_string();
    assert!(default_error.contains("匹配 2 个会话"));
    assert_eq!(
        execute(&f.req("", &["--id", "010000000000400080000001"]))
            .unwrap()
            .sessions[0]
            .provider,
        Provider::Trae
    );
    assert_eq!(f.run(&["show", A]).messages[1].text, "CLI reply");
    execute(&f.req("codex", &["recent", "--refresh"])).unwrap();
    let warm = execute(&req).unwrap();
    assert_eq!(warm.total, 3);
    assert_eq!(warm.stats.parsed_files, 0);
    assert!(warm.stats.cached_files >= 3);
    let page = execute(&f.req(
        "all",
        &[
            "search",
            "共同检索词",
            "--exact",
            "-n",
            "1",
            "--offset",
            "1",
        ],
    ))
    .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.sessions.len(), 1);
    assert_eq!(page.sessions[0].public(), warm.sessions[1].public());
    f.log(B, &canonical(B, "unique CLI session", "CLI body"));
    let shown = execute(&f.req("", &["show", B])).unwrap();
    assert_eq!(shown.sessions[0].provider, Provider::Traecli);
    assert_eq!(shown.messages[1].text, "CLI body");
}

#[test]
fn default_search_keeps_available_sources_when_other_sources_are_missing() {
    let f = Fixture::new();
    write_rows(
        &f.0.path()
            .join("codex/sessions")
            .join(format!("rollout-{A}.jsonl")),
        &[
            meta(A),
            json!({"type":"message","role":"user","content":"available source"}),
        ],
    );
    let mut req = f.req("", &["search", "available"]);
    req.options.trae_index = Some(f.0.path().join("missing-ide.jsonl"));
    req.options.traecli_home = Some(f.0.path().join("missing-cli"));
    let out = execute(&req).unwrap();
    assert_eq!(out.total, 1);
    assert_eq!(out.sessions[0].provider, Provider::Codex);
    assert!(out.warnings.iter().any(|s| s.contains("TRAE 来源不可用")));
    assert!(
        out.warnings
            .iter()
            .any(|s| s.contains("TRAE CLI 来源不可用"))
    );
    req.options.refresh = true;
    assert!(execute(&req).is_err());
}

#[test]
fn explicitly_shared_home_keeps_provider_parsers_and_text_separate() {
    let f = Fixture::new();
    f.log(
        A,
        &canonical(A, "只有新版解析器能看到", "canonical-only reply"),
    );
    let mut req = f.req("all", &["search", "canonical-only", "--exact"]);
    req.options.home = Some(f.home());
    let out = execute(&req).unwrap();
    assert_eq!(out.total, 1);
    assert_eq!(out.sessions[0].provider, Provider::Traecli);
    req.query.clear();
    let recent = execute(&req).unwrap();
    assert_eq!(recent.total, 2);
    let codex = recent
        .sessions
        .iter()
        .find(|s| s.provider == Provider::Codex)
        .unwrap();
    let cli = recent
        .sessions
        .iter()
        .find(|s| s.provider == Provider::Traecli)
        .unwrap();
    assert_eq!(codex.message_count, Some(0));
    assert_eq!(cli.message_count, Some(2));
}

#[test]
fn append_delete_refresh_and_scoped_queries_keep_cache_current() {
    let f = Fixture::new();
    let path = f.log(A, &canonical(A, "first", "before"));
    f.log(B, &canonical(B, "second", "unchanged"));
    f.run(&["search", "before", "--exact"]);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        mutation(
            "three",
            "turn-2",
            "2026-11-03T00:00:00Z",
            vec![
                item("user", "u2", "appended prompt"),
                item("assistant", "a2", "append-unique-reply")
            ]
        )
    )
    .unwrap();
    let updated = f.run(&["search", "append-unique-reply", "--exact"]);
    assert_eq!(updated.total, 1);
    assert_eq!(updated.stats.parsed_files, 1);
    assert_eq!(updated.sessions[0].message_count, Some(4));
    let scoped = f.run(&["recent", "--id", A, "--refresh"]);
    assert_eq!(scoped.stats.parsed_files, 1);
    assert_eq!(
        f.run(&["search", "unchanged", "--exact"])
            .stats
            .parsed_files,
        0
    );
    fs::remove_file(path).unwrap();
    assert_eq!(
        f.run(&["search", "append-unique-reply", "--exact"]).total,
        0
    );
    assert_eq!(f.run(&["recent"]).total, 1);
}

#[test]
fn titles_fall_back_to_index_then_prompt_and_never_infer_missing_logs() {
    let f = Fixture::new();
    f.log(A, &canonical(A, "原始提问", "answer"));
    fs::write(
        f.home().join("session_index.jsonl"),
        json!({
            "id":A,"thread_name":"索引标题","updated_at":END
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(f.run(&["search", "索引标题", "--exact"]).total, 1);
    fs::remove_file(f.home().join("session_index.jsonl")).unwrap();
    assert_eq!(f.run(&["recent"]).sessions[0].title, "原始提问");
    let db = database(&f);
    db.execute("INSERT INTO threads(id,cwd,source,created_at_ms,title) VALUES(?1,'/missing','cli',1793494800000,'无正文元数据')",params![B]).unwrap();
    assert_eq!(f.run(&["recent"]).total, 1);
    assert!(execute(&f.req("traecli", &["show", B])).is_err());
}

#[test]
fn unsupported_history_is_reported_without_silently_indexing_partial_text() {
    let f = Fixture::new();
    let mut rows = canonical(A, "部分提问", "部分回答");
    rows.push(
        json!({"type":"history_mutation","payload":{"version":2,"operation":"append","items":[]}}),
    );
    f.log(A, &rows);
    let out = f.run(&["recent"]);
    assert_eq!(out.total, 0);
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("history_mutation 版本"))
    );
}

#[test]
fn environment_home_resolution_and_no_cache_do_not_write_source_files() {
    let f = Fixture::new();
    let path = f.log(A, &canonical(A, "环境目录提问", "环境目录回答"));
    let before = fs::read(&path).unwrap();
    for direct in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command.args(["recent", "--provider", "traecli", "--no-cache", "--json"]);
        command.env_remove("TRAECLI_HOME").env_remove("TRAE_HOME");
        if direct {
            command.env("TRAECLI_HOME", f.home());
        } else {
            command.env("TRAE_HOME", f.0.path());
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["source"], "local_traecli_rollout");
        assert_eq!(value["total"], 1);
        assert_eq!(value["stats"]["cachedFiles"], 0);
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(f.home()).unwrap().count(), 1);
    assert!(!f.0.path().join("cache").exists());
}
