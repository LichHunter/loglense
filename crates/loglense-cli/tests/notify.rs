//! Completion callbacks + notify adapters through the CLI (T16, D-011).

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;
use common::{assert_clean_failure, describe, Project, BIN};

/// Polls `show --json` until the callback result is recorded.
fn callback_done(project: &Project, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let show = project.json(&["show", run_id], 0);
        if show["summary"].get("callback").is_some() || Instant::now() > deadline {
            return show;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[test]
fn prefix_form_callback_receives_the_summary_and_is_shown() {
    let project = Project::new();
    let out = project.root.parent().unwrap().join("cb.json");
    let callback = format!("cat > {}; echo delivered", quote(&out));
    let started = project.json(
        &[
            "--on-complete",
            &callback,
            "sh",
            "-c",
            "echo 'ERROR failed build'; exit 1",
        ],
        0,
    );
    let run_id = started["run_id"].as_str().unwrap();
    let show = callback_done(&project, run_id);
    assert_eq!(show["summary"]["callback"], "ok", "{show}");
    assert_eq!(show["meta"]["on_complete"]["timeout_ms"], 30_000);
    assert_eq!(show["callback_log"], "delivered\n");
    let delivered: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(delivered["exit_code"], 1);
    assert_eq!(delivered["severities"]["error"], 1);
    let human = String::from_utf8_lossy(&project.cmd(&["show", run_id]).stdout).into_owned();
    assert!(human.contains("callback: ok (exit 0)"), "{human}");
    assert!(human.contains("| delivered"), "{human}");
}

#[test]
fn callback_timeout_is_recorded_and_bounded() {
    let project = Project::new();
    let started = project.json(
        &[
            "run",
            "--on-complete",
            "sleep 60",
            "--on-complete-timeout",
            "500",
            "--",
            "true",
        ],
        0,
    );
    let begun = Instant::now();
    let show = callback_done(&project, started["run_id"].as_str().unwrap());
    assert!(begun.elapsed() < Duration::from_secs(5));
    assert_eq!(show["summary"]["callback"], "timeout");
    assert_eq!(show["summary"]["status"], "exited", "run outcome untouched");
}

#[test]
fn timeout_flag_without_a_callback_is_a_usage_error() {
    let project = Project::new();
    assert_clean_failure(
        &project.cmd(&["run", "--on-complete-timeout", "5", "--", "true"]),
        2,
    );
}

#[test]
fn notify_exec_passes_env_and_summary_through() {
    let project = Project::new();
    let dir = project.root.parent().unwrap().join("exec-out");
    std::fs::create_dir(&dir).unwrap();
    let inner = format!(
        "cat > {d}/summary.json; printf '%s %s' \"$LOGLENSE_RUN_ID\" \"$LOGLENSE_STATUS\" > {d}/ctx",
        d = dir.display()
    );
    let callback = format!("{BIN} notify exec -- sh -c \"{inner}\"");
    let started = project.json(
        &[
            "run",
            "--on-complete",
            &callback,
            "--",
            "sh",
            "-c",
            "exit 0",
        ],
        0,
    );
    let run_id = started["run_id"].as_str().unwrap();
    assert_eq!(callback_done(&project, run_id)["summary"]["callback"], "ok");
    assert_eq!(
        std::fs::read_to_string(dir.join("ctx")).unwrap(),
        format!("{run_id} exited")
    );
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["status"], "exited");
}

#[test]
fn notify_outside_a_callback_explains_itself() {
    let project = Project::new();
    let out = project.cmd(&["notify", "tmux", "--pane", "%0"]);
    assert_clean_failure(&out, 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("LOGLENSE_RUN_ID"));
}

fn tmux(tmpdir: &Path, args: &[&str]) -> Option<Output> {
    Command::new("tmux")
        .args(args)
        .env("TMUX_TMPDIR", tmpdir)
        .env_remove("TMUX")
        .output()
        .ok()
}

#[test]
fn notify_tmux_types_the_completion_line_into_a_real_pane() {
    let project = Project::new();
    let tmpdir = project.root.parent().unwrap().join("tmux");
    std::fs::create_dir(&tmpdir).unwrap();
    let started = tmux(
        &tmpdir,
        &[
            "new-session",
            "-d",
            "-s",
            "lltest",
            "-x",
            "200",
            "-y",
            "10",
            "cat",
        ],
    );
    if !started.as_ref().is_some_and(|o| o.status.success()) {
        eprintln!("skipping: tmux unavailable ({started:?})");
        return;
    }
    let callback = format!("{BIN} notify tmux --pane lltest");
    // The callback inherits TMUX_TMPDIR through the CLI → supervisor chain.
    let out = Command::new(BIN)
        .args([
            "--json",
            "run",
            "--on-complete",
            &callback,
            "--",
            "sh",
            "-c",
            "echo 'ERROR x'; exit 4",
        ])
        .current_dir(&project.root)
        .env("HOME", &project.home)
        .env("TMUX_TMPDIR", &tmpdir)
        .env_remove("TMUX")
        .env_remove("LOGLENSE_STATE_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let run_id = serde_json::from_slice::<Value>(&out.stdout).unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let show = callback_done(&project, &run_id);
    assert_eq!(show["summary"]["callback"], "ok", "{show}");
    let expected = format!("loglense: run {run_id} exited (exit 4, 1 errors, 0 warnings)");
    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let pane = tmux(&tmpdir, &["capture-pane", "-p", "-t", "lltest"]).unwrap();
        let text = String::from_utf8_lossy(&pane.stdout).into_owned();
        if text.contains(&expected) || Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    tmux(&tmpdir, &["kill-server"]);
    assert!(
        captured.contains(&expected),
        "pane shows the line: {captured:?}"
    );
}

#[test]
fn bad_msg_fmt_makes_the_callback_fail_not_the_run() {
    let project = Project::new();
    let callback = format!("{BIN} notify tmux --pane x --msg-fmt '{{cmd}}'");
    let started = project.json(&["run", "--on-complete", &callback, "--", "true"], 0);
    let show = callback_done(&project, started["run_id"].as_str().unwrap());
    assert_eq!(show["summary"]["callback"], "failed");
    assert_eq!(show["summary"]["status"], "exited");
    assert!(
        show["callback_log"]
            .as_str()
            .unwrap()
            .contains("unknown field"),
        "{show}"
    );
}
