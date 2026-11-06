use clap::Parser;
use serde_json::json;
use session_finder::{
    cli::{Cli, Request},
    model::{Provider, Session},
    query::{self, Event},
};
use std::{fs, process::Command};
use tempfile::TempDir;

const ID: &str = "01000000-0000-4000-8000-000000000001";

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sessions")).unwrap();
    for (id, time, text) in [
        (
            ID.to_string(),
            "2026-11-01T00:00:01Z",
            "Modern ＡＢＣ Cafe\u{301} 切到主分支",
        ),
        (
            "02000000-0000-4000-8000-000000000002".into(),
            "2026-11-02T00:00:01Z",
            "Modern second",
        ),
    ] {
        let rows = [
            json!({"type":"session_meta","payload":{"id":id,"timestamp":time,"cwd":"/fixture/project","source":"cli"}}),
            json!({"type":"event_msg","timestamp":time,"payload":{"type":"agent_message","message":text}}),
        ];
        fs::write(
            root.path().join(format!("sessions/rollout-{id}.jsonl")),
            rows.map(|r| r.to_string()).join("\n") + "\n",
        )
        .unwrap();
    }
    root
}

fn run(root: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_session-finder"))
        .args(["--provider", "codex", "--home"])
        .arg(root.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn terminal_links_keep_full_ids_and_json_stays_undecorated() {
    let root = fixture();
    let args = [
        "search", "modren", "--id", ID, "--color", "always", "--links", "always",
    ];
    let decorated = run(&root, &args);
    assert!(decorated.status.success());
    let text = String::from_utf8(decorated.stdout).unwrap();
    assert!(text.contains("Session ID: "));
    assert!(text.contains(&format!(
        "\x1b]8;;codex://threads/{ID}\x1b\\\x1b[38;5;163m{ID}\x1b[0m\x1b]8;;\x1b\\"
    )));
    assert!(text.contains("\x1b[1;38;5;129mModern\x1b[0m"));
    assert!(!text.contains("\x1b[1;33m"));
    assert!(!text.contains("\x1b[36m"));
    assert!(!text.contains("\x1b[2m"));
    let plain = run(&root, &["search", "modren", "--id", ID]);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert!(text.contains(&format!("Session ID: {ID}")));
    assert!(!text.contains('\x1b'));
    let mut args = args.to_vec();
    args.push("--json");
    let output = run(&root, &args);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 2);
    assert_eq!(value["sessions"][0]["id"], ID);
    assert!(!output.stdout.contains(&0x1b));
    assert!(output.stderr.is_empty());
    for (query, expected) in [
        ("abc", "ＡＢＣ"),
        ("café", "Cafe\u{301}"),
        ("切换主分支", "切到主分支"),
    ] {
        let output = run(&root, &["search", query, "--id", ID, "--color", "always"]);
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains(&format!("\x1b[1;38;5;129m{expected}\x1b[0m"))
        );
    }
}

#[test]
fn trae_bridge_uses_conversation_id_and_encoded_recorded_cwd() {
    let mut s = Session {
        provider: Provider::Trae,
        id: "0123456789abcdef01234567".into(),
        cwd: Some("/tmp/project 空格&a#b".into()),
        index_source: Some("native_database".into()),
        ..Default::default()
    };
    let url = url::Url::parse(&s.open_url().unwrap()).unwrap();
    assert_eq!(url.scheme(), "trae-cn");
    assert_eq!(url.host_str(), Some("session-finder.trae-session-link"));
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs["id"], s.id);
    assert_eq!(pairs["cwd"], s.cwd.as_deref().unwrap());
    assert_eq!(
        session_finder::text::session_id(url.as_str()).as_deref(),
        Some(s.id.as_str())
    );
    s.cwd = Some(format!("/tmp/project-{ID}"));
    let link = s.open_url().unwrap();
    assert_eq!(
        session_finder::text::session_id(&link).as_deref(),
        Some(s.id.as_str())
    );
    assert!(session_finder::text::session_id(&format!("{link}&id={}", s.id)).is_none());
    s.id.push_str("\x1b]8;;https://example.com");
    assert!(s.open_url().is_none());
    s.id = "0123456789abcdef01234567".into();
    s.cwd = Some("relative".into());
    assert!(s.open_url().is_none());
    s.cwd = Some("/tmp/project".into());
    s.index_source = None;
    assert!(s.open_url().is_none());
    s.provider = Provider::Traecli;
    assert!(s.open_url().is_none());
}

