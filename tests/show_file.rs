use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const ID: &str = "010000000000400080000001";
const ROLLOUT_ID: &str = "01000000-0000-4000-8000-000000000001";
const PROMPT: &str = "多行问题\n保留引号 \" 和反斜线 \\";

struct Fixture {
    root: tempfile::TempDir,
    writer: Connection,
    reply: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["tmp", "codex/sessions", "cli/sessions"] {
            fs::create_dir_all(root.path().join(name)).unwrap();
        }
        let writer = Connection::open(root.path().join("database.db")).unwrap();
        writer.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE chat_session(session_id TEXT,session_title TEXT,created_at INTEGER,
                 updated_at INTEGER,session_type TEXT,project_id TEXT,worktree_id TEXT,deleted_at INTEGER);
             CREATE TABLE project(project_id TEXT,absolute_path TEXT);
             CREATE TABLE session_project(id INTEGER,session_id TEXT,project_id TEXT,update_at INTEGER);
             CREATE TABLE worktree(worktree_id TEXT,worktree_path TEXT,worktree_branch TEXT);
             CREATE TABLE chat_message(id INTEGER,message_id TEXT PRIMARY KEY,session_id TEXT,
                 message_role TEXT,message_type TEXT,message_index INTEGER,created_at INTEGER,
                 updated_at INTEGER,deleted_at INTEGER,is_archived INTEGER,reply_to_message_id TEXT);
             CREATE TABLE chat_message_general(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             CREATE TABLE chat_message_task(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             CREATE TABLE chat_message_chat(message_id TEXT PRIMARY KEY,content TEXT,updated_at INTEGER,deleted_at INTEGER);
             INSERT INTO chat_session VALUES('010000000000400080000001','原生标题',1793491200,1793491200,'side_chat',NULL,NULL,0);
             INSERT INTO chat_session VALUES('020000000000400080000002','其他会话',1793491200,1793491200,'side_chat',NULL,NULL,0);
             INSERT INTO chat_message VALUES(1,'u','010000000000400080000001','user','general',1,1793491200,1793491200,0,0,NULL);
             INSERT INTO chat_message VALUES(2,'a','010000000000400080000001','assistant','task',2,1793491210,1793491210,0,0,'u');"
        ).unwrap();
        writer
            .execute(
                "INSERT INTO chat_message_general VALUES('u',?1,1793491200,0)",
                [json!([{"type":"text","text_content":PROMPT}]).to_string()],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO chat_message_task VALUES('a','{}',1793491210,0)",
                [],
            )
            .unwrap();
        let fixture = Self {
            root,
            writer,
            reply: format!("{}\n完整正文末尾", "长回复内容 😀 \"\\\n".repeat(12000)),
        };
        fixture.set_reply(&fixture.reply);
        fixture
    }

    fn temp(&self) -> PathBuf {
        self.root.path().join("tmp")
    }

    fn set_reply(&self, reply: &str) {
        self.writer
            .execute(
                "UPDATE chat_message_task SET content=?1",
                [
                    json!({"messages":[{"type":"plan_item","id":"final","plan_item":{
                        "tool_call_info":{"name":"finish","params":{"summary":reply}}
                    }}]})
                    .to_string(),
                ],
            )
            .unwrap();
    }

