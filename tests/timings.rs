use serde_json::{Value, json};
use std::{fs, process::Command};
use tempfile::TempDir;

const ID: &str = "01000000-0000-4000-8000-000000000001";

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sessions")).unwrap();
    let records = [
        json!({"type":"session_meta","payload":{"id":ID,"timestamp":"2026-11-01T00:00:00Z","cwd":"/fixture/project","source":"cli"}}),
        json!({"type":"event_msg","timestamp":"2026-11-01T00:00:01Z","payload":{"type":"user_message","message":"timing needle"}}),
    ];
    fs::write(
        root.path().join(format!("sessions/rollout-{ID}.jsonl")),
        records.map(|v| v.to_string()).join("\n") + "\n",
    )
    .unwrap();
    root
}

fn run(root: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_session-finder"))
        .args(["--provider", "codex"])
        .arg("--home")
        .arg(root.path())
        .arg("--cache-dir")
        .arg(root.path().join("cache"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn json_stays_clean_and_timings_are_opt_in_on_stderr() {
    let root = fixture();
    for args in [
        vec!["recent"],
        vec!["search", "needle"],
        vec!["projects"],
        vec!["show", ID],
        vec!["relations", ID],
    ] {
        let mut args = args;
        args.push("--json");
        let quiet = run(&root, &args);
        assert!(quiet.status.success());
        assert!(quiet.stderr.is_empty());
        let json: Value = serde_json::from_slice(&quiet.stdout).unwrap();
        assert!(json["stats"]["elapsedMs"].is_u64());

        args.push("--timings");
        let timed = run(&root, &args);
        assert!(timed.status.success());
        let json: Value = serde_json::from_slice(&timed.stdout).unwrap();
        let stderr = String::from_utf8(timed.stderr).unwrap();
        assert!(stderr.contains(&format!("耗时 {} ms", json["stats"]["elapsedMs"])));
        assert_eq!(stderr.matches("耗时 ").count(), 1);
    }
}

#[test]
fn text_queries_report_read_counts_separately_from_results() {
    let root = fixture();
    for args in [
        vec!["recent"],
        vec!["search", "needle"],
        vec!["projects"],
        vec!["show", ID],
        vec!["relations", ID],
    ] {
        let output = run(&root, &args);
        assert!(output.status.success());
        assert!(!String::from_utf8(output.stdout).unwrap().contains("耗时 "));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stderr.matches("耗时 ").count(), 1);
        assert!(stderr.contains("正文读取 "));
        assert!(stderr.contains("元数据读取 "));
        assert!(stderr.contains("缓存命中 "));
    }
}

#[test]
fn errors_and_nonterminal_browse_do_not_report_success_timings() {
    let root = fixture();
    for args in [
        vec!["show", "ffffffff"],
        vec!["browse", "--timings"],
        vec!["recent", "--sort", "invalid"],
    ] {
        let output = run(&root, &args);
        assert!(!output.status.success());
        assert!(!String::from_utf8(output.stderr).unwrap().contains("耗时 "));
    }
}
