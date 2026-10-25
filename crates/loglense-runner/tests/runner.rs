//! Runner integration tests (T11): every case spawns REAL commands through
//! the real detached supervisor binary and observes the store and `/proc`.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use loglense_runner::{
    check_terminal, kill_run, spawn_run, wait_run, KillAction, Lifecycle, RunnerError,
    SpawnOutcome, SpawnRequest,
};
use loglense_store::{read_events, read_meta, write_meta, Status};

const SUPERVISOR: &str = env!("CARGO_BIN_EXE_loglense-runner-supervisor");

/// A fresh `<tmp>/.loglense` state dir (the tempdir guard keeps it alive).
fn state() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join(".loglense");
    std::fs::create_dir(&state).expect("state dir");
    (tmp, state)
}

fn request(cmd: &[&str], cwd: &Path) -> SpawnRequest {
    let mut req = SpawnRequest::new(
        cmd.iter().map(|s| s.to_string()).collect(),
        cwd.to_path_buf(),
    );
    req.supervisor_exe = Some(PathBuf::from(SUPERVISOR));
    req
}

fn start(state: &Path, cmd: &[&str]) -> SpawnOutcome {
    spawn_run(state, request(cmd, state)).expect("run starts")
}

fn finish(outcome: &SpawnOutcome) -> loglense_runner::WaitOutcome {
    wait_run(
        &outcome.run_dir,
        Some(Duration::from_secs(20)),
        Duration::from_millis(20),
    )
}

fn alive(pid: u32) -> bool {
    std::fs::metadata(format!("/proc/{pid}")).is_ok()
}

/// `/proc/<pid>/stat` fields after the comm: index 0 = state, 2 = pgrp,
/// 3 = session.
fn stat_field(pid: u32, index: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(index)?
            .to_string(),
    )
}

fn until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

fn signal(pid: u32, sig: i32) {
    // SAFETY: plain syscall on a pid this test spawned.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

fn summary(run_dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(run_dir.join("summary.json")).expect("summary.json");
    serde_json::from_str(&text).expect("summary parses")
}

/// Best-effort cleanup for runs a test leaves live.
fn reap(outcome: &SpawnOutcome) {
    kill_run(&outcome.run_dir);
    finish(outcome);
}

#[test]
fn spawn_returns_instantly_with_running_meta() {
    let (_tmp, state) = state();
    let begun = Instant::now();
    let run = start(&state, &["sleep", "5"]);
    assert!(
        begun.elapsed() < Duration::from_millis(500),
        "spawn took {:?}",
        begun.elapsed()
    );
    let meta = read_meta(&run.run_dir).expect("meta");
    assert_eq!(meta.status, Status::Running);
    assert_eq!(meta.child_pid, Some(run.child_pid));
    assert!(meta.child_starttime.is_some(), "pid-reuse guard recorded");
    assert!(alive(run.child_pid));
    assert_eq!(check_terminal(&run.run_dir), Lifecycle::Running);
    reap(&run);
}

#[test]
fn supervisor_leads_a_new_session_and_child_its_own_group() {
    let (_tmp, state) = state();
    let run = start(&state, &["sleep", "5"]);
    let ours = stat_field(std::process::id(), 3).expect("own session");
    let sup = run.supervisor_pid.to_string();
    assert_eq!(
        stat_field(run.supervisor_pid, 3),
        Some(sup.clone()),
        "setsid"
    );
    assert_ne!(Some(ours), stat_field(run.supervisor_pid, 3));
    assert_eq!(
        stat_field(run.child_pid, 2),
        Some(run.child_pid.to_string()),
        "child leads its own process group (group kill target)"
    );
    assert_eq!(
        stat_field(run.child_pid, 3),
        Some(sup),
        "child in supervisor session"
    );
    reap(&run);
}

#[test]
fn run_survives_the_cli_and_its_whole_process_group_dying() {
    let (_tmp, state) = state();
    let mut cli = Command::new(SUPERVISOR)
        .arg("spawn")
        .arg(&state)
        .args(["sleep", "30"])
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("cli stand-in starts");
    let cli_group = cli.id();
    let mut line = String::new();
    cli.stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut line)
        .expect("read ids");
    assert!(
        cli.wait().expect("cli exits").success(),
        "cli printed {line:?}"
    );
    let ids: Vec<&str> = line.split_whitespace().collect();
    let (supervisor, child): (u32, u32) = (ids[1].parse().unwrap(), ids[2].parse().unwrap());
    // Kill the CLI's entire process group, as a harness timeout would.
    // SAFETY: negative pid = the group the test itself created.
    unsafe {
        libc::kill(-(cli_group as i32), libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(supervisor), "supervisor survived");
    assert!(alive(child), "child survived");
    let run_dir = state.join("runs").join(ids[0]);
    let run_dir = run_dir.canonicalize().expect("run dir");
    assert_eq!(check_terminal(&run_dir), Lifecycle::Running);
    assert_eq!(kill_run(&run_dir).action, KillAction::Signaled);
    let done = wait_run(
        &run_dir,
        Some(Duration::from_secs(10)),
        Duration::from_millis(20),
    );
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Signaled));
}

