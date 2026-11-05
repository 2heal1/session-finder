use clap::Parser;
use serde_json::{Value, json};
use session_finder::{
    cli::{Cli, Request},
    model::{Provider, Scope},
    query::execute,
    text,
    trae::{convert_queries, convert_turn, parse_index},
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

const CONVERSATION: &str = "010000000000400080000001";
const TURN: &str = "010000000000400080000002";
const CODEX: &str = "01000000-0000-4000-8000-000000000001";

fn index(question: &str) -> String {
    json!({"conversation_id":CONVERSATION,"sessions":[{"session_id":TURN,"user_query":question}]})
        .to_string()
}

fn request(root: &Path, args: &[&str]) -> Request {
    let mut argv = vec![
        "session-finder".into(),
        "--home".into(),
        root.display().to_string(),
        "--cache-dir".into(),
        root.join("cache").display().to_string(),
        "--trae-index".into(),
        root.join("index.jsonl").display().to_string(),
        "--traecli-home".into(),
        root.join("traecli").display().to_string(),
        "--provider".into(),
        "all".into(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    Request::new(Cli::try_parse_from(argv).unwrap(), false).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("traecli")).unwrap();
    fs::write(
        root.path().join("index.jsonl"),
        index("threadline shared search"),
    )
    .unwrap();
    fs::create_dir(root.path().join("sessions")).unwrap();
    let messages = [
        json!({"type":"session_meta","payload":{"id":CODEX,"timestamp":"2026-11-01T00:00:00Z","cwd":"/tmp/example","source":"cli"}}),
        json!({"type":"message","role":"user","content":"threadline shared search"}),
        json!({"type":"message","role":"assistant","timestamp":"2026-11-01T00:00:00Z","content":"codex-only response"}),
    ];
    fs::write(
        root.path()
            .join("sessions")
            .join(format!("rollout-{CODEX}.jsonl")),
        messages
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    root
}

#[test]
fn shared_scoring_global_pagination_and_namespaces() {
    let f = fixture();
    let req = request(f.path(), &["search", "thredline", "--sort", "relevance"]);
    let out = execute(&req).unwrap();
    assert_eq!(out.total, 2);
    assert_eq!(
        out.sessions
            .iter()
            .filter(|s| s.provider == Provider::Trae)
            .count(),
        1
    );
    assert!(out.sessions.iter().all(|s| s.match_score > 0.0));
    let trae = out
        .sessions
        .iter()
        .find(|s| s.provider == Provider::Trae)
        .unwrap();
    assert_eq!(trae.id, CONVERSATION);
    assert_eq!(trae.scope, Scope::IndexedUserQueries);
    assert_eq!(trae.user_message_count, Some(1));
    assert_eq!(trae.message_count, None);
    assert_eq!(trae.reply_count, None);
    assert_eq!(trae.created_at, None);
    assert_eq!(trae.last_reply_at, None);
    assert_eq!(trae.cwd, None);
    assert!(!trae.details_available);
    assert_eq!(trae.parent_session_id, None);
    assert_eq!(trae.forked_from_id, None);
    assert!(!trae.is_subagent);
    let warm = execute(&req).unwrap();
    assert!(warm.stats.cached_files >= 2);
    let page = execute(&request(
        f.path(),
        &[
            "search",
            "thredline",
            "--sort",
            "relevance",
            "--offset",
            "1",
            "-n",
            "1",
        ],
    ))
    .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.sessions.len(), 1);
    assert_eq!(page.sessions[0].provider, out.sessions[1].provider);
    assert_eq!(
        execute(&request(f.path(), &["search", "thredline", "--exact"]))
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        execute(&request(f.path(), &["recent", "--since", "2026-10-01"]))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        execute(&request(
            f.path(),
            &["recent", "--sort", "created", "-n", "1", "--offset", "1"]
        ))
        .unwrap()
        .sessions[0]
            .provider,
        Provider::Trae
    );
    let error = execute(&request(f.path(), &["show", "01000000"]))
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("--provider"));
    assert_eq!(
        execute(&request(f.path(), &["recent", "--id", TURN]))
            .unwrap()
            .total,
        0
    );
    assert_eq!(out.json(&req)["source"], "local_session_providers");
}

#[test]
fn conversion_preserves_repeated_questions_and_cleans_injections() {
    let row = format!(
        "{}\n{}",
        index("<system-reminder>injected-secret</system-reminder>repeat"),
        json!({"conversation_id":CONVERSATION,"sessions":[
            {"session_id":TURN,"user_query":"repeat"},
            {"session_id":"010000000000400080000003","user_query":"repeat"}]})
    );
    let rows = parse_index(&row).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sessions.len(), 2);
    let parsed = convert_queries(&rows[0]);
    assert_eq!(parsed.session.user_message_count, Some(2));
    assert!(!parsed.search_text.unwrap().contains("injected"));
    let input = [
        json!({"role":"system","content":"system-secret"}),
        json!({"role":"tool","content":["tool-secret"]}),
        json!({"role":"user","content":["<environment_context>injected-secret</environment_context>","actual question"]}),
        json!({"role":"assistant","content":[{"type":"text","text":"typed reply"},{"type":"image","text":"image-secret"}],"id":"r1"}),
        json!({"role":"assistant","content":["typed reply expanded"],"id":"r1"}),
        json!({"role":"assistant","tool_calls":[{"name":"tool-secret"}]}),
        json!({"role":"user","content":"actual question"}),
        json!({"role":"assistant","content":"actual reply","timestamp":"2026-11-02T00:00:00Z"}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let messages = convert_turn(&input, TURN).unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].text, "actual question");
    assert_eq!(messages[1].text, "typed reply expanded");
    assert_eq!(messages[2].text, "actual question");
    assert_eq!(
        messages[3].timestamp.as_deref(),
        Some("2026-11-02T00:00:00.000Z")
    );
    assert!(messages.iter().all(|m| m.turn_id.as_deref() == Some(TURN)));
    assert!(convert_turn("{broken", TURN).is_err());
    assert!(parse_index(r#"{"conversation_id":"../../escape","sessions":[]}"#).is_err());
    assert_eq!(
        text::session_id(&format!("context://history/{CONVERSATION}/{TURN}.jsonl")).as_deref(),
        Some(CONVERSATION)
    );
}

#[test]
fn native_reader_reuses_node_snapshot_without_credentials() {
    let f = tempfile::tempdir().unwrap();
    let reader = f.path().join("ctx-cli");
    fs::write(&reader, "must never execute this file").unwrap();
    let identity = reader.canonicalize().unwrap().display().to_string();
    let cache = f.path().join("cache");
    fs::create_dir_all(cache.join("trae-query-index")).unwrap();
    let snapshot_path = cache
        .join("trae-query-index")
        .join(format!("{:x}.json", Sha256::digest(identity.as_bytes())));
    fs::write(
        &snapshot_path,
        json!({
            "schemaVersion":1,"reader":identity,"indexedAt":"2026-11-05T00:00:00Z",
            "rows":[serde_json::from_str::<Value>(&index("threadline")).unwrap()]
        })
        .to_string(),
    )
    .unwrap();
    let run = |refresh: bool| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        c.env_remove("CFS_TOKEN")
            .env_remove("CFS_TRANSPORT")
            .env_remove("CFS_IPC_NAME")
            .env_remove("ICUBE_APP_ROOT")
            .args([
                "search",
                "thredline",
                "--provider",
                "trae",
                "--json",
                "--verbose",
                "--trae-ctx",
            ])
            .arg(&reader)
            .arg("--cache-dir")
            .arg(&cache);
        if refresh {
            c.arg("--refresh");
        }
        c.output().unwrap()
    };
    let out = run(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["total"], 1);
    assert_eq!(json["sessions"][0]["indexSource"], "cache");
    assert_eq!(json["sessions"][0]["indexedAt"], "2026-11-05T00:00:00Z");
    let before = fs::read(&snapshot_path).unwrap();
    let failed = run(true);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("TRAE Agent"));
    assert_eq!(fs::read(snapshot_path).unwrap(), before);
}

