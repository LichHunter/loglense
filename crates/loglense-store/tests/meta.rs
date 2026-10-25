//! TDD unit 2: run metadata model, atomic writes, run-dir creation.

use std::path::Path;
use std::sync::Barrier;

use loglense_core::RunId;
use loglense_store::{create_run_dir_at, read_meta, write_meta, Meta, OnComplete, Status};
use tempfile::tempdir;

fn rid(id: &str) -> RunId {
    use std::str::FromStr;
    RunId::from_str(id).expect("valid RunId fixture")
}

fn sample_meta(id: &str, status: Status) -> Meta {
    Meta {
        run_id: rid(id),
        cmd: vec!["cargo".into(), "test".into()],
        cwd: Path::new("/tmp/proj").to_path_buf(),
        supervisor_pid: 4242,
        child_pid: Some(4243),
        child_starttime: Some(12345),
        parser: Some("cargo".into()),
        status,
        exit_code: None,
        signal: None,
        started_at_ms: 1_764_000_000_000,
        ended_at_ms: None,
        tool_version: "0.1.0".into(),
        schema_version: 1,
        on_complete: None,
    }
}

#[test]
fn meta_serde_uses_snake_case_and_kebab_status() {
    let json = serde_json::to_string(&sample_meta("20260101-AAAAAA", Status::FailedToStart))
        .expect("serialize");

    assert!(json.contains("\"run_id\":\"20260101-AAAAAA\""), "{json}");
    assert!(json.contains("\"supervisor_pid\":4242"), "{json}");
    assert!(json.contains("\"child_starttime\":12345"), "{json}");
    assert!(json.contains("\"status\":\"failed-to-start\""), "{json}");
    assert!(json.contains("\"schema_version\":1"), "{json}");
    // Optional None fields are omitted.
    assert!(!json.contains("exit_code"), "{json}");
    assert!(!json.contains("on_complete"), "{json}");

    let back: Meta = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.status, Status::FailedToStart);
    assert_eq!(back.child_pid, Some(4243));
}

#[test]
fn meta_reader_tolerates_absent_optional_fields() {
    // Minimal meta: every optional field absent, an unknown field present.
    let json = r#"{
        "run_id": "20260101-AAAAAA",
        "cmd": ["make"],
        "cwd": "/tmp",
        "supervisor_pid": 7,
        "status": "running",
        "started_at_ms": 5,
        "tool_version": "0.1.0",
        "schema_version": 1,
        "future_field": {"anything": true}
    }"#;

    let meta: Meta = serde_json::from_str(json).expect("tolerant parse");

    assert_eq!(meta.status, Status::Running);
    assert_eq!(meta.child_pid, None);
    assert_eq!(meta.on_complete, None);
}

#[test]
fn on_complete_round_trips() {
    let mut meta = sample_meta("20260101-AAAAAA", Status::Running);
    meta.on_complete = Some(OnComplete {
        command: "loglense notify tmux -t %3".into(),
        timeout_ms: 30_000,
    });
    let json = serde_json::to_string(&meta).expect("serialize");
    assert!(json.contains("\"on_complete\":{\"command\":"), "{json}");

    let back: Meta = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.on_complete.as_ref().map(|c| c.timeout_ms),
        Some(30_000)
    );
}

#[test]
fn meta_write_is_atomic_round_trip() {
    let dir = tempdir().expect("tempdir");
    let meta = sample_meta("20260101-BBBBBB", Status::Exited);
    let exited = Meta {
        exit_code: Some(3),
        ended_at_ms: Some(1_764_000_001_000),
        ..meta
    };

    write_meta(dir.path(), &exited).expect("write");
    // No .tmp litter after a successful write.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(entries, vec!["meta.json"]);

    let back = read_meta(dir.path()).expect("read");
    assert_eq!(back, exited);
    assert_eq!(back.exit_code, Some(3));
}

#[test]
fn concurrent_readers_never_observe_partial_meta() {
    let dir = tempdir().expect("tempdir");
    let rounds: i64 = 1000;
    let start_ms = 1_764_000_000_000;
    let initial = sample_meta("20260101-CCCCCC", Status::Running);
    write_meta(dir.path(), &initial).expect("initial write");

    let writer_dir = dir.path().to_path_buf();
    let reader_dir = dir.path().to_path_buf();
    let barrier = std::sync::Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();

    let writer = std::thread::spawn(move || {
        for i in 0..rounds {
            let status = if i % 2 == 0 {
                Status::Running
            } else {
                Status::Exited
            };
            let meta = Meta {
                status,
                started_at_ms: start_ms + i,
                ..initial.clone()
            };
            write_meta(&writer_dir, &meta).expect("writer round");
        }
        writer_barrier.wait();
    });

    let mut reads = 0u32;
    for _ in 0..rounds {
        let meta = read_meta(&reader_dir).expect("reader parse");
        assert_eq!(meta.run_id, rid("20260101-CCCCCC"));
        assert!((1_764_000_000_000..1_764_000_001_000).contains(&meta.started_at_ms));
        reads += 1;
        std::thread::yield_now();
    }
    barrier.wait();
    writer.join().expect("writer join");

    assert_eq!(reads, 1000);
    let final_meta = read_meta(dir.path()).expect("final read");
    assert_eq!(final_meta.status, Status::Exited);
}

#[test]
fn create_run_dir_makes_layout_with_empty_files() {
    let state = tempdir().expect("tempdir");
    let date = time::Date::from_calendar_date(2026, time::Month::January, 2).expect("date fixture");

    let (id, run_dir) =
        create_run_dir_at(state.path(), date, &mut || 0x0AAAAAAA).expect("create run dir");

    assert!(id.as_str().starts_with("20260102-"), "{}", id);
    assert_eq!(run_dir, state.path().join("runs").join(id.as_str()));
    for name in ["meta.json", "stdout.raw", "stderr.raw", "events.jsonl"] {
        let f = run_dir.join(name);
        assert!(f.is_file(), "missing {name}");
        assert_eq!(std::fs::metadata(&f).expect("stat").len(), 0, "{name}");
    }
}

#[test]
fn create_run_dir_regenerates_on_id_collision() {
    let state = tempdir().expect("tempdir");
    let date = time::Date::from_calendar_date(2026, time::Month::January, 2).expect("date fixture");
    // First create grabs entropy X; a pre-existing dir for X forces EEXIST
    // on the next attempt, exercising the regen path.
    let (first, _) = create_run_dir_at(state.path(), date, &mut || 0x0BBBBBBB).expect("first");
    let mut values = [0x0BBBBBBBu32, 0x0BBBBBBB, 0x0CCCCCCC].iter();
    let (second, _) = create_run_dir_at(state.path(), date, &mut || *values.next().unwrap())
        .expect("second after collisions");

    assert_ne!(first, second);
    assert!(second.as_str().starts_with("20260102-"));
}