#[test]
fn happy_exit_spools_exact_raws_and_one_event_per_line() {
    let (_tmp, state) = state();
    let run = start(&state, &["sh", "-c", "echo out; echo err >&2; exit 0"]);
    let done = finish(&run);
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Exited));
    assert_eq!(done.exit_code, Some(0));
    assert_eq!(
        std::fs::read(run.run_dir.join("stdout.raw")).unwrap(),
        b"out\n"
    );
    assert_eq!(
        std::fs::read(run.run_dir.join("stderr.raw")).unwrap(),
        b"err\n"
    );
    let (events, read) = read_events(&run.run_dir.join("events.jsonl")).expect("events");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(read.skipped, 0);
    let mut seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2], "run-monotonic seq");
    let summary = summary(&run.run_dir);
    assert_eq!(summary["status"], "exited");
    assert_eq!(summary["exit_code"], 0);
    assert_eq!(summary["events"], 2);
    assert_eq!(summary["truncated"], false);
}

#[test]
fn failing_exit_records_code_error_event_and_summary_counts() {
    let (_tmp, state) = state();
    let run = start(&state, &["sh", "-c", "echo ERROR boom; exit 3"]);
    let done = finish(&run);
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Exited));
    assert_eq!(done.exit_code, Some(3));
    let (events, _) = read_events(&run.run_dir.join("events.jsonl")).expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].severity, loglense_core::Severity::Error);
    assert!(events[0].template_id.is_some(), "dedup id stamped");
    let summary = summary(&run.run_dir);
    assert_eq!(summary["severities"]["error"], 1);
    assert_eq!(summary["templates"][0]["count"], 1);
    assert_eq!(summary["templates"][0]["example"], "ERROR boom");
}

#[test]
fn kill_terminates_the_run_within_the_grace_window() {
    let (_tmp, state) = state();
    let run = start(&state, &["sleep", "300"]);
    let begun = Instant::now();
    let outcome = kill_run(&run.run_dir);
    assert_eq!(outcome.action, KillAction::Signaled, "{}", outcome.detail);
    let done = finish(&run);
    assert!(
        begun.elapsed() < Duration::from_secs(6),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Signaled));
    assert_eq!(read_meta(&run.run_dir).unwrap().signal, Some(libc::SIGTERM));
    assert!(
        until(Duration::from_secs(2), || !alive(run.child_pid)),
        "child reaped"
    );
    assert!(
        until(Duration::from_secs(2), || !alive(run.supervisor_pid)),
        "supervisor exited and was reaped (no zombie)"
    );
    let again = kill_run(&run.run_dir);
    assert_eq!(
        again.action,
        KillAction::AlreadyTerminal,
        "{}",
        again.detail
    );
}

#[test]
fn kill_escalates_to_sigkill_when_term_is_ignored() {
    let (_tmp, state) = state();
    let run = start(&state, &["sh", "-c", "trap '' TERM; sleep 300"]);
    // Let the shell install its trap before the TERM lands.
    std::thread::sleep(Duration::from_millis(200));
    let begun = Instant::now();
    assert_eq!(kill_run(&run.run_dir).action, KillAction::Signaled);
    let done = finish(&run);
    let took = begun.elapsed();
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(8),
        "{took:?}"
    );
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Signaled));
    assert_eq!(read_meta(&run.run_dir).unwrap().signal, Some(libc::SIGKILL));
}