    fn command(&self, provider: &str, temp: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .env("TMPDIR", temp)
            .args(["--provider", provider])
            .arg("--home")
            .arg(self.root.path().join("codex"))
            .arg("--cache-dir")
            .arg(self.root.path().join("cache"))
            .args(args);
        if matches!(provider, "trae" | "all") {
            command
                .arg("--trae-db")
                .arg(self.root.path().join("database.db"));
        }
        if matches!(provider, "traecli" | "all") {
            command
                .arg("--traecli-home")
                .arg(self.root.path().join("cli"));
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.command("trae", &self.temp(), args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn show(&self, args: &[&str]) -> PathBuf {
        let output = self.run(args);
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.lines().count(), 1);
        PathBuf::from(text.trim())
    }
}

fn rows(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn show_returns_one_file_without_exporting_search_results_or_truncating_text() {
    let f = Fixture::new();
    let search = f.run(&["search", "原生", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&search.stdout).unwrap()["total"],
        1
    );
    assert!(!f.temp().join("session-finder").exists());
    let path = f.show(&["show", "01000000"]);
    assert!(path.is_absolute());
    assert!(path.starts_with(f.temp().canonicalize().unwrap()));
    assert_eq!(path.file_name().unwrap(), format!("{ID}.jsonl").as_str());
    let lines = rows(&path);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["type"], "session_meta");
    assert_eq!(lines[0]["schemaVersion"], 1);
    assert_eq!(lines[0]["session"]["id"], ID);
    assert_eq!(lines[0]["session"]["title"], "原生标题");
    assert_eq!(lines[1]["type"], "message");
    assert_eq!(lines[1]["role"], "user");
    assert_eq!(lines[1]["text"], PROMPT);
    assert_eq!(lines[2]["role"], "assistant");
    assert_eq!(lines[2]["text"], f.reply);
    assert_eq!(lines[2]["phase"], "final");
    assert_eq!(lines[2]["turn_id"], "u");
    assert_eq!(lines[2]["id"], "final:reply");
    assert_eq!(lines[2]["timestamp"], "2026-11-01T00:00:10.000Z");
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn json_and_all_provider_return_manifest_and_reuse_unchanged_file() {
    let f = Fixture::new();
    let path = f.show(&["show", ID]);
    let before = fs::metadata(&path).unwrap();
    let output = f
        .command("all", &f.temp(), &["show", ID, "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session"]["provider"], "trae");
    assert_eq!(value["session"]["messageCount"], 2);
    assert_eq!(value["session"]["scope"], "local_conversation");
    assert_eq!(value["file"]["path"], path.to_str().unwrap());
    assert_eq!(value["file"]["format"], "jsonl");
    assert_eq!(value["file"]["bytes"], before.len());
    assert!(value.get("messages").is_none());
    assert!(value["session"].get("lastReplyPreview").is_none());
    assert!(!String::from_utf8(output.stdout).unwrap().contains(PROMPT));
    assert_eq!(f.show(&["show", ID, "--no-cache"]), path);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
}

#[test]
fn selected_file_refreshes_on_body_or_title_change_and_reappears_after_cleanup() {
    let f = Fixture::new();
    let path = f.show(&["show", ID]);
    f.set_reply("更新后的正文");
    assert_eq!(f.show(&["show", ID]), path);
    assert_eq!(rows(&path)[2]["text"], "更新后的正文");
    f.writer
        .execute(
            "UPDATE chat_session SET session_title='更新后的标题' WHERE session_id=?1",
            [ID],
        )
        .unwrap();
    assert_eq!(f.show(&["show", ID, "--refresh"]), path);
    assert_eq!(rows(&path)[0]["session"]["title"], "更新后的标题");
    fs::remove_file(&path).unwrap();
    assert_eq!(f.show(&["show", ID]), path);
    assert_eq!(rows(&path)[2]["text"], "更新后的正文");
}

#[test]
fn same_id_in_different_databases_has_separate_files() {
    let first = Fixture::new();
    let second = Fixture::new();
    second.set_reply("另一个数据库的正文");
    let first_path = first.show(&["show", ID]);
    let output = second
        .command("trae", &first.temp(), &["show", ID])
        .output()
        .unwrap();
    assert!(output.status.success());
    let second_path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert_ne!(first_path, second_path);
    assert_eq!(first_path.file_name(), second_path.file_name());
    assert_eq!(rows(&first_path)[2]["text"], first.reply);
    assert_eq!(rows(&second_path)[2]["text"], "另一个数据库的正文");
}

#[test]
fn export_failure_reports_an_error_without_printing_conversation_body() {
    let f = Fixture::new();
    fs::write(f.temp().join("session-finder"), "blocked").unwrap();
    for format in [Vec::new(), vec!["--json"]] {
        let mut args = vec!["show", ID];
        args.extend(format);
        let output = f.command("trae", &f.temp(), &args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("Cannot create temporary TRAE conversation file"));
        assert!(!error.contains(PROMPT));
        assert!(!error.contains("长回复内容"));
    }
}

#[test]
fn concurrent_show_calls_publish_complete_jsonl() {
    let f = Fixture::new();
    let children: Vec<_> = (0..4)
        .map(|_| {
            f.command("trae", &f.temp(), &["show", ID, "--no-cache"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut paths = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert_eq!(rows(&path)[2]["text"], f.reply);
        paths.push(path);
    }
    assert!(paths.iter().all(|path| path == &paths[0]));
    assert_eq!(fs::read_dir(paths[0].parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn codex_and_traecli_still_print_body_and_inline_json_messages() {
    let f = Fixture::new();
    let messages = [
        json!({"type":"session_meta","payload":{"id":ROLLOUT_ID,"timestamp":"2026-11-01T00:00:00Z","source":"cli"}}),
        json!({"type":"message","role":"user","content":"a prompt"}),
        json!({"type":"message","role":"assistant","content":"existing inline reply"}),
    ];
    let text = messages
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    for (provider, home) in [("codex", "codex"), ("traecli", "cli")] {
        fs::write(
            f.root
                .path()
                .join(format!("{home}/sessions/rollout-{ROLLOUT_ID}.jsonl")),
            &text,
        )
        .unwrap();
        let output = f
            .command(provider, &f.temp(), &["show", ROLLOUT_ID])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("existing inline reply")
        );
        let output = f
            .command(provider, &f.temp(), &["show", ROLLOUT_ID, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["messages"][1]["text"], "existing inline reply");
        assert!(value.get("file").is_none());
    }
    assert!(!f.temp().join("session-finder").exists());
}