#[test]
fn provider_cache_cleanup_preserves_other_sources_and_invalidates_changed_export() {
    let f = fixture();
    let all = request(f.path(), &["search", "threadline"]);
    execute(&all).unwrap();
    let mut codex = all.clone();
    codex.options.provider = session_finder::cli::Providers::Codex;
    codex.options.trae_index = None;
    execute(&codex).unwrap();
    let warm = execute(&all).unwrap();
    assert_eq!(warm.total, 2);
    assert!(warm.stats.cached_files >= 2);
    fs::write(f.path().join("index.jsonl"), index("replacement-question")).unwrap();
    assert_eq!(execute(&all).unwrap().total, 1);
    let mut no_cache = all.clone();
    no_cache.options.no_cache = true;
    assert_eq!(execute(&no_cache).unwrap().total, 1);
    assert_eq!(execute(&no_cache).unwrap().stats.cached_files, 0);
}

#[test]
fn synchronized_body_is_searchable_and_showable_through_common_engine() {
    let f = fixture();
    let export = f.path().join("index.jsonl").canonicalize().unwrap();
    let identity = format!("export:{}", export.display());
    let rows = parse_index(&fs::read_to_string(&export).unwrap()).unwrap();
    let row_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&rows[0]).unwrap())
    );
    let transcript_dir = f
        .path()
        .join("cache/trae-transcripts")
        .join(format!("{:x}", Sha256::digest(identity.as_bytes())));
    fs::create_dir_all(&transcript_dir).unwrap();
    let messages = convert_turn(
        "{\"role\":\"assistant\",\"content\":[\"synchronized body needle\"]}",
        TURN,
    )
    .unwrap();
    let body_request = request(f.path(), &["search", "synchronized", "--sort", "relevance"]);
    assert_eq!(execute(&body_request).unwrap().total, 0);
    fs::write(
        transcript_dir.join(format!("{CONVERSATION}.json")),
        json!({
            "schemaVersion":2, "conversationId":CONVERSATION, "indexSignature":row_hash,
            "syncedAt":"2026-11-05T01:00:00Z", "messages":messages
        })
        .to_string(),
    )
    .unwrap();
    let out = execute(&body_request).unwrap();
    assert_eq!(out.total, 1);
    assert_eq!(out.sessions[0].scope, Scope::IndexedConversation);
    assert_eq!(out.sessions[0].reply_count, Some(1));
    let show_request = request(f.path(), &["show", CONVERSATION]);
    let shown = execute(&show_request).unwrap();
    assert_eq!(shown.messages.len(), 1);
    assert_eq!(shown.messages[0].text, "synchronized body needle");
    let output = shown.json(&show_request);
    assert!(output.get("messages").is_none());
    assert_eq!(output["session"]["scope"], "indexed_conversation");
    let file = shown.transcript_file.as_ref().unwrap();
    let transcript = fs::read_to_string(&file.path).unwrap();
    let message: Value = serde_json::from_str(transcript.lines().nth(1).unwrap()).unwrap();
    assert_eq!(message["text"], "synchronized body needle");
    assert_eq!(message["turn_id"], TURN);
    assert_eq!(
        shown.sessions[0].synced_at.as_deref(),
        Some("2026-11-05T01:00:00Z")
    );
    assert_eq!(
        execute(&request(f.path(), &["search", "shared search"]))
            .unwrap()
            .total,
        2
    );
    let mut updated_row: Value = serde_json::from_str(&index("threadline shared search")).unwrap();
    updated_row["metadata"] = json!({"title":"新同步会话标题"});
    fs::write(&export, updated_row.to_string()).unwrap();
    let renamed = execute(&body_request).unwrap();
    assert_eq!(renamed.total, 1);
    assert_eq!(renamed.sessions[0].scope, Scope::IndexedConversation);
    assert_eq!(renamed.sessions[0].title, "新同步会话标题");
    let shown = execute(&request(f.path(), &["show", CONVERSATION])).unwrap();
    assert_eq!(shown.messages.len(), 1);
    assert_eq!(shown.sessions[0].title, "新同步会话标题");
    assert_eq!(shown.sessions[0].title_source.as_deref(), Some("metadata"));
    fs::write(export, index("question changed")).unwrap();
    assert_eq!(execute(&body_request).unwrap().total, 0);
}