#[test]
fn external_term_to_supervisor_is_forwarded_and_run_finalized() {
    let (_tmp, state) = state();
    let run = start(&state, &["sleep", "300"]);
    signal(run.supervisor_pid, libc::SIGTERM);
    let done = finish(&run);
    assert_eq!(done.lifecycle, Lifecycle::Terminal(Status::Signaled));
    assert_eq!(summary(&run.run_dir)["status"], "signaled");
    assert!(until(Duration::from_secs(2), || !alive(run.child_pid)));
}

#[test]
fn dead_supervisor_fast_fails_wait_and_orphan_kill_cleans_up() {
    let (_tmp, state) = state();
    let run = start(&state, &["sleep", "300"]);
    signal(run.supervisor_pid, libc::SIGKILL);
    assert!(until(Duration::from_secs(2), || !alive(run.supervisor_pid)));
    let begun = Instant::now();
    let waited = wait_run(
        &run.run_dir,
        Some(Duration::from_secs(10)),
        Duration::from_millis(250),
    );
    assert!(
        begun.elapsed() < Duration::from_secs(1),
        "fast-fail, not timeout"
    );
    assert_eq!(waited.lifecycle, Lifecycle::Orphaned);
    assert!(!waited.timed_out);
    let killed = kill_run(&run.run_dir);
    assert_eq!(killed.action, KillAction::Signaled, "{}", killed.detail);
    assert!(until(Duration::from_secs(2), || !alive(run.child_pid)));
    assert_eq!(check_terminal(&run.run_dir), Lifecycle::Lost);
    assert_eq!(kill_run(&run.run_dir).action, KillAction::Refused);
}

#[test]
fn starttime_mismatch_refuses_to_signal_a_reused_pid() {
    let (_tmp, state) = state();
    let run = start(&state, &["sleep", "300"]);
    signal(run.supervisor_pid, libc::SIGKILL);
    assert!(until(Duration::from_secs(2), || !alive(run.supervisor_pid)));
    // Simulate pid reuse: the live pid no longer carries the recorded
    // starttime.
    let mut meta = read_meta(&run.run_dir).unwrap();
    meta.child_starttime = Some(1);
    write_meta(&run.run_dir, &meta).unwrap();
    let outcome = kill_run(&run.run_dir);
    assert_eq!(outcome.action, KillAction::Refused);
    assert!(outcome.detail.contains("pid reuse"), "{}", outcome.detail);
    assert!(alive(run.child_pid), "the impostor pid was not signaled");
    signal(run.child_pid, libc::SIGKILL);
}

