//! TDD unit 3: events.jsonl writer and tolerant reader.

use std::io::Write;

use loglense_core::{Event, Kind, Severity, Stream};
use loglense_store::{read_events, EventsWriter};
use tempfile::tempdir;

fn event(seq: u64, message: &str) -> Event {
    Event {
        seq,
        ts_ms: 1_764_000_000_000 + seq as i64,
        stream: Stream::Out,
        severity: Severity::Error,
        kind: Kind::Line,
        message: message.into(),
        ..Event::default()
    }
}

#[test]
fn writer_appends_events_as_json_lines() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let mut writer = EventsWriter::open(&path).expect("open");

    writer.append(&event(1, "first")).expect("append 1");
    writer.append(&event(2, "second")).expect("append 2");

    let raw = std::fs::read_to_string(&path).expect("read");
    let lines: Vec<&str> = raw.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"seq\":1"), "{}", lines[0]);
    assert!(lines[0].contains("\"message\":\"first\""), "{}", lines[0]);
    assert!(raw.ends_with('\n'));

    let (events, stats) = read_events(&path).expect("read_events");
    assert_eq!(stats.events, 2);
    assert_eq!(stats.skipped, 0);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].message, "second");
}

#[test]
fn reader_tolerates_truncated_tail_and_garbage_lines() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let mut file = std::fs::File::create(&path).expect("create");
    writeln!(
        file,
        "{}",
        serde_json::to_string(&event(1, "ok-1")).unwrap()
    )
    .unwrap();
    writeln!(file, "{{ this is not json").unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&event(2, "ok-2")).unwrap()
    )
    .unwrap();
    // Trailing partial line WITHOUT newline: a torn crash write.
    write!(file, "{{\"seq\":3,\"ts_ms\":1").unwrap();
    drop(file);

    let (events, stats) = read_events(&path).expect("read_events");

    assert_eq!(stats.events, 2);
    assert_eq!(stats.skipped, 2, "one garbage line + one torn tail");
    assert_eq!(
        events
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        vec!["ok-1", "ok-2"]
    );
}

#[test]
fn reader_handles_empty_and_missing_files() {
    let dir = tempdir().expect("tempdir");
    let empty = dir.path().join("empty.jsonl");
    std::fs::File::create(&empty).expect("create empty");

    let (_, stats) = read_events(&empty).expect("read empty");
    assert_eq!((stats.events, stats.skipped), (0, 0));

    let (_, missing_stats) = read_events(&dir.path().join("absent.jsonl")).expect("read missing");
    assert_eq!((missing_stats.events, missing_stats.skipped), (0, 0));
}
