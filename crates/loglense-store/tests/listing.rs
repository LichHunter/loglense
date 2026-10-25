//! TDD unit 4: run listing with derived liveness (orphaned / lost / corrupt).

use std::path::{Path, PathBuf};

use loglense_core::RunId;
use loglense_store::{list_runs, write_meta, DerivedState, Meta, Status};
use tempfile::{tempdir, TempDir};

fn rid(id: &str) -> RunId {
    use std::str::FromStr;
    RunId::from_str(id).expect("valid RunId fixture")
}

fn meta_fixture(id: &str, status: Status, started: i64) -> Meta {
    Meta {
        run_id: rid(id),
        cmd: vec!["cargo".into(), "test".into()],
        cwd: Path::new("/tmp/proj").to_path_buf(),
        supervisor_pid: std::process::id(),
        child_pid: None,
        child_starttime: None,
        parser: None,
        status,
        exit_code: None,
        signal: None,
        started_at_ms: started,
        ended_at_ms: None,
        tool_version: "0.1.0".into(),
        schema_version: 1,
        on_complete: None,
    }
}

fn make_run(state: &Path, id: &str, meta: &Meta) -> PathBuf {
    let dir = state.join("runs").join(id);
    std::fs::create_dir_all(&dir).expect("mkdir run");
    write_meta(&dir, meta).expect("write meta");
    dir
}

fn state() -> TempDir {
    tempdir().expect("tempdir state")
}

/// A pid that was reaped, so `/proc/<pid>` no longer exists.
fn reaped_pid() -> u32 {
    for _ in 0..8 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return pid;
        }
    }
    panic!("could not obtain a reaped pid");
}

/// `/proc/<pid>/stat` field 22 (starttime), for building honest fixtures.
fn proc_starttime(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read stat");
    let after_comm = stat.rsplit_once(')').expect("split comm").1;
    after_comm
        .split_whitespace()
        .nth(19)
        .expect("starttime field")
        .parse()
        .expect("starttime number")
}

#[test]
fn lists_runs_newest_first() {
    let state = state();
    make_run(
        state.path(),
        "20260101-AAAAAA",
        &meta_fixture("20260101-AAAAAA", Status::Exited, 1000),
    );
    make_run(
        state.path(),
        "20260101-BBBBBB",
        &meta_fixture("20260101-BBBBBB", Status::Exited, 3000),
    );
    make_run(
        state.path(),
        "20260101-CCCCCC",
        &meta_fixture("20260101-CCCCCC", Status::Exited, 2000),
    );

    let runs = list_runs(state.path());

    let starts: Vec<i64> = runs.iter().map(|r| r.started_at_ms.unwrap()).collect();
    assert_eq!(starts, vec![3000, 2000, 1000]);
}

/// A live stand-in supervisor whose argv names the run directory, like the
/// real `loglense __supervisor <run-dir>`. A compound script keeps `sh`
/// alive (no exec into sleep); its own process group plus null stdio let
/// [`reap_group`] remove it and its sleep without holding harness pipes.
fn fake_supervisor(run_dir: &Path) -> std::process::Child {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    std::process::Command::new("sh")
        .args(["-c", "sleep 300; :", "__supervisor"])
        .arg(run_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn fake supervisor")
}

fn reap_group(mut leader: std::process::Child) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", leader.id())])
        .status();
    let _ = leader.wait();
}

#[test]
fn running_with_live_supervisor_has_no_derived_state() {
    let state = state();
    let run_dir = state.path().join("runs").join("20260101-AAAAAA");
    let supervisor = fake_supervisor(&run_dir);
    let mut meta = meta_fixture("20260101-AAAAAA", Status::Running, 1000);
    meta.supervisor_pid = supervisor.id();
    make_run(state.path(), "20260101-AAAAAA", &meta);

    let runs = list_runs(state.path());

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, Some(Status::Running));
    assert_eq!(runs[0].derived, None);
    assert_eq!(runs[0].cmd_head, "cargo");
    reap_group(supervisor);
}

