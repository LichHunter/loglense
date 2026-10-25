//! TDD unit 5: gc — retention, size budget, flock serialization.

use std::path::Path;

use loglense_core::RunId;
use loglense_store::{gc, write_meta, GcReport, Meta, Status};
use tempfile::{tempdir, TempDir};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_764_000_000_000;

fn rid(id: &str) -> RunId {
    use std::str::FromStr;
    RunId::from_str(id).expect("valid RunId fixture")
}

struct RunSpec {
    id: &'static str,
    started: i64,
    ended: Option<i64>,
    status: Status,
    supervisor_pid: u32,
    payload_bytes: u64,
}

fn spec(id: &'static str) -> RunSpec {
    RunSpec {
        id,
        started: 1000,
        ended: Some(1000),
        status: Status::Exited,
        supervisor_pid: 0,
        payload_bytes: 1024,
    }
}

fn make_run(state: &Path, spec: RunSpec) {
    let RunSpec {
        id,
        started,
        ended,
        status,
        supervisor_pid,
        payload_bytes,
    } = spec;
    let dir = state.join("runs").join(id);
    std::fs::create_dir_all(&dir).expect("mkdir run");
    let meta = Meta {
        run_id: rid(id),
        cmd: vec!["cargo".into(), "test".into()],
        cwd: Path::new("/tmp/proj").to_path_buf(),
        supervisor_pid,
        child_pid: None,
        child_starttime: None,
        parser: None,
        status,
        exit_code: None,
        signal: None,
        started_at_ms: started,
        ended_at_ms: ended,
        tool_version: "0.1.0".into(),
        schema_version: 1,
        on_complete: None,
    };
    write_meta(&dir, &meta).expect("write meta");
    let payload = dir.join("stdout.raw");
    let file = std::fs::File::create(&payload).expect("create payload");
    file.set_len(payload_bytes).expect("sparse payload");
}

const GC_IDS: [&str; 5] = [
    "20260101-AAAAA0",
    "20260102-AAAAA1",
    "20260103-AAAAA2",
    "20260104-AAAAA3",
    "20260105-AAAAA4",
];

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

#[test]
fn age_based_removal_removes_only_old_runs() {
    let state = tempdir().expect("tempdir");
    make_run(
        state.path(),
        RunSpec {
            started: NOW - 30 * DAY_MS,
            ended: Some(NOW - 30 * DAY_MS),
            supervisor_pid: reaped_pid(),
            ..spec("20260101-AAAAAA")
        },
    );
    make_run(
        state.path(),
        RunSpec {
            started: NOW - DAY_MS,
            ended: Some(NOW - DAY_MS),
            supervisor_pid: reaped_pid(),
            ..spec("20260101-BBBBBB")
        },
    );

    let report = gc(state.path(), 14, 2048, NOW).expect("gc");

    assert_eq!(report.runs_removed, 1);
    assert!(report.bytes_freed >= 1024);
    assert!(!state.path().join("runs/20260101-AAAAAA").exists());
    assert!(state.path().join("runs/20260101-BBBBBB").exists());
    assert!(state.path().join("gc.lock").is_file());
}

#[test]
fn size_budget_removes_oldest_first() {
    let state = tempdir().expect("tempdir");
    let mb = 900_000u64;
    make_run(
        state.path(),
        RunSpec {
            ended: Some(NOW - 2 * DAY_MS),
            supervisor_pid: reaped_pid(),
            payload_bytes: mb,
            ..spec("20260101-AAAAAA")
        },
    );
    make_run(
        state.path(),
        RunSpec {
            started: 2000,
            ended: Some(NOW - DAY_MS),
            supervisor_pid: reaped_pid(),
            payload_bytes: mb,
            ..spec("20260101-BBBBBB")
        },
    );
    make_run(
        state.path(),
        RunSpec {
            started: 3000,
            ended: Some(NOW - DAY_MS),
            supervisor_pid: reaped_pid(),
            payload_bytes: mb,
            ..spec("20260101-CCCCCC")
        },
    );

    let report = gc(state.path(), 3650, 2, NOW).expect("gc");

    assert_eq!(report.runs_removed, 1);
    assert!(report.bytes_freed >= mb);
    assert!(!state.path().join("runs/20260101-AAAAAA").exists());
    assert!(state.path().join("runs/20260101-BBBBBB").exists());
    assert!(state.path().join("runs/20260101-CCCCCC").exists());
}

#[test]
fn live_run_is_never_touched_even_under_full_pressure() {
    let state = tempdir().expect("tempdir");
    make_run(
        state.path(),
        RunSpec {
            started: NOW - 400 * DAY_MS,
            ended: Some(NOW - 400 * DAY_MS),
            status: Status::Running,
            supervisor_pid: std::process::id(),
            payload_bytes: 5_000_000,
            ..spec("20260101-HEART1")
        },
    );
    make_run(
        state.path(),
        RunSpec {
            started: NOW - DAY_MS,
            ended: Some(NOW - DAY_MS),
            supervisor_pid: reaped_pid(),
            payload_bytes: 1_000_000,
            ..spec("20260101-DEADAA")
        },
    );

    let report = gc(state.path(), 14, 1, NOW).expect("gc");

    assert!(state.path().join("runs/20260101-HEART1").exists());
    assert!(!state.path().join("runs/20260101-DEADAA").exists());
    assert_eq!(report.runs_removed, 1);
    assert!(report.bytes_freed >= 1_000_000);
}

#[test]
fn stale_running_run_without_supervisor_is_size_collectable() {
    let state = tempdir().expect("tempdir");
    make_run(
        state.path(),
        RunSpec {
            ended: None,
            status: Status::Running,
            supervisor_pid: reaped_pid(),
            payload_bytes: 3_000_000,
            ..spec("20260101-STRAYA")
        },
    );

    let report = gc(state.path(), 3650, 1, NOW).expect("gc");

    assert_eq!(report.runs_removed, 1);
    assert!(!state.path().join("runs/20260101-STRAYA").exists());
}

#[test]
fn concurrent_gc_invocations_both_succeed() {
    let state: TempDir = tempdir().expect("tempdir");
    for i in 0..5u32 {
        make_run(
            state.path(),
            RunSpec {
                supervisor_pid: reaped_pid(),
                ..spec(GC_IDS[i as usize])
            },
        );
    }
    let a = state.path().to_path_buf();
    let b = state.path().to_path_buf();

    let left = std::thread::spawn(move || gc(&a, 0, 2048, NOW));
    let right = std::thread::spawn(move || gc(&b, 0, 2048, NOW));
    let ra: GcReport = left.join().expect("left join").expect("left gc");
    let rb: GcReport = right.join().expect("right join").expect("right gc");

    assert_eq!(ra.runs_removed + rb.runs_removed, 5);
    assert!(state
        .path()
        .join("runs")
        .read_dir()
        .expect("readdir runs")
        .next()
        .is_none());
    let leftovers: Vec<_> = std::fs::read_dir(state.path())
        .expect("readdir state")
        .map(|e| e.expect("entry").file_name())
        .filter(|n| n.to_string_lossy().starts_with(".trash-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "trash dirs left behind: {leftovers:?}"
    );
}
