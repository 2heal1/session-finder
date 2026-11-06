use clap::Parser;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use session_finder::{
    cli::{Cli, Request},
    query::{Output, execute},
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
const D: &str = "03000000-0000-4000-8000-000000000004";
const E: &str = "04000000-0000-4000-8000-000000000005";

struct Fixture(TempDir);
impl Fixture {
    fn new() -> Self {
        let f = Self(tempfile::tempdir().unwrap());
        fs::create_dir(f.root().join("sessions")).unwrap();
        fs::create_dir(f.root().join("archived_sessions")).unwrap();
        f
    }
    fn root(&self) -> &Path {
        self.0.path()
    }
    fn path(&self, id: &str) -> PathBuf {
        self.root()
            .join("sessions")
            .join(format!("rollout-2026-11-01T00-00-00-{id}.jsonl"))
    }
    fn add(&self, id: &str, parent: Option<&str>, kind: &str) -> PathBuf {
        let path = self.path(id);
        let mut record = header(id);
        if let Some(parent) = parent {
            if kind == "fork" {
                record["payload"]["forked_from_id"] = parent.into();
            } else {
                record["payload"]["source"] =
                    json!({"subagent":{"thread_spawn":{"parent_thread_id":parent}}});
            }
        }
        fs::write(&path, format!("{record}\n")).unwrap();
        path
    }
    fn req(&self, args: &[&str]) -> Request {
        let mut all = vec![
            "sf".into(),
            "--provider".into(),
            "codex".into(),
            "--home".into(),
            self.root().to_str().unwrap().into(),
            "--cache-dir".into(),
            self.root().join("cache").to_str().unwrap().into(),
        ];
        all.extend(args.iter().map(|s| s.to_string()));
        Request::new(Cli::try_parse_from(all).unwrap(), false).unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        execute(&self.req(args)).unwrap()
    }
    fn graph(&self, args: &[&str]) -> Value {
        let request = self.req(args);
        execute(&request).unwrap().json(&request)
    }
    fn db(&self) -> Connection {
        let conn = Connection::open(self.root().join("state_5.sqlite")).unwrap();
        conn.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY,created_at INTEGER,
            cwd TEXT,source TEXT,git_origin_url TEXT,git_branch TEXT,title TEXT,thread_source TEXT);
            CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT PRIMARY KEY,status TEXT);").unwrap();
        conn
    }
}
fn header(id: &str) -> Value {
    json!({"type":"session_meta","timestamp":"2026-11-01T00:00:00Z",
        "payload":{"id":id,"cwd":"/test","source":"vscode","base_instructions":"private instructions must not be cached"}})
}
fn row(conn: &Connection, id: &str, source: Value, origin: &str) {
    conn.execute(
        "INSERT INTO threads VALUES(?1,1793491200,'/test',?2,NULL,NULL,'db title',?3)",
        params![id, source.to_string(), origin],
    )
    .unwrap();
}
fn edge(conn: &Connection, parent: &str, child: &str) {
    conn.execute(
        "INSERT INTO thread_spawn_edges VALUES(?1,?2,'closed')",
        params![parent, child],
    )
    .unwrap();
}
fn ids(graph: &Value) -> Vec<&str> {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect()
}

