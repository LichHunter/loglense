//! Integration tests for the [`Event`] model: JSON roundtrip, wire field
//! names per architecture.md §3.5, unknown-field tolerance, defaults.

use loglense_core::{Event, Kind, Severity, Source, Stream, TestInfo};

#[test]
fn full_event_serializes_with_wire_field_names() {
    let event = Event {
        seq: 7,
        ts_ms: 1_728_547_200_000,
        stream: Stream::Err,
        severity: Severity::Warning,
        kind: Kind::Test,
        message: "test_login failed".into(),
        template_id: Some("506f80de74ce".into()),
        source: Some(Source {
            file: "tests/auth.rs".into(),
            line: 42,
            module: Some("auth::login".into()),
        }),
        lines: vec![10, 11, 12],
        frames: vec!["auth::login at /app/src/auth.rs:42".into()],
        phase: Some("integration-tests".into()),
        test: Some(TestInfo {
            name: "test_login".into(),
            outcome: "failed".into(),
            duration_ms: Some(3_150),
        }),
        totals: [("failed".to_string(), 1)].into_iter().collect(),
    };
    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json["seq"], 7, "field name must be `seq`: {json}");
    let ts_ms: i64 = 1_728_547_200_000;
    assert_eq!(json["ts_ms"], ts_ms, "field name must be `ts_ms`: {json}");
    assert_eq!(
        json["stream"], "err",
        "stream enum is snake_case on the wire: {json}"
    );
    assert_eq!(
        json["severity"], "warning",
        "severity enum is snake_case: {json}"
    );
    assert_eq!(json["kind"], "test", "kind enum is snake_case: {json}");
    assert_eq!(json["message"], "test_login failed");
    assert_eq!(json["template_id"], "506f80de74ce");
    assert_eq!(json["source"]["file"], "tests/auth.rs");
    assert_eq!(json["source"]["line"], 42);
    assert_eq!(json["source"]["module"], "auth::login");
    assert_eq!(json["lines"], serde_json::json!([10, 11, 12]));
    assert_eq!(
        json["frames"],
        serde_json::json!(["auth::login at /app/src/auth.rs:42"])
    );
    assert_eq!(json["phase"], "integration-tests");
    assert_eq!(json["test"]["name"], "test_login");
    assert_eq!(json["test"]["outcome"], "failed");
    assert_eq!(json["test"]["duration_ms"], 3_150);
}

#[test]
fn full_event_json_roundtrip_is_lossless() {
    let event = Event {
        seq: 1,
        ts_ms: -1, // pre-epoch timestamps must survive (signed i64)
        stream: Stream::Out,
        severity: Severity::Error,
        kind: Kind::Summary,
        message: "BUILD FAILURE".into(),
        template_id: Some("deadbeef0000".into()),
        source: None,
        lines: vec![99],
        frames: vec![],
        phase: None,
        test: None,
        totals: Default::default(),
    };
    let json = serde_json::to_string(&event).expect("serialize");
    let back: Event = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, event);
}

#[test]
fn optional_fields_are_omitted_when_none() {
    let event = Event {
        seq: 2,
        ts_ms: 5,
        stream: Stream::Out,
        severity: Severity::Info,
        kind: Kind::Line,
        message: "hello".into(),
        template_id: None,
        source: None,
        lines: vec![1],
        frames: vec![],
        phase: None,
        test: None,
        totals: Default::default(),
    };
    let json = serde_json::to_value(&event).expect("serialize");
    for absent in ["template_id", "source", "frames", "phase", "test"] {
        assert!(
            json.get(absent).is_none(),
            "`{absent}` must be omitted when empty/None: {json}"
        );
    }
    assert_eq!(
        json["lines"],
        serde_json::json!([1]),
        "`lines` is always present"
    );
}

#[test]
fn deserializing_tolerates_unknown_fields() {
    // Forwards compatibility: future grammar versions may add fields; old
    // readers must keep parsing (architecture.md §3.5).
    let json = serde_json::json!({
        "seq": 3,
        "ts_ms": 1000,
        "stream": "out",
        "severity": "debug",
        "kind": "line",
        "message": "m",
        "lines": [4],
        "future_field": {"nested": [1, 2, 3]},
        "another_future_field": 7,
    });
    let event: Event = serde_json::from_value(json).expect("unknown fields are tolerated");
    assert_eq!(event.severity, Severity::Debug);
    assert_eq!(event.lines, vec![4]);
}

#[test]
fn deserializing_tolerates_missing_optional_collections() {
    // Tolerant readers (NFR-04): `frames` may be absent in older events.
    let json = serde_json::json!({
        "seq": 4,
        "ts_ms": 6,
        "stream": "out",
        "severity": "notice",
        "kind": "phase",
        "message": "phase only",
        "lines": [8],
    });
    let event: Event = serde_json::from_value(json).expect("missing frames tolerated");
    assert_eq!(event.kind, Kind::Phase);
    assert!(event.frames.is_empty());
    assert_eq!(event.severity, Severity::Notice);
}

#[test]
fn default_event_is_info_line_out() {
    let event = Event::default();
    assert_eq!(event.severity, Severity::Info);
    assert_eq!(event.kind, Kind::Line);
    assert_eq!(event.stream, Stream::Out);
    assert_eq!(event.seq, 0);
    assert_eq!(event.ts_ms, 0);
    assert!(event.message.is_empty());
    assert!(event.lines.is_empty());
}

#[test]
fn all_variants_roundtrip_through_json() {
    let streams = [Stream::Out, Stream::Err];
    let severities = [
        Severity::Error,
        Severity::Warning,
        Severity::Notice,
        Severity::Info,
        Severity::Debug,
    ];
    let kinds = [Kind::Line, Kind::Phase, Kind::Summary, Kind::Test];
    for &stream in &streams {
        for &severity in &severities {
            for &kind in &kinds {
                let event = Event {
                    stream,
                    severity,
                    kind,
                    ..Event::default()
                };
                let json = serde_json::to_string(&event).expect("serialize");
                let back: Event = serde_json::from_str(&json).expect("deserialize");
                assert_eq!(back, event, "roundtrip failed for {json}");
            }
        }
    }
}