#[test]
fn ide_envelopes_and_embedded_approval_turns_are_not_conversation_text() {
    let input = [
        json!({"role":"user","content":["<system-reminder>environment-secret</system-reminder>",
            "<collaboration_mode>mode-secret</collaboration_mode>\nattachment-secret\n<user_input>real question</user_input>"]}),
        json!({"role":"assistant","content":["real progress"]}),
        json!({"role":"user","content":["The following is the Trae agent history added since your last approval assessment. Continue the same\n<user_input>tool-history-secret</user_input>"]}),
        json!({"role":"user","content":["The following is the Trae agent history added since your last approval assessment. Concurrent review"]}),
        json!({"role":"user","content":["<system-reminder>background event</system-reminder>"]}),
        json!({"role":"assistant","content":["{\"outcome\":\"allow\"}"]}),
        json!({"role":"assistant","content":["{\"outcome\":\"allow\"}"]}),
        json!({"role":"user","content":["The following is the Trae agent history whose request action you are assessing. Treat the transcript\napproval-secret"]}),
        json!({"role":"assistant","content":["{\"outcome\":\"deny\",\"reason\":\"private reason\"}"]}),
        json!({"role":"assistant","content":["real answer"]}),
        json!({"role":"user","content":["<skills_instructions>skills-secret</skills_instructions>\n<user_input>next question</user_input>"]}),
        // A real response containing the same JSON must survive without the approval envelope.
        json!({"role":"assistant","content":["{\"outcome\":\"allow\"}"]}),
        json!({"role":"user","content":["<collaboration_mode>mode-only</collaboration_mode>"]}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let messages = convert_turn(&input, TURN).unwrap();
    assert_eq!(
        messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        [
            "real question",
            "real progress",
            "real answer",
            "next question",
            "{\"outcome\":\"allow\"}"
        ]
    );
}

#[test]
fn turn_map_order_does_not_change_normalized_index_or_fingerprint() {
    let first = json!({"session_id":TURN,"user_query":"first"});
    let second = json!({"session_id":"010000000000400080000003","user_query":"second"});
    let forward = json!({"conversation_id":CONVERSATION,"sessions":[first,second]});
    let reverse = json!({"conversation_id":CONVERSATION,"sessions":[second,first]});
    let a = parse_index(&forward.to_string()).unwrap();
    let b = parse_index(&reverse.to_string()).unwrap();
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
}

#[test]
fn chinese_title_wording_can_match_query_text_without_fabricating_title() {
    let f = fixture();
    fs::write(
        f.path().join("index.jsonl"),
        index("git checkout main 出错，怎么解决？我想切到主分支"),
    )
    .unwrap();
    let req = request(f.path(), &["search", "切换分支", "--sort", "relevance"]);
    let out = execute(&req).unwrap();
    assert_eq!(out.total, 1);
    assert_eq!(out.sessions[0].id, CONVERSATION);
    assert!(out.sessions[0].title.contains("切到主分支"));
    assert!(!out.sessions[0].title.contains("切换分支"));
    assert!(
        out.warnings
            .iter()
            .any(|s| s.contains("未包含 IDE 会话标题") && s.contains("1 个仅有索引提问"))
    );
    let warm = execute(&req).unwrap();
    assert_eq!(warm.total, out.total);
    assert_eq!(warm.sessions[0].match_score, out.sessions[0].match_score);
    let mut exact = req.clone();
    exact.options.exact = true;
    let missed = execute(&exact).unwrap();
    assert_eq!(missed.total, 0);
    assert!(
        missed.json(&exact)["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("未包含 IDE 会话标题"))
    );
    let cli = Command::new(env!("CARGO_BIN_EXE_session-finder"))
        .args([
            "search",
            "完全不相关关键词",
            "--provider",
            "trae",
            "--trae-index",
        ])
        .arg(f.path().join("index.jsonl"))
        .arg("--cache-dir")
        .arg(f.path().join("cache"))
        .output()
        .unwrap();
    assert!(cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stdout).contains("匹配 0 个会话"));
    assert!(String::from_utf8_lossy(&cli.stderr).contains("未包含 IDE 会话标题"));
}

fn metadata_index(metadata: Value) -> String {
    let mut row: Value = serde_json::from_str(&index("我想切到主分支")).unwrap();
    row["metadata"] = metadata;
    row.to_string()
}

#[test]
fn true_titles_use_shared_exact_search_with_cold_warm_and_no_cache_parity() {
    let f = fixture();
    let title = "Git Worktree 切换分支冲突";
    fs::write(
        f.path().join("index.jsonl"),
        metadata_index(json!({"title":title,"cwd":"/tmp/core"})),
    )
    .unwrap();
    fs::write(
        f.path().join("session_index.jsonl"),
        json!({"id":CODEX,"thread_name":title}).to_string(),
    )
    .unwrap();
    let req = request(
        f.path(),
        &["search", "切换分支", "--exact", "--sort", "relevance"],
    );
    let cold = execute(&req).unwrap();
    assert_eq!(cold.total, 2);
    assert!(cold.sessions.iter().all(|s| s.title == title));
    // A generated title is enough for both providers; neither body contains it.
    assert_eq!(cold.sessions[0].match_score, cold.sessions[1].match_score);
    let warm = execute(&req).unwrap();
    assert!(warm.stats.cached_files >= 2);
    let mut uncached_req = req.clone();
    uncached_req.options.no_cache = true;
    let uncached = execute(&uncached_req).unwrap();
    for out in [&warm, &uncached] {
        assert_eq!(out.total, cold.total);
        assert_eq!(
            out.sessions.iter().map(|s| s.public()).collect::<Vec<_>>(),
            cold.sessions.iter().map(|s| s.public()).collect::<Vec<_>>()
        );
    }
    let trae = cold
        .sessions
        .iter()
        .find(|s| s.provider == Provider::Trae)
        .unwrap();
    assert_eq!(trae.title_source.as_deref(), Some("metadata"));
    assert_eq!(trae.first_user_preview, "我想切到主分支");
    assert_eq!(trae.project.name, "core");
    assert!(
        cold.warnings
            .iter()
            .any(|s| s.contains("包含 1 个 IDE 会话标题"))
    );
    assert!(
        !cold
            .warnings
            .iter()
            .any(|s| s.contains("未包含 IDE 会话标题"))
    );
}

#[test]
fn title_rename_and_removal_invalidate_warm_search_without_rebuilding_body() {
    let f = fixture();
    let export = f.path().join("index.jsonl");
    fs::write(&export, metadata_index(json!({"title":"旧标题专用词"}))).unwrap();
    let old = request(f.path(), &["search", "旧标题专用词", "--exact"]);
    assert_eq!(execute(&old).unwrap().total, 1);
    assert!(execute(&old).unwrap().stats.cached_files >= 2);
    fs::write(&export, metadata_index(json!({"title":"新标题专用词"}))).unwrap();
    assert_eq!(execute(&old).unwrap().total, 0);
    let new = request(f.path(), &["search", "新标题专用词", "--exact"]);
    assert_eq!(execute(&new).unwrap().total, 1);
    assert!(execute(&new).unwrap().stats.cached_files >= 2);
    fs::write(&export, metadata_index(json!({"title":null}))).unwrap();
    assert_eq!(execute(&new).unwrap().total, 0);
    let fallback = execute(&request(f.path(), &["recent", "--id", CONVERSATION])).unwrap();
    assert_eq!(fallback.sessions[0].title, "我想切到主分支");
    assert_eq!(
        fallback.sessions[0].title_source.as_deref(),
        Some("first_user")
    );
}

#[test]
fn metadata_creation_time_and_project_share_filters_without_inventing_reply_times() {
    let f = fixture();
    fs::write(
        f.path().join("index.jsonl"),
        metadata_index(json!({
            "title":"metadata title", "cwd":"/tmp/core",
            "createdAt":"2026-11-04T08:00:00+08:00", "branch":"feature"
        })),
    )
    .unwrap();
    let out = execute(&request(
        f.path(),
        &[
            "recent",
            "-p",
            "core",
            "--sort",
            "created",
            "--since",
            "2026-11-03T00:00:00Z",
            "--until",
            "2026-11-05T00:00:00Z",
        ],
    ))
    .unwrap();
    assert_eq!(out.total, 1);
    assert_eq!(
        out.sessions[0].created_at.as_deref(),
        Some("2026-11-04T00:00:00.000Z")
    );
    assert_eq!(out.sessions[0].branch.as_deref(), Some("feature"));
    assert_eq!(out.sessions[0].last_reply_at, None);
    assert_eq!(out.sessions[0].last_user_at, None);
    let replies = execute(&request(
        f.path(),
        &[
            "recent",
            "-p",
            "core",
            "--sort",
            "replied",
            "--since",
            "2026-11-03T00:00:00Z",
        ],
    ))
    .unwrap();
    assert_eq!(replies.total, 0);
}

#[test]
fn metadata_only_sessions_and_duplicate_updates_preserve_source_semantics() {
    let first = json!({
        "conversation_id":CONVERSATION,"sessions":[],
        "metadata":{"title":" title without messages ","cwd":" "}
    });
    let no_metadata = json!({"conversation_id":CONVERSATION,"sessions":[]});
    let rows = parse_index(&format!("{first}\n{no_metadata}")).unwrap();
    let parsed = convert_queries(&rows[0]);
    assert_eq!(parsed.session.title, "title without messages");
    assert_eq!(parsed.session.title_source.as_deref(), Some("metadata"));
    assert_eq!(parsed.session.cwd, None);
    assert_eq!(parsed.session.user_message_count, Some(0));
    assert_eq!(parsed.session.message_count, None);
    assert_eq!(parsed.session.created_at, None);
    let cleared = json!({
        "conversation_id":CONVERSATION,"sessions":[],"metadata":{"title":" "}
    });
    let rows = parse_index(&format!("{first}\n{cleared}")).unwrap();
    let parsed = convert_queries(&rows[0]);
    assert_eq!(parsed.session.title, CONVERSATION);
    assert_eq!(parsed.session.title_source.as_deref(), Some("id"));
    assert!(parse_index(&metadata_index(json!({"createdAt":"not a date"}))).is_err());
    assert!(parse_index(&metadata_index(json!({"title":42}))).is_err());
}