#[test]
fn unknown_command_fails_to_start_with_typed_error() {
    let (_tmp, state) = state();
    let err = spawn_run(&state, request(&["/nonexistent/loglense-test-cmd"], &state))
        .expect_err("cannot start");
    assert!(matches!(err, RunnerError::StartFailed { .. }), "{err}");
    let runs: Vec<_> = std::fs::read_dir(state.join("runs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(runs.len(), 1);
    let meta = read_meta(&runs[0].path()).unwrap();
    assert_eq!(meta.status, Status::FailedToStart);
    assert_eq!(
        check_terminal(&runs[0].path()),
        Lifecycle::Terminal(Status::FailedToStart)
    );
}

#[test]
fn unknown_explicit_parser_is_rejected_before_any_run_dir() {
    let (_tmp, state) = state();
    let mut req = request(&["true"], &state);
    req.parser = Some("no-such-parser".to_string());
    let err = spawn_run(&state, req).expect_err("rejected");
    assert!(matches!(err, RunnerError::UnknownParser { .. }), "{err}");
    assert!(!state.join("runs").exists());
}

#[test]
fn free_space_floor_refuses_to_start_before_any_run_dir() {
    let (_tmp, state) = state();
    // A floor no real filesystem can satisfy (1 EiB).
    std::fs::write(
        state.join("config.toml"),
        "[retention]\nmin_free_mb = 1099511627776\n",
    )
    .unwrap();
    let err = spawn_run(&state, request(&["true"], &state)).expect_err("refused");
    assert!(matches!(err, RunnerError::LowDisk { .. }), "{err}");
    assert!(err.to_string().contains("min_free_mb"), "actionable: {err}");
    assert!(!state.join("runs").exists());
}

#[test]
fn explicit_parser_streams_events_without_detection_delay() {
    let (_tmp, state) = state();
    let mut req = request(&["sh", "-c", "echo ERROR now; sleep 3"], &state);
    req.parser = Some("generic".to_string());
    let run = spawn_run(&state, req).expect("starts");
    let events = run.run_dir.join("events.jsonl");
    assert!(
        until(Duration::from_secs(1), || read_events(&events)
            .is_ok_and(|(e, _)| e.len() == 1)),
        "event written while the run is still live"
    );
    assert_eq!(check_terminal(&run.run_dir), Lifecycle::Running);
    reap(&run);
}

#[test]
fn autodetect_selects_tool_parser_and_counts_tests() {
    let (_tmp, state) = state();
    let script = "printf '=== RUN   TestA\\n--- FAIL: TestA (0.00s)\\n=== RUN   TestB\\n--- PASS: TestB (0.01s)\\nFAIL\\n'; exit 1";
    let run = start(&state, &["sh", "-c", script]);
    let done = finish(&run);
    assert_eq!(done.exit_code, Some(1));
    assert_eq!(
        read_meta(&run.run_dir).unwrap().parser.as_deref(),
        Some("gotest")
    );
    let summary = summary(&run.run_dir);
    assert_eq!(summary["tests"]["failed"], 1);
    assert_eq!(summary["tests"]["passed"], 1);
}

#[test]
fn ansi_is_stripped_for_parsing_but_raw_stays_pristine() {
    let (_tmp, state) = state();
    let run = start(&state, &["printf", "\\033[31mERROR\\033[0m red\\n"]);
    finish(&run);
    assert_eq!(
        std::fs::read(run.run_dir.join("stdout.raw")).unwrap(),
        b"\x1b[31mERROR\x1b[0m red\n"
    );
    let (events, _) = read_events(&run.run_dir.join("events.jsonl")).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message, "ERROR red");
    assert_eq!(events[0].severity, loglense_core::Severity::Error);
}

#[test]
fn binary_stream_is_spooled_raw_only() {
    let (_tmp, state) = state();
    let run = start(&state, &["printf", "ERROR a\\000b\\nmore\\n"]);
    finish(&run);
    assert_eq!(
        std::fs::read(run.run_dir.join("stdout.raw")).unwrap(),
        b"ERROR a\0b\nmore\n"
    );
    let (events, _) = read_events(&run.run_dir.join("events.jsonl")).unwrap();
    assert!(events.is_empty(), "binary stream never parsed: {events:?}");
}

/// NFR-03 smoke: 200 MB of stdout through the full pipeline with flat
/// supervisor memory. Slow in debug builds — run with
/// `cargo test -p loglense-runner --release -- --ignored`.
#[test]
#[ignore = "200 MB throughput smoke; run with --release -- --ignored"]
fn large_output_keeps_supervisor_rss_bounded() {
    let (_tmp, state) = state();
    let line = "x".repeat(1023);
    let script = format!("yes '{line}' | head -c 209715200");
    let run = start(&state, &["sh", "-c", &script]);
    let mut peak_kb = 0u64;
    while check_terminal(&run.run_dir) == Lifecycle::Running {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", run.supervisor_pid))
        {
            if let Some(kb) = status
                .lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            {
                peak_kb = peak_kb.max(kb);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let raw = std::fs::metadata(run.run_dir.join("stdout.raw"))
        .unwrap()
        .len();
    assert_eq!(raw, 209_715_200, "every byte spooled");
    assert!(peak_kb > 0, "sampled the supervisor");
    eprintln!("supervisor peak RSS (VmHWM): {peak_kb} kB for {raw} raw bytes");
    assert!(peak_kb < 150 * 1024, "peak RSS {peak_kb} kB");
}

// ------------------------------------------------------------- callbacks

fn with_callback(cmd: &[&str], callback: &str, timeout_ms: u64, cwd: &Path) -> SpawnRequest {
    let mut req = request(cmd, cwd);
    req.on_complete = Some(loglense_store::OnComplete {
        command: callback.to_string(),
        timeout_ms,
    });
    req
}

/// Polls summary.json until the callback result is recorded.
fn callback_result(run_dir: &Path) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    until(Duration::from_secs(15), || {
        last = std::fs::read_to_string(run_dir.join("summary.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        last.get("callback").is_some()
    });
    last
}

#[test]
fn callback_fires_after_finalization_with_env_and_summary_on_stdin() {
    let (tmp, state) = state();
    let out = tmp.path().join("cb");
    std::fs::create_dir(&out).unwrap();
    // The callback checks the run is already final when it starts, then
    // records its stdin and environment.
    let callback = format!(
        "grep -q '\"status\":\"exited\"' \"$LOGLENSE_STORE_PATH/runs/$LOGLENSE_RUN_ID/meta.json\" && touch {o}/meta-was-final; \
         cat > {o}/stdin.json; env | grep '^LOGLENSE_' | sort > {o}/env.txt; echo callback-said-hi",
        o = out.display()
    );
    let run = spawn_run(
        &state,
        with_callback(
            &["sh", "-c", "echo 'ERROR boom'; exit 3"],
            &callback,
            10_000,
            &state,
        ),
    )
    .expect("starts");
    let summary = callback_result(&run.run_dir);
    assert_eq!(summary["callback"], "ok", "{summary}");
    assert_eq!(summary["callback_detail"], "exit 0");
    assert!(
        out.join("meta-was-final").exists(),
        "finalize precedes callback"
    );
    let stdin: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("stdin.json")).unwrap()).unwrap();
    assert_eq!(stdin["exit_code"], 3);
    assert_eq!(stdin["severities"]["error"], 1);
    assert!(
        stdin.get("callback").is_none(),
        "stdin is the pre-callback summary"
    );
    let env = std::fs::read_to_string(out.join("env.txt")).unwrap();
    assert!(
        env.contains(&format!("LOGLENSE_RUN_ID={}", run.run_id)),
        "{env}"
    );
    assert!(env.contains("LOGLENSE_STATUS=exited"), "{env}");
    assert!(env.contains("LOGLENSE_EXIT_CODE=3"), "{env}");
    assert!(env.contains("LOGLENSE_STORE_PATH="), "{env}");
    let log = std::fs::read_to_string(run.run_dir.join(loglense_runner::CALLBACK_LOG)).unwrap();
    assert_eq!(log, "callback-said-hi\n");
    // The run's own outcome is untouched by the callback.
    assert_eq!(read_meta(&run.run_dir).unwrap().exit_code, Some(3));
}

#[test]
fn failing_and_missing_callbacks_record_failed() {
    let (_tmp, state) = state();
    for (callback, detail) in [("exit 7", "exit 7"), ("/nonexistent/notify-me", "exit 127")] {
        let run = spawn_run(&state, with_callback(&["true"], callback, 10_000, &state)).unwrap();
        let summary = callback_result(&run.run_dir);
        assert_eq!(summary["callback"], "failed", "{callback}: {summary}");
        assert_eq!(summary["callback_detail"], detail);
        assert_eq!(summary["status"], "exited");
    }
}

#[test]
fn hanging_callback_is_killed_at_its_timeout() {
    let (_tmp, state) = state();
    let begun = Instant::now();
    let run = spawn_run(&state, with_callback(&["true"], "sleep 60", 500, &state)).unwrap();
    let summary = callback_result(&run.run_dir);
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(summary["callback"], "timeout");
    assert_eq!(summary["callback_detail"], "killed after 500 ms");
    assert!(
        until(Duration::from_secs(2), || !alive(run.supervisor_pid)),
        "supervisor exits after the callback"
    );
}

#[test]
fn callback_output_is_bounded() {
    let (_tmp, state) = state();
    let run = spawn_run(
        &state,
        with_callback(
            &["true"],
            "head -c 200000 /dev/zero | tr '\\0' x",
            10_000,
            &state,
        ),
    )
    .unwrap();
    assert_eq!(callback_result(&run.run_dir)["callback"], "ok");
    let log = std::fs::read(run.run_dir.join(loglense_runner::CALLBACK_LOG)).unwrap();
    assert!(log.len() < 64 * 1024 + 200, "{} bytes", log.len());
    assert!(String::from_utf8_lossy(&log).contains("more bytes of callback output dropped"));
}

#[test]
fn failed_to_start_runs_fire_the_callback_without_blocking_the_caller() {
    let (tmp, state) = state();
    let marker = tmp.path().join("fired");
    let callback = format!("echo \"$LOGLENSE_STATUS\" > {}; sleep 1", marker.display());
    let begun = Instant::now();
    let err = spawn_run(
        &state,
        with_callback(&["/nonexistent/loglense-cmd"], &callback, 10_000, &state),
    )
    .expect_err("cannot start");
    assert!(matches!(err, RunnerError::StartFailed { .. }), "{err}");
    assert!(
        begun.elapsed() < Duration::from_millis(900),
        "caller not blocked by the callback: {:?}",
        begun.elapsed()
    );
    assert!(until(Duration::from_secs(5), || marker.exists()));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "failed-to-start"
    );
    let run_dir = std::fs::read_dir(state.join("runs"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let summary = callback_result(&run_dir);
    assert_eq!(summary["status"], "failed-to-start");
    assert_eq!(summary["callback"], "ok");
}

#[test]
fn killed_supervisor_fires_no_callback() {
    let (tmp, state) = state();
    let marker = tmp.path().join("fired");
    let run = spawn_run(
        &state,
        with_callback(
            &["sleep", "300"],
            &format!("touch {}", marker.display()),
            10_000,
            &state,
        ),
    )
    .unwrap();
    signal(run.supervisor_pid, libc::SIGKILL);
    assert!(until(Duration::from_secs(2), || !alive(run.supervisor_pid)));
    signal(run.child_pid, libc::SIGKILL);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !marker.exists(),
        "lost runs fire nothing (best-effort by construction)"
    );
    assert_eq!(check_terminal(&run.run_dir), Lifecycle::Lost);
}

#[test]
fn project_packs_and_config_apply_inside_the_supervisor() {
    let (_tmp, state) = state();
    std::fs::create_dir_all(state.join("formats.d")).unwrap();
    std::fs::write(
        state.join("formats.d/mytool.toml"),
        "schema = 1\nname = \"mytool\"\n\n[[rule]]\nid = \"err\"\nseverity = \"error\"\npattern = '^\\[ERR\\] (.+)$'\nmessage = \"$1\"\nkind = \"line\"\n",
    )
    .unwrap();
    std::fs::write(state.join("config.toml"), "[output]\nmax_templates = 1\n").unwrap();
    let mut req = request(
        &[
            "sh",
            "-c",
            "echo '[ERR] disk full'; echo 'ERROR other'; echo 'warning: w'",
        ],
        &state,
    );
    req.parser = Some("mytool".to_string());
    let run = spawn_run(&state, req).expect("project pack is a valid parser");
    finish(&run);
    let (events, _) = read_events(&run.run_dir.join("events.jsonl")).unwrap();
    assert_eq!(
        events[0].message, "disk full",
        "pack rule applied by the supervisor"
    );
    let summary = summary(&run.run_dir);
    assert_eq!(
        summary["templates"].as_array().unwrap().len(),
        1,
        "project config.toml max_templates applied"
    );
}

#[test]
fn summary_test_totals_come_from_tool_totals_lines() {
    let (_tmp, state) = state();
    // pytest -q prints no per-test line for passes: only the totals line
    // knows about them.
    let mut req = request(
        &["sh", "-c", "printf 'F.F\\nFAILED t.py::a - x\\nFAILED t.py::b - y\\n2 failed, 1 passed in 0.02s\\n'; exit 1"],
        &state,
    );
    req.parser = Some("pytest".to_string());
    let run = spawn_run(&state, req).unwrap();
    finish(&run);
    let tests = &summary(&run.run_dir)["tests"];
    assert_eq!(tests["failed"], 2, "{tests}");
    assert_eq!(tests["passed"], 1, "{tests}");

    // cargo prints one `test result:` per test binary; the run sums them.
    let script = "printf 'test result: ok. 5 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\\n'";
    let mut req = request(&["sh", "-c", script], &state);
    req.parser = Some("cargo".to_string());
    let run = spawn_run(&state, req).unwrap();
    finish(&run);
    let tests = &summary(&run.run_dir)["tests"];
    assert_eq!(tests["passed"], 7, "{tests}");
    assert_eq!(tests["failed"], 1, "{tests}");
    assert_eq!(tests["ignored"], 1, "{tests}");
}
