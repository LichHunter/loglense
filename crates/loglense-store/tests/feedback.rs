//! TDD unit 6: feedback records + global index (append, resolve, tolerance).

use std::io::Write;

use loglense_store::{
    feedback_index_path, list_feedback, read_feedback, read_feedback_index, resolve_feedback,
    write_feedback, FeedbackRecord, FeedbackStatus, StoreError,
};
use tempfile::tempdir;

const NOW_MS: i64 = 1_764_000_000_123;
const TS: i64 = NOW_MS / 1000;

fn record(run_id: &str) -> FeedbackRecord {
    FeedbackRecord {
        kind: "parse-miss".into(),
        detail: "error on line 3 not captured".into(),
        run_id: run_id.into(),
        parser: Some("maven".into()),
        tool_version: "0.1.0".into(),
        line: Some(3),
        expected: Some("error".into()),
        snippet: Some("BUILD FAILURE".into()),
        status: FeedbackStatus::Open,
        note: None,
    }
}

#[test]
fn round_trip_leave_list_show_resolve() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");

    let fid = write_feedback(state.path(), home.path(), NOW_MS, record("20260101-AAAAAA"))
        .expect("leave");
    assert_eq!(fid, format!("f-{TS}-1"), "first seq is 1");
    assert!(state.path().join(format!("feedback/{fid}.json")).is_file());

    let listed = list_feedback(state.path(), Some(FeedbackStatus::Open));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, fid);
    assert_eq!(listed[0].1.run_id, "20260101-AAAAAA");

    let shown = read_feedback(state.path(), &fid).expect("show");
    assert_eq!(shown.kind, "parse-miss");
    assert_eq!(shown.line, Some(3));

    let index = read_feedback_index(home.path()).expect("index");
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].fid, fid);
    assert_eq!(index[0].status, FeedbackStatus::Open);
    assert_eq!(
        index[0].path,
        state.path().join(format!("feedback/{fid}.json"))
    );

    resolve_feedback(
        state.path(),
        home.path(),
        &fid,
        FeedbackStatus::Fixed,
        Some("fixed in 0.2.0".into()),
    )
    .expect("resolve");

    let resolved = read_feedback(state.path(), &fid).expect("show resolved");
    assert_eq!(resolved.status, FeedbackStatus::Fixed);
    assert_eq!(resolved.note.as_deref(), Some("fixed in 0.2.0"));
    assert!(list_feedback(state.path(), Some(FeedbackStatus::Open)).is_empty());
    assert_eq!(
        list_feedback(state.path(), Some(FeedbackStatus::Fixed)).len(),
        1
    );

    let index = read_feedback_index(home.path()).expect("index after resolve");
    assert_eq!(index.len(), 1, "resolve rewrites, never appends");
    assert_eq!(index[0].status, FeedbackStatus::Fixed);
}

#[test]
fn sequential_writes_advance_seq() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");

    let first = write_feedback(state.path(), home.path(), NOW_MS, record("r1")).expect("1st");
    let second = write_feedback(state.path(), home.path(), NOW_MS, record("r2")).expect("2nd");

    assert_eq!(first, format!("f-{TS}-1"));
    assert_eq!(second, format!("f-{TS}-2"));
    assert_eq!(read_feedback_index(home.path()).expect("index").len(), 2);
}

#[test]
fn seq_collision_retries_with_plus_two() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");
    let dir = state.path().join("feedback");
    std::fs::create_dir_all(&dir).expect("mkdir feedback");
    // Occupied slots 1 and 2 force: seq=1 collides, retry lands on 3.
    std::fs::write(dir.join(format!("f-{TS}-1.json")), b"{}").expect("seed 1");
    std::fs::write(dir.join(format!("f-{TS}-2.json")), b"{}").expect("seed 2");

    let fid = write_feedback(state.path(), home.path(), NOW_MS, record("r9")).expect("leave");

    assert_eq!(fid, format!("f-{TS}-3"));
}

#[test]
fn fifty_parallel_appends_leave_fifty_intact_lines() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");
    let state_shared = state.path().to_path_buf();
    let home_shared = home.path().to_path_buf();
    let mut handles = Vec::new();
    for i in 0..50u32 {
        let state = state_shared.clone();
        let home = home_shared.clone();
        handles.push(std::thread::spawn(move || {
            write_feedback(
                &state,
                &home,
                NOW_MS + i as i64,
                record(&format!("run-{i}")),
            )
            .expect("parallel leave")
        }));
    }
    let mut fids = Vec::new();
    for handle in handles {
        fids.push(handle.join().expect("join"));
    }
    assert_eq!(fids.len(), 50);
    assert_eq!(
        fids.iter().collect::<std::collections::HashSet<_>>().len(),
        50
    );

    let raw = std::fs::read_to_string(feedback_index_path(home.path())).expect("read index");
    assert!(raw.ends_with('\n'), "torn final line");
    let lines: Vec<&str> = raw.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 50, "exactly 50 lines, none torn");
    for line in &lines {
        assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
    }

    let index = read_feedback_index(home.path()).expect("index");
    assert_eq!(index.len(), 50);
    let records = std::fs::read_dir(state.path().join("feedback"))
        .expect("readdir feedback")
        .count();
    assert_eq!(records, 50);
}

#[test]
fn index_reader_tolerates_dangling_pointers_and_partial_lines() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");
    write_feedback(state.path(), home.path(), NOW_MS, record("r1")).expect("leave");

    let index = feedback_index_path(home.path());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&index)
        .expect("open index");
    // Dangling pointer: record file does not exist.
    writeln!(
        file,
        r#"{{"fid":"f-9999-9","path":"/nonexistent/f-9999-9.json","status":"open","kind":"parse-miss","run_id":"r","ts_ms":1}}"#
    )
    .expect("append dangling");
    // Torn trailing write: no newline.
    write!(file, r#"{{"fid":"f-8888-8","path":"/tmp"#).expect("append torn");
    drop(file);

    let entries = read_feedback_index(home.path()).expect("tolerant read");
    assert_eq!(entries.len(), 2, "real + dangling, torn tail skipped");

    let err = read_feedback(state.path(), "f-9999-9").unwrap_err();
    assert!(
        matches!(err, StoreError::FeedbackNotFound { ref fid, .. } if fid == "f-9999-9"),
        "{err:?}"
    );
}

#[test]
fn resolve_unknown_fid_is_typed_error() {
    let state = tempdir().expect("tempdir state");
    let home = tempdir().expect("tempdir home");

    let err = resolve_feedback(
        state.path(),
        home.path(),
        "f-1-1",
        FeedbackStatus::Wontfix,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, StoreError::FeedbackNotFound { .. }),
        "{err:?}"
    );
}