#[test]
fn streamed_page_matches_final_page_and_precedes_cache_save() {
    let root = fixture();
    let request = Request::new(
        Cli::try_parse_from([
            "sf",
            "search",
            "modren",
            "--provider",
            "codex",
            "--home",
            root.path().to_str().unwrap(),
            "--cache-dir",
            root.path().join("cache").to_str().unwrap(),
            "--offset",
            "1",
            "--limit",
            "1",
            "--sort",
            "replied",
        ])
        .unwrap(),
        false,
    )
    .unwrap();
    let mut streamed = Vec::new();
    let mut declared = None;
    let mut saving = false;
    let out = query::execute_observed(&request, &mut |event| {
        match event {
            Event::Results { total, shown } => declared = Some((total, shown)),
            Event::Session { index, session } => {
                assert!(!saving);
                assert_eq!(index, streamed.len());
                assert!(!session.matches.is_empty());
                streamed.push(session.list_json(false, true));
            }
            Event::Phase("Saving index cache") => {
                assert_eq!(streamed.len(), 1);
                saving = true;
            }
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(declared, Some((2, 1)));
    assert!(saving);
    assert_eq!(out.sessions[0].id, ID);
    assert_eq!(
        streamed,
        out.sessions
            .iter()
            .map(|s| s.list_json(false, true))
            .collect::<Vec<_>>()
    );
    let warm = query::execute(&request).unwrap();
    assert_eq!(
        streamed,
        warm.sessions
            .iter()
            .map(|s| s.list_json(false, true))
            .collect::<Vec<_>>()
    );
    let error = query::execute_observed(&request, &mut |event| {
        if let Event::Session { .. } = event {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into());
        }
        Ok(())
    })
    .err()
    .unwrap();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[test]
fn discovery_precedes_next_rollout_read_and_later_better_match_moves_up() {
    let root = fixture();
    let second = "02000000-0000-4000-8000-000000000002";
    let second_path = root.path().join(format!("sessions/rollout-{second}.jsonl"));
    let replacement = fs::read_to_string(&second_path).unwrap();
    fs::write(
        &second_path,
        replacement.replace("Modern second", "unrelated"),
    )
    .unwrap();
    let request = Request::new(
        Cli::try_parse_from([
            "sf",
            "search",
            "modern",
            "--provider",
            "traecli",
            "--traecli-home",
            root.path().to_str().unwrap(),
            "--no-cache",
            "--sort",
            "relevance",
        ])
        .unwrap(),
        false,
    )
    .unwrap();
    // Give the first result a lower fuzzy score than the later exact result.
    let first_path = root.path().join(format!("sessions/rollout-{ID}.jsonl"));
    let first = fs::read_to_string(&first_path).unwrap();
    fs::write(first_path, first.replace("Modern", "Modren")).unwrap();
    let mut live = query::LiveResults::default();
    let mut first_seen = false;
    let mut higher_seen = false;
    let mut complete = false;
    let out = query::execute_incremental(&request, &mut |event| {
        match event {
            Event::Candidate { session } => {
                assert!(!complete);
                live.update(session);
                if session.id == ID {
                    first_seen = true;
                    assert_eq!(live.page(&request).1[0].id, ID);
                    // The adapter has not read the next rollout yet.
                    fs::write(&second_path, &replacement).unwrap();
                } else if session.id == second {
                    assert!(first_seen);
                    assert!(session.match_score > 0.0);
                    let (total, page) = live.page(&request);
                    assert_eq!(total, 2);
                    assert_eq!(page[0].id, second);
                    assert!(page[0].match_score > page[1].match_score);
                    higher_seen = true;
                }
            }
            Event::Results { .. } => complete = true,
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    assert!(first_seen && higher_seen && complete);
    assert_eq!(out.sessions[0].id, second);
    assert_eq!(
        live.page(&request)
            .1
            .iter()
            .map(|s| (&s.id, s.match_score))
            .collect::<Vec<_>>(),
        out.sessions
            .iter()
            .map(|s| (&s.id, s.match_score))
            .collect::<Vec<_>>(),
    );
    let final_out = query::execute(&request).unwrap();
    assert_eq!(
        out.json(&request)["sessions"],
        final_out.json(&request)["sessions"]
    );
}

#[test]
fn incremental_codex_cold_warm_and_uncached_pages_remain_identical() {
    let root = fixture();
    for extra in [vec![], vec![], vec!["--refresh"], vec!["--no-cache"]] {
        let mut args = vec![
            "sf",
            "search",
            "modren",
            "--provider",
            "codex",
            "--home",
            root.path().to_str().unwrap(),
            "--cache-dir",
        ];
        let cache = root.path().join("cache");
        args.push(cache.to_str().unwrap());
        args.extend(["--sort", "relevance", "--offset", "1", "--limit", "1"]);
        args.extend(extra);
        let request = Request::new(Cli::try_parse_from(args).unwrap(), false).unwrap();
        let mut live = query::LiveResults::default();
        let mut discovered = false;
        let mut completed = false;
        let incremental = query::execute_incremental(&request, &mut |event| {
            match event {
                Event::Candidate { session } => {
                    assert!(!completed);
                    live.update(session);
                    discovered |= session.match_score > 0.0;
                }
                Event::Results { .. } => completed = true,
                _ => {}
            }
            Ok(())
        })
        .unwrap();
        assert!(discovered && completed);
        let final_out = query::execute(&request).unwrap();
        assert_eq!(
            incremental.json(&request)["sessions"],
            final_out.json(&request)["sessions"]
        );
        let (total, page) = live.page(&request);
        assert_eq!(total, incremental.total);
        assert_eq!(page[0].id, incremental.sessions[0].id);
        assert_eq!(page[0].match_score, incremental.sessions[0].match_score);
    }
}

#[test]
fn live_catalog_retracts_fuzzy_project_hits_when_exact_project_arrives() {
    let root = fixture();
    let request = Request::new(
        Cli::try_parse_from([
            "sf",
            "recent",
            "--provider",
            "codex",
            "--home",
            root.path().to_str().unwrap(),
            "--no-cache",
            "-p",
            "project",
        ])
        .unwrap(),
        false,
    )
    .unwrap();
    let mut source = request.clone();
    source.options.project = None;
    let out = query::execute(&source).unwrap();
    let mut fuzzy = out.sessions[0].clone();
    fuzzy.project.name = "projects".into();
    let mut direct = out.sessions[1].clone();
    // Even a nonmatching/hidden record establishes exact-project precedence.
    direct.project.name = "project".into();
    direct.match_score = 0.0;
    let mut live = query::LiveResults::default();
    live.update(&fuzzy);
    assert_eq!(live.page(&request).0, 1);
    live.update(&direct);
    assert_eq!(live.page(&request).0, 0);
    direct.match_score = 1.0;
    live.update(&direct);
    assert_eq!(live.page(&request).0, 1);
    assert_eq!(live.page(&request).1[0].id, direct.id);
}
