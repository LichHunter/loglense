//! Integration tests for [`RunId`]: format, parse/display roundtrip,
//! rejection of malformed shapes, ordering, and collision-aware creation.

use std::fs;
use std::str::FromStr;

use loglense_core::{RunId, RunIdError};
use time::{Date, Month};

fn date(year: i32, month: Month, day: u8) -> Date {
    Date::from_calendar_date(year, month, day).expect("valid calendar date")
}

#[test]
fn generate_matches_documented_format() {
    let id = RunId::generate(date(2026, Month::October, 24), 0);
    let s = id.to_string();
    assert_eq!(s.len(), 15, "id must be 15 chars: {s}");
    assert_eq!(&s[8..9], "-", "separator must be `-` at position 8");
    assert!(
        s[..8].bytes().all(|b| b.is_ascii_digit()),
        "date part must be 8 digits: {s}"
    );
    assert!(
        s[9..].bytes().all(is_crockford),
        "random part must be Crockford base32: {s}"
    );
}

fn is_crockford(b: u8) -> bool {
    matches!(
        b,
        b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'
    )
}

#[test]
fn generate_is_deterministic_per_entropy() {
    let d = date(2026, Month::January, 2);
    assert_eq!(RunId::generate(d, 12_345), RunId::generate(d, 12_345));
    assert_eq!(RunId::generate(d, 0).to_string(), "20260102-000000");
    assert_eq!(RunId::generate(d, 31).to_string(), "20260102-00000Z");
    assert_eq!(RunId::generate(d, 32).to_string(), "20260102-000010");
    assert_eq!(RunId::generate(d, 42).to_string(), "20260102-00001A");
}

#[test]
fn display_fromstr_roundtrip() {
    for s in ["20260102-000000", "20261024-A3F8C2", "19991231-ZZZZZZ"] {
        let id = RunId::from_str(s).expect("valid id");
        assert_eq!(id.as_str(), s);
        assert_eq!(id.to_string(), s);
    }
}

#[test]
fn fromstr_rejects_wrong_length() {
    let err = RunId::from_str("20261024-ABC").expect_err("12 chars is malformed");
    assert!(
        matches!(err, RunIdError::InvalidLength { .. }),
        "got: {err:?}"
    );
}

#[test]
fn fromstr_rejects_missing_separator() {
    let err = RunId::from_str("20261024XABC123").expect_err("missing `-`");
    assert!(
        matches!(err, RunIdError::MissingSeparator { .. }),
        "got: {err:?}"
    );
}

#[test]
fn fromstr_rejects_excluded_crockford_letters() {
    // I, L, O are excluded from the alphabet.
    let err = RunId::from_str("20261024-ABCILO").expect_err("I/L/O are not in the alphabet");
    assert!(
        matches!(err, RunIdError::InvalidCharacter { .. }),
        "got: {err:?}"
    );
}

#[test]
fn fromstr_rejects_lowercase_random_part() {
    let err = RunId::from_str("20261024-abc123").expect_err("lowercase is not in the alphabet");
    assert!(
        matches!(err, RunIdError::InvalidCharacter { .. }),
        "got: {err:?}"
    );
}

#[test]
fn fromstr_rejects_impossible_dates() {
    for bad in ["20261399-ABC123", "20260230-ABC123", "20260000-ABC123"] {
        let err = RunId::from_str(bad).expect_err("impossible calendar date");
        assert!(
            matches!(err, RunIdError::InvalidDate { .. }),
            "{bad} got: {err:?}"
        );
    }
}

#[test]
fn error_messages_name_the_problem() {
    let probes = [
        ("20261024-ABC", "expected 15 chars"),
        ("20261024XABC123", "expected `-` at position 8"),
        ("20261024-ABCILO", "invalid character `I` at position 12"),
        ("20261024-abc123", "invalid character `a` at position 9"),
        ("20260230-ABC123", "is not a valid calendar date"),
    ];
    for (input, expected_fragment) in probes {
        let err = RunId::from_str(input).expect_err("malformed id");
        let msg = err.to_string();
        eprintln!("probe {input:?} -> {msg}");
        assert!(msg.contains(expected_fragment), "{input}: {msg}");
    }
}

#[test]
fn ordering_is_date_then_random_part() {
    let a = RunId::from_str("20260101-000000").expect("valid");
    let b = RunId::from_str("20260101-000001").expect("valid");
    let z = RunId::from_str("20260101-ZZZZZZ").expect("valid");
    let c = RunId::from_str("20260102-000000").expect("valid");
    assert!(a < b, "same date: rand orders");
    assert!(b < z);
    assert!(z < c, "date dominates the random part");
}

#[test]
fn create_run_dir_returns_fresh_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let (id, dir) = RunId::create_run_dir(root.path(), date(2026, Month::October, 24), || 7)
        .expect("first create succeeds");
    assert_eq!(dir, root.path().join(id.as_str()));
    assert!(dir.is_dir());
}

#[test]
fn create_run_dir_retries_on_collision() {
    let root = tempfile::tempdir().expect("tempdir");
    let d = date(2026, Month::October, 24);
    let (id1, dir1) = RunId::create_run_dir(root.path(), d, || 7).expect("first create");
    assert!(dir1.is_dir());

    // Same entropy first (collides with id1), then a different one.
    let sequence = [7u32, 8];
    let mut next = 0;
    let (id2, dir2) = RunId::create_run_dir(root.path(), d, move || {
        let v = sequence[next];
        next += 1;
        v
    })
    .expect("second create regenerates after EEXIST");
    assert_ne!(id1, id2);
    assert_ne!(dir1, dir2);
    assert!(dir2.is_dir(), "regenerated dir must exist");
}

#[test]
fn create_run_dir_errors_after_five_collisions() {
    let root = tempfile::tempdir().expect("tempdir");
    let d = date(2026, Month::October, 24);
    // Pre-create the single dir this constant entropy source always proposes.
    let target = root.path().join(RunId::generate(d, 42).to_string());
    fs::create_dir(&target).expect("pre-create colliding dir");

    let err = RunId::create_run_dir(root.path(), d, || 42)
        .expect_err("5 collisions must exhaust the retry budget");
    assert!(
        matches!(err, RunIdError::CollisionExhausted { tries: 5, .. }),
        "got: {err:?}"
    );
}

#[test]
fn create_run_dir_propagates_io_errors() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing_parent = root.path().join("does-not-exist");
    let err = RunId::create_run_dir(&missing_parent, date(2026, Month::January, 1), || 1)
        .expect_err("missing parent dir is an I/O error, not a collision");
    assert!(matches!(err, RunIdError::DirCreate { .. }), "got: {err:?}");
}

#[test]
fn runid_serde_roundtrip_and_rejects_invalid() {
    let id = RunId::from_str("20261024-A3F8C2").expect("valid");
    let json = serde_json::to_string(&id).expect("serialize");
    assert_eq!(json, "\"20261024-A3F8C2\"");
    let back: RunId = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, id);
    let bad: Result<RunId, _> = serde_json::from_str("\"nope\"");
    assert!(bad.is_err(), "serde must reject malformed ids too");
}
