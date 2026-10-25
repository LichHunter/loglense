//! CLI integration tests (T12): the real `loglense` binary against real
//! wrapped commands in an isolated project (own HOME, fake git root, no
//! LOGLENSE_STATE_DIR).

use std::time::{Duration, Instant};

use serde_json::Value;

mod common;
use common::{assert_clean_failure, describe, Project};

#[test]
fn version_flag_prints_name_and_version() {
    let project = Project::new();
    let out = project.cmd(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("loglense {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn prefix_dispatch_runs_both_bare_and_double_dash_forms() {
    let project = Project::new();
    for args in [
        &["sh", "-c", "echo hi"][..],
        &["--", "sh", "-c", "echo hi"][..],
    ] {
        let out = project.cmd(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", describe(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("run "), "{stdout}");
        assert!(stdout.contains("started (pid "), "{stdout}");
    }
    assert!(
        project.root.join(".loglense/runs").is_dir(),
        "store created in cwd"
    );
    let ls = project.json(&["ls"], 0);
    assert_eq!(ls["total"], 2);
}

#[test]
fn prefix_form_keeps_wrapped_command_flags_away_from_loglense() {
    let project = Project::new();
    let out = project.cmd(&["--json", "sh", "-c", "echo --json --max 3"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let started: Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = started["run_id"].as_str().unwrap();
    project.cmd(&["wait", id, "--timeout", "10"]);
    let tail = project.json(&["tail", id, "--stream", "out"], 0);
    assert_eq!(tail["lines"][0]["text"], "--json --max 3");
}

#[test]
fn ls_lists_newest_first_with_counts_and_exit_codes() {
    let project = Project::new();
    let failing = project.finished(&["sh", "-c", "echo 'ERROR boom'; echo 'warning: meh'; exit 3"]);
    let passing = project.finished(&["true"]);
    let ls = project.json(&["ls"], 0);
    let runs = ls["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["run_id"], passing.as_str(), "newest first");
    assert_eq!(runs[1]["run_id"], failing.as_str());
    assert_eq!(runs[1]["status"], "exited");
    assert_eq!(runs[1]["exit_code"], 3);
    assert_eq!(runs[1]["counts"]["error"], 1);
    assert_eq!(runs[1]["counts"]["warning"], 1);
    assert_eq!(runs[0]["counts"]["error"], 0);
    let capped = project.json(&["ls", "--max", "1"], 0);
    assert_eq!(capped["shown"], 1);
    assert_eq!(capped["total"], 2);
    let filtered = project.json(&["ls", "--status", "exited"], 0);
    assert_eq!(filtered["total"], 2);
    let human = String::from_utf8_lossy(&project.cmd(&["ls"]).stdout).into_owned();
    assert!(human.contains("exited 3"), "{human}");
}

#[test]
fn show_reports_summary_and_error_templates_first() {
    let project = Project::new();
    let script = "for i in 1 2 3 4 5; do echo \"compiling unit $i\"; done; \
                  echo 'warning: slow'; echo 'ERROR db timeout 1'; echo 'ERROR db timeout 2'; exit 1";
    let id = project.finished(&["sh", "-c", script]);
    let show = project.json(&["show", &id], 0);
    assert_eq!(show["state"], "exited");
    assert_eq!(show["meta"]["exit_code"], 1);
    assert_eq!(show["summary"]["severities"]["error"], 2);
    let templates = show["templates"].as_array().unwrap();
    assert_eq!(
        templates.len(),
        2,
        "only error/warning templates: {templates:?}"
    );
    assert_eq!(templates[0]["severity"], "error");
    assert_eq!(
        templates[0]["count"], 2,
        "both timeouts dedupe into one template"
    );
    assert_eq!(templates[1]["severity"], "warning");
    assert!(show["summary"].get("templates").is_none(), "not duplicated");
    let capped = project.json(&["show", &id, "--max", "1"], 0);
    assert_eq!(capped["templates"].as_array().unwrap().len(), 1);
    let human = String::from_utf8_lossy(&project.cmd(&["show", &id]).stdout).into_owned();
    assert!(
        human.contains("exited 1") && human.contains("ERROR db timeout 1"),
        "{human}"
    );
}

#[test]
fn errors_and_warnings_filter_and_cap_oldest_first() {
    let project = Project::new();
    let script =
        "i=1; while [ $i -le 60 ]; do echo \"ERROR item $i\"; i=$((i+1)); done; echo 'WARNING one'";
    let id = project.finished(&["sh", "-c", script]);
    let errors = project.json(&["errors", &id], 0);
    assert_eq!(errors["total"], 60);
    assert_eq!(errors["shown"], 50, "default cap");
    assert_eq!(
        errors["events"][0]["message"], "ERROR item 1",
        "oldest first"
    );
    let capped = project.json(&["errors", &id, "--max", "5"], 0);
    assert_eq!(capped["events"].as_array().unwrap().len(), 5);
    let warnings = project.json(&["warnings", &id], 0);
    assert_eq!(warnings["total"], 1);
    assert_eq!(warnings["events"][0]["message"], "WARNING one");
    let human = String::from_utf8_lossy(&project.cmd(&["errors", &id]).stdout).into_owned();
    assert!(human.contains("(50 of 60 errors shown"), "{human}");
}

#[test]
fn tail_bounds_lines_per_stream_and_merged() {
    let project = Project::new();
    // The pause makes the stderr line a later pipe read than every stdout
    // line; merged order is exact only up to read granularity (D-007).
    let script =
        "i=1; while [ $i -le 150 ]; do echo \"out $i\"; i=$((i+1)); done; sleep 0.2; echo 'err last' >&2";
    let id = project.finished(&["sh", "-c", script]);
    let merged = project.json(&["tail", &id], 0);
    let lines = merged["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 100, "default cap");
    assert_eq!(merged["truncated"], true);
    assert!(lines
        .iter()
        .any(|l| l["stream"] == "err" && l["text"] == "err last"));
    let out = project.json(&["tail", &id, "--stream", "out", "--lines", "3"], 0);
    let texts: Vec<&str> = out["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["out 148", "out 149", "out 150"]);
    let err = project.json(&["tail", &id, "--stream", "err"], 0);
    assert_eq!(err["lines"][0]["text"], "err last");
    assert_eq!(err["truncated"], false);
}

#[test]
fn tail_follow_is_rejected() {
    let project = Project::new();
    let id = project.finished(&["true"]);
    assert_clean_failure(&project.cmd(&["tail", &id, "--follow"]), 2);
}

#[test]
fn wait_mirrors_outcome_and_times_out_with_124() {
    let project = Project::new();
    let ok = project.start(&["true"]);
    assert_eq!(
        project.cmd(&["wait", &ok, "--timeout", "10"]).status.code(),
        Some(0)
    );
    let failed = project.start(&["sh", "-c", "exit 4"]);
    let waited = project.json(&["wait", &failed, "--timeout", "10"], 1);
    assert_eq!(waited["status"], "exited");
    assert_eq!(waited["exit_code"], 4);
    assert_eq!(waited["timed_out"], false);

    let slow = project.start(&["sleep", "5"]);
    let begun = Instant::now();
    let timed_out = project.json(&["wait", &slow, "--timeout", "1"], 124);
    assert!(
        begun.elapsed() < Duration::from_secs(3),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(timed_out["timed_out"], true);
    assert_eq!(timed_out["status"], "running");
    assert!(timed_out.get("exit_code").is_none());
    project.cmd(&["kill", &slow]);
}

#[test]
fn kill_signals_then_reports_already_terminal() {
    let project = Project::new();
    let id = project.start(&["sleep", "300"]);
    let killed = project.json(&["kill", &id], 0);
    assert_eq!(killed["action"], "signaled");
    let waited = project.json(&["wait", &id, "--timeout", "10"], 1);
    assert_eq!(waited["status"], "signaled");
    let again = project.json(&["kill", &id], 0);
    assert_eq!(again["action"], "already-terminal");
}

#[test]
fn init_creates_store_and_gitignore_entry_idempotently() {
    let project = Project::new();
    std::fs::write(project.root.join(".gitignore"), "/target").unwrap();
    let first = project.json(&["init"], 0);
    let actions: Vec<&str> = first["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["created", "created", "appended"]);
    assert_eq!(
        std::fs::read_to_string(project.root.join(".loglense/.gitignore")).unwrap(),
        "*\n"
    );
    let second = project.json(&["init"], 0);
    let actions: Vec<&str> = second["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["exists", "exists", "unchanged"]);
    assert_eq!(
        std::fs::read_to_string(project.root.join(".gitignore")).unwrap(),
        "/target\n.loglense/\n",
        "appended once, after a newline fix-up"
    );
}

#[test]
fn init_outside_git_touches_no_repository_gitignore() {
    let project = Project::new();
    let plain = project.root.parent().unwrap().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let out = project.cmd_in(&plain, &["init"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("skipped"));
    assert!(!plain.join(".gitignore").exists());
    assert!(plain.join(".loglense").is_dir());
}

#[test]
fn unknown_and_malformed_run_ids_fail_cleanly_with_2() {
    let project = Project::new();
    project.finished(&["true"]);
    for id in ["20991231-ZZZZZZ", "bogus", "../../etc"] {
        for verb in ["show", "errors", "warnings", "tail", "wait", "kill"] {
            assert_clean_failure(&project.cmd(&[verb, id]), 2);
        }
    }
}

#[test]
fn readers_without_a_store_fail_cleanly_and_create_nothing() {
    let project = Project::new();
    assert_clean_failure(&project.cmd(&["ls"]), 2);
    assert!(!project.root.join(".loglense").exists());
}

#[test]
fn corrupt_run_is_warned_and_skipped_while_the_rest_lists() {
    let project = Project::new();
    let good = project.finished(&["true"]);
    let bad = project.root.join(".loglense/runs/20261025-AAAAAA");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("meta.json"), b"{garbage").unwrap();
    let out = project.cmd(&["--json", "ls"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let ls: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ls["total"], 1);
    assert_eq!(ls["runs"][0]["run_id"], good.as_str());
    assert!(String::from_utf8_lossy(&out.stderr).contains("20261025-AAAAAA"));
    assert_clean_failure(&project.cmd(&["show", "20261025-AAAAAA"]), 2);
}

#[test]
fn dir_flag_overrides_discovery_for_writers_and_readers() {
    let project = Project::new();
    let elsewhere = project.root.parent().unwrap().join("elsewhere-store");
    let dir = elsewhere.to_str().unwrap();
    let started = project.json(&["--dir", dir, "run", "--", "true"], 0);
    assert!(elsewhere.join("runs").is_dir());
    assert!(
        !project.root.join(".loglense").exists(),
        "discovery bypassed"
    );
    let ls = project.json(&["--dir", dir, "ls"], 0);
    assert_eq!(ls["runs"][0]["run_id"], started["run_id"]);
}

#[test]
fn gc_reports_removed_runs() {
    let project = Project::new();
    project.finished(&["true"]);
    let gc = project.json(&["gc"], 0);
    assert_eq!(gc["runs_removed"], 0, "fresh runs are within retention");
    let gc = project.json(&["gc", "--max-total", "0"], 0);
    assert_eq!(gc["runs_removed"], 1);
}

#[test]
fn unknown_explicit_parser_is_a_usage_failure() {
    let project = Project::new();
    assert_clean_failure(&project.cmd(&["run", "--parser", "nope", "--", "true"]), 2);
}

#[test]
fn merged_tail_follows_arrival_order_across_streams() {
    let project = Project::new();
    // Pauses force separate pipe reads, so arrival order is unambiguous —
    // including a late stderr line while stdout is still in autodetect.
    let script = "echo a1; sleep 0.1; echo b1 >&2; sleep 0.1; echo a2; sleep 0.1; echo b2 >&2";
    let id = project.finished(&["sh", "-c", script]);
    let merged = project.json(&["tail", &id], 0);
    let order: Vec<String> = merged["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            format!(
                "{}:{}",
                l["stream"].as_str().unwrap(),
                l["text"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(order, ["out:a1", "err:b1", "out:a2", "err:b2"]);
    let last_two = project.json(&["tail", &id, "--lines", "2"], 0);
    assert_eq!(last_two["lines"][0]["text"], "a2");
    assert_eq!(last_two["lines"][1]["text"], "b2");
}