#[test]
fn live_pid_that_is_not_this_runs_supervisor_is_not_trusted() {
    // pid reuse: the recorded supervisor pid now belongs to an unrelated
    // process (this test binary), so the run is classified dead.
    let state = state();
    let meta = meta_fixture("20260101-AAAAAA", Status::Running, 1000);
    make_run(state.path(), "20260101-AAAAAA", &meta);

    let runs = list_runs(state.path());

    assert_eq!(runs[0].derived, Some(DerivedState::Lost));
}

#[test]
fn dead_supervisor_live_child_classifies_orphaned() {
    let state = state();
    let mut sleep = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn sleep");
    let mut meta = meta_fixture("20260101-AAAAAA", Status::Running, 1000);
    meta.supervisor_pid = reaped_pid();
    meta.child_pid = Some(sleep.id());
    meta.child_starttime = Some(proc_starttime(sleep.id()));
    make_run(state.path(), "20260101-AAAAAA", &meta);

    let runs = list_runs(state.path());

    assert_eq!(runs[0].derived, Some(DerivedState::Orphaned));
    sleep.kill().expect("cleanup kill");
    sleep.wait().expect("cleanup wait");
}

#[test]
fn dead_supervisor_dead_child_classifies_lost() {
    let state = state();
    let mut meta = meta_fixture("20260101-AAAAAA", Status::Running, 1000);
    meta.supervisor_pid = reaped_pid();
    meta.child_pid = Some(reaped_pid());
    meta.child_starttime = Some(1);
    make_run(state.path(), "20260101-AAAAAA", &meta);

    let runs = list_runs(state.path());

    assert_eq!(runs[0].derived, Some(DerivedState::Lost));
}

#[test]
fn starttime_mismatch_on_live_child_pid_classifies_lost() {
    // Pid-reuse guard: the child pid IS alive (this test process) but the
    // recorded starttime does not match, so the child is NOT ours.
    let state = state();
    let mut meta = meta_fixture("20260101-AAAAAA", Status::Running, 1000);
    meta.supervisor_pid = reaped_pid();
    meta.child_pid = Some(std::process::id());
    meta.child_starttime = Some(u64::MAX);
    make_run(state.path(), "20260101-AAAAAA", &meta);

    let runs = list_runs(state.path());

    assert_eq!(runs[0].derived, Some(DerivedState::Lost));
}

#[test]
fn corrupt_meta_is_listed_as_placeholder_and_never_panics() {
    let state = state();
    make_run(
        state.path(),
        "20260101-DECADE",
        &meta_fixture("20260101-DECADE", Status::Exited, 2000),
    );
    let corrupt_dir = state.path().join("runs/20260101-BADRUN0");
    std::fs::create_dir_all(&corrupt_dir).expect("mkdir corrupt");
    std::fs::write(corrupt_dir.join("meta.json"), b"{\"run_id\": \"2026").expect("garbage meta");
    let no_meta_dir = state.path().join("runs/20260101-NOMETAAA");
    std::fs::create_dir_all(&no_meta_dir).expect("mkdir no-meta");

    let runs = list_runs(state.path());

    assert_eq!(runs.len(), 3);
    let by_id = |id: &str| runs.iter().find(|r| r.run_id == id).expect(id);
    let corrupt = by_id("20260101-BADRUN0");
    assert_eq!(corrupt.derived, Some(DerivedState::Corrupt));
    assert_eq!(corrupt.status, None);
    assert_eq!(corrupt.cmd_head, "");
    assert_eq!(corrupt.started_at_ms, None);
    let missing = by_id("20260101-NOMETAAA");
    assert_eq!(missing.derived, Some(DerivedState::Corrupt));
    let good = by_id("20260101-DECADE");
    assert_eq!(good.status, Some(Status::Exited));
    // Good run sorts first (only one with a parseable started_at).
    assert_eq!(runs[0].run_id, "20260101-DECADE");
}

#[test]
fn missing_runs_dir_lists_empty() {
    let state = state();

    assert!(list_runs(state.path()).is_empty());
}