#[test]
fn reverse_forks_and_spawns_preserve_types_and_evidence() {
    let f = Fixture::new();
    f.add(A, None, "");
    f.add(B, Some(A), "fork");
    f.add(C, Some(A), "spawn");
    let graph = f.graph(&["relations", A]);
    assert_eq!(ids(&graph), vec![A, B, C]);
    let edges = graph["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0]["kind"], "fork");
    assert_eq!(edges[0]["from"], A);
    assert_eq!(edges[0]["to"], B);
    assert_eq!(edges[0]["evidence"][0]["line"], 1);
    assert_eq!(edges[0]["evidence"][0]["field"], "/payload/forked_from_id");
    assert_eq!(edges[1]["kind"], "spawn");
    assert_eq!(edges[1]["to"], C);
    assert_eq!(graph["stats"]["parsedFiles"], 0);
    assert_eq!(graph["coverage"]["toolsScanned"], false);
}

#[test]
fn both_traverses_ancestors_and_descendants_without_siblings() {
    let f = Fixture::new();
    f.add(A, None, "");
    f.add(B, Some(A), "fork");
    f.add(C, Some(A), "spawn"); // sibling of B
    f.add(D, Some(B), "spawn");
    f.add(E, Some(D), "fork");
    let one = f.graph(&["relations", B]);
    assert_eq!(ids(&one), vec![A, B, D]);
    assert_eq!(one["truncated"], true);
    assert_eq!(one["frontier"][0]["id"], D);
    assert_eq!(one["frontier"][0]["omittedEdges"], 1);
    let all = f.graph(&["relations", B, "--depth", "100"]);
    assert_eq!(ids(&all), vec![A, B, D, E]);
    assert_eq!(all["truncated"], false);
    assert_eq!(
        ids(&f.graph(&["relations", B, "--depth", "3", "--direction", "parents"])),
        vec![A, B]
    );
    assert_eq!(
        ids(&f.graph(&["relations", B, "--depth", "3", "--direction", "children"])),
        vec![B, D, E]
    );
}

#[test]
fn native_table_finds_missing_logs_and_referenced_only_nodes() {
    let f = Fixture::new();
    let db = f.db();
    row(&db, B, json!("subagent"), "subagent");
    edge(&db, A, B);
    edge(&db, A, C); // no threads row for A or C
    let graph = f.graph(&["relations", A]);
    assert_eq!(ids(&graph), vec![A, B, C]);
    assert_eq!(graph["coverage"]["spawnTableAvailable"], true);
    assert_eq!(graph["nodes"][1]["logStatus"], "missing");
    assert_eq!(graph["nodes"][1]["inDatabase"], true);
    assert_eq!(graph["nodes"][2]["inDatabase"], false);
    assert_eq!(
        graph["edges"][0]["evidence"][0]["table"],
        "thread_spawn_edges"
    );
    assert_eq!(graph["edges"][0]["evidence"][0]["status"], "closed");
    assert_eq!(graph["warnings"], json!([]));
}

#[test]
fn database_source_fills_table_gaps_and_changes_are_not_cached() {
    let f = Fixture::new();
    let db = f.db();
    row(
        &db,
        B,
        json!({"subagent":{"thread_spawn":{"parent_thread_id":A}}}),
        "subagent",
    );
    f.add(B, Some(A), "spawn");
    let one = f.graph(&["relations", A]);
    assert_eq!(one["edges"][0]["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(one["stats"]["headerFiles"], 1);
    db.execute(
        "UPDATE threads SET source=?1",
        [json!({"subagent":{"spawn":{"parentThreadId":C}}}).to_string()],
    )
    .unwrap();
    let two = f.graph(&["relations", B]);
    assert_eq!(two["stats"]["cachedFiles"], 1);
    assert_eq!(two["edges"][0]["from"], C);
    assert_eq!(two["conflicts"][0]["selectedParentId"], C);
    assert_eq!(
        two["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == B)
            .unwrap()["parentStatus"],
        "conflicting"
    );
}

#[test]
fn native_table_has_priority_and_same_rank_conflicts_do_not_make_edges() {
    let f = Fixture::new();
    let db = f.db();
    row(
        &db,
        B,
        json!({"subagent":{"thread_spawn":{"parent_thread_id":C}}}),
        "subagent",
    );
    edge(&db, A, B);
    f.add(B, Some(D), "spawn");
    let graph = f.graph(&["relations", B]);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
    assert_eq!(graph["edges"][0]["from"], A);
    assert_eq!(
        graph["conflicts"][0]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(graph["conflicts"][0]["selectedParentId"], A);

    let f = Fixture::new();
    let path = f.add(B, Some(A), "fork");
    let mut other = header(B);
    other["payload"]["forked_from_id"] = C.into();
    let archive = f
        .root()
        .join("archived_sessions")
        .join(path.file_name().unwrap())
        .with_extension("jsonl.zst");
    fs::write(
        archive,
        zstd::stream::encode_all(format!("{other}\n").as_bytes(), 1).unwrap(),
    )
    .unwrap();
    let graph = f.graph(&["relations", B]);
    assert_eq!(graph["edges"], json!([]));
    assert_eq!(graph["conflicts"][0]["selectedParentId"], Value::Null);
    assert_eq!(graph["nodes"][0]["paths"].as_array().unwrap().len(), 2);
    assert_eq!(graph["nodes"][0]["parentStatus"], "conflicting");
}

#[test]
fn identical_copies_deduplicate_edges_but_retain_all_proofs() {
    let f = Fixture::new();
    let path = f.add(B, Some(A), "fork");
    let archive = f
        .root()
        .join("archived_sessions")
        .join(path.file_name().unwrap());
    fs::copy(path, archive).unwrap();
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
    assert_eq!(graph["edges"][0]["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(graph["conflicts"], json!([]));
}

#[test]
fn embedded_parent_metadata_and_tool_mentions_never_become_child_edges() {
    let f = Fixture::new();
    let mut inherited = header(A);
    inherited["payload"]["forked_from_id"] = C.into();
    let mut own = header(B);
    own["payload"]["forked_from_id"] = A.into();
    own["payload"]["session_id"] = C.into(); // id takes precedence
    let tool = json!({"type":"response_item","payload":{"type":"function_call_output","output":{"agent_id":D}}});
    fs::write(
        f.path(B),
        format!("{inherited}\n{tool}\n{own}\n{inherited}\n"),
    )
    .unwrap();
    let graph = f.graph(&["relations", B, "--depth", "10"]);
    assert_eq!(ids(&graph), vec![A, B]);
    assert_eq!(graph["edges"][0]["evidence"][0]["line"], 3);
    fs::write(f.path(E), format!("{inherited}\n{tool}\n")).unwrap();
    let graph = f.graph(&["relations", E]);
    assert_eq!(ids(&graph), vec![E]);
    assert_eq!(graph["edges"], json!([]));
    assert_eq!(
        graph["coverage"]["logsWithoutOwnMetadata"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(graph["nodes"][0]["metadataAvailable"], false);
}

#[test]
fn unknown_agent_created_origin_is_visible_without_inventing_a_caller() {
    let f = Fixture::new();
    let mut own = header(A);
    own["payload"]["thread_source"] = "agent_created_thread".into();
    fs::write(f.path(A), format!("{own}\n")).unwrap();
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["nodes"][0]["threadSource"], "agent_created_thread");
    assert_eq!(graph["nodes"][0]["parentStatus"], "unknown");
    assert_eq!(graph["edges"], json!([]));
    let db = f.db();
    row(&db, B, json!("vscode"), "agent_created_thread");
    let graph = f.graph(&["relations", B]);
    assert_eq!(graph["nodes"][0]["parentStatus"], "unknown");
}

#[test]
fn guardian_parent_field_is_a_typed_child_with_its_origin_preserved() {
    let f = Fixture::new();
    let mut own = header(B);
    own["payload"]["source"] = json!({"subagent":{"other":"guardian"}});
    own["payload"]["thread_source"] = "guardian_review".into();
    own["payload"]["parent_thread_id"] = A.into();
    fs::write(f.path(B), format!("{own}\n")).unwrap();
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["edges"][0]["kind"], "spawn");
    assert_eq!(
        graph["edges"][0]["evidence"][0]["field"],
        "/payload/parent_thread_id"
    );
    assert_eq!(graph["nodes"][1]["threadSource"], "guardian_review");
}

#[test]
fn missing_or_incompatible_optional_spawn_table_keeps_source_relationships() {
    let f = Fixture::new();
    let db = f.db();
    row(
        &db,
        B,
        json!({"subagent":{"thread_spawn":{"parent_thread_id":A}}}),
        "subagent",
    );
    db.execute_batch(
        "DROP TABLE thread_spawn_edges; ALTER TABLE threads DROP COLUMN thread_source;",
    )
    .unwrap();
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["coverage"]["spawnTableAvailable"], false);
    assert_eq!(graph["warnings"], json!([]));
    assert_eq!(graph["edges"][0]["from"], A);
    db.execute_batch("CREATE TABLE thread_spawn_edges(unrecognized TEXT);")
        .unwrap();
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["coverage"]["spawnTableAvailable"], false);
    assert_eq!(graph["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(graph["edges"][0]["from"], A);
}

#[test]
fn cycles_terminate_and_depth_reports_unexpanded_edges() {
    let f = Fixture::new();
    f.add(A, Some(B), "spawn");
    f.add(B, Some(A), "spawn");
    let one = f.graph(&["relations", A, "--direction", "children"]);
    assert_eq!(one["edges"].as_array().unwrap().len(), 1);
    assert_eq!(one["truncated"], true);
    let all = f.graph(&["relations", A, "--depth", "100"]);
    assert_eq!(all["edges"].as_array().unwrap().len(), 2);
    assert_eq!(all["truncated"], false);
    assert_eq!(ids(&all), vec![A, B]);
}

#[test]
fn relation_cache_handles_append_refresh_delete_and_preserves_body_cache() {
    let f = Fixture::new();
    let path = f.add(A, None, "");
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{}", json!({"type":"event_msg","timestamp":"2026-11-02T00:00:00Z","payload":{"type":"agent_message","message":"searchable"}})).unwrap();
    f.run(&["search", "searchable"]);
    let one = f.graph(&["relations", A]);
    assert_eq!(one["stats"]["headerFiles"], 1);
    let two = f.graph(&["relations", A]);
    assert_eq!(two["stats"]["cachedFiles"], 1);
    assert_eq!(two["stats"]["headerFiles"], 0);
    assert_eq!(f.run(&["search", "searchable"]).stats.parsed_files, 0);
    assert_eq!(
        f.graph(&["relations", A, "--refresh"])["stats"]["headerFiles"],
        1
    );

    // Missing own header is negative-cached, then invalidated when it is appended.
    fs::write(f.path(B), format!("{}\n", header(A))).unwrap();
    assert_eq!(f.graph(&["relations", A])["edges"], json!([]));
    let mut file = fs::OpenOptions::new().append(true).open(f.path(B)).unwrap();
    let mut own = header(B);
    own["payload"]["forked_from_id"] = A.into();
    writeln!(file, "{own}").unwrap();
    let three = f.graph(&["relations", A]);
    assert_eq!(three["edges"].as_array().unwrap().len(), 1);
    assert_eq!(three["edges"][0]["evidence"][0]["line"], 2);
    fs::remove_file(f.path(B)).unwrap();
    assert_eq!(f.graph(&["relations", A])["edges"], json!([]));

    let cache_path = fs::read_dir(f.root().join("cache"))
        .unwrap()
        .map(Result::unwrap)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
        .unwrap();
    let cache = Connection::open(cache_path).unwrap();
    let headers: Vec<String> = cache
        .prepare("SELECT header FROM relation_headers")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(headers.len(), 1);
    assert!(!headers[0].contains("private instructions"));
    assert!(!headers[0].contains("searchable"));
}

#[test]
fn relation_queries_do_not_read_bodies_or_apply_default_limit() {
    let f = Fixture::new();
    f.add(A, None, "");
    for n in 1..=30 {
        let id = format!("abcdef00-0000-4000-8000-{n:012}");
        let path = f.add(&id, Some(A), "spawn");
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(&[0xff; 10000])
            .unwrap();
    }
    let graph = f.graph(&["relations", A]);
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 31);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 30);
    assert_eq!(graph["stats"]["parsedFiles"], 0);
    assert_eq!(graph["warnings"], json!([]));
}

#[test]
fn unreadable_compression_keeps_database_edges_and_no_cache_writes_nothing() {
    let f = Fixture::new();
    let db = f.db();
    edge(&db, A, B);
    fs::write(f.path(B).with_extension("jsonl.zst"), b"broken").unwrap();
    let graph = f.graph(&["relations", A, "--no-cache"]);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
    assert_eq!(graph["nodes"][1]["logStatus"], "unreadable");
    assert_eq!(
        graph["coverage"]["unreadableLogs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!f.root().join("cache").exists());
}

#[test]
fn prefix_ambiguity_alias_and_argument_validation() {
    let f = Fixture::new();
    f.add(A, None, "");
    f.add(B, Some(A), "fork");
    assert_eq!(f.run(&["show", "--id", A]).sessions[0].id, A);
    let err = execute(&f.req(&["relations", "01000000"]))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("matches 2"));
    assert_eq!(
        f.graph(&["related", "--id", &format!("codex://threads/{A}")])["rootId"],
        A
    );
    for args in [
        vec!["sf", "relations"],
        vec!["sf", "relations", A, "-p", "test"],
        vec!["sf", "relations", A, "--since", "7d"],
        vec!["sf", "relations", A, "-n", "1"],
        vec!["sf", "relations", A, "--offset", "10"],
        vec!["sf", "relations", A, "--id", B],
        vec!["sf", "show", A, "--id", B],
    ] {
        assert!(Request::new(Cli::try_parse_from(args).unwrap(), false).is_err());
    }
    for depth in ["0", "101"] {
        assert!(Cli::try_parse_from(["sf", "relations", A, "--depth", depth]).is_err());
    }
}

#[test]
fn cli_json_is_machine_readable_and_terminal_shows_typed_edges() {
    let f = Fixture::new();
    f.add(B, Some(A), "fork");
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
        command
            .args(["relations", A, "--home"])
            .arg(f.root())
            .arg("--no-cache");
        if json {
            command.arg("--json");
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if json {
            let value: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(value["schemaVersion"], 1);
            assert_eq!(value["source"], "local_codex_metadata");
            assert_eq!(value["rootId"], A);
            assert!(value.get("sessions").is_none());
        } else {
            assert!(
                String::from_utf8(out.stdout)
                    .unwrap()
                    .contains(&format!("{A} --fork--> {B}"))
            );
        }
    }
}
