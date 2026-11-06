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
        assert!(stderr.contains(&format!("Elapsed {} ms", json["stats"]["elapsedMs"])));
        assert_eq!(stderr.matches("Elapsed ").count(), 1);
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
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("Elapsed "));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stdout
                .chars()
                .chain(stderr.chars())
                .all(|c| !('\u{4e00}'..='\u{9fff}').contains(&c)),
            "CLI labels must be English: {stdout}\n{stderr}"
        );
        assert_eq!(stderr.matches("Elapsed ").count(), 1);
        assert!(stderr.contains("Body reads "));
        assert!(stderr.contains("Metadata reads "));
        assert!(stderr.contains("Cache hits "));
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
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("Elapsed ")
        );
    }
}

#[test]
fn read_timings_cover_each_provider_and_respect_cache_controls() {
    let root = fixture();
    let index = root.path().join("index.jsonl");
    fs::write(
        &index,
        json!({
            "conversation_id": "010000000000400080000001",
            "sessions": [{"session_id":"010000000000400080000002", "user_query":"timing needle"}]
        })
        .to_string(),
    )
    .unwrap();
    for (provider, source_flag, source) in [
        ("codex", "--home", root.path()),
        ("traecli", "--traecli-home", root.path()),
        ("trae", "--trae-index", index.as_path()),
    ] {
        for mode in ["cold", "warm", "uncached"] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_session-finder"));
            command
                .args(["search", "needle", "--provider", provider, source_flag])
                .arg(source)
                .arg("--cache-dir")
                .arg(root.path().join(format!("cache-{provider}")))
                .arg("--json");
            if mode == "uncached" {
                command.arg("--no-cache");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            let stats = &value["stats"];
            assert_eq!(value["total"], 1);
            let mut sum = 0.0;
            for field in ["bodyReadMs", "metadataReadMs", "cacheReadMs", "otherMs"] {
                let ms = stats[field].as_f64().unwrap();
                assert!(ms.is_finite() && ms >= 0.0, "{provider}/{mode}: {field}");
                sum += ms;
            }
            let elapsed = stats["elapsedMs"].as_f64().unwrap();
            assert!(
                sum >= elapsed && sum < elapsed + 1.01,
                "{provider}/{mode}: {stats}"
            );
            assert!(stats["metadataReadMs"].as_f64().unwrap() > 0.0);
            if mode == "warm" {
                assert_eq!(stats["parsedFiles"], 0);
                assert_eq!(stats["bodyReadMs"], 0.0);
                assert!(stats["cachedFiles"].as_u64().unwrap() > 0);
            } else {
                assert!(stats["bodyReadMs"].as_f64().unwrap() > 0.0);
            }
            if mode == "uncached" {
                assert_eq!(stats["cacheReadMs"], 0.0);
                assert_eq!(stats["cachedFiles"], 0);
            } else {
                assert!(stats["cacheReadMs"].as_f64().unwrap() > 0.0);
            }
        }
    }
}
