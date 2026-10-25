//! Golden-fixture runner (docs/parsers.md §6.2): walks
//! `tests/fixtures/<parser>/<case>.{log,expected.json}` at TEST RUNTIME —
//! plain `std::fs` walk, no build-time codegen — so promoted fixtures from
//! `feedback promote` (T14) land here automatically and fail until their
//! expected array is filled in by a maintainer.
//!
//! Comparison is deep-equal on the full [`Event`], ignoring `ts_ms` (runner
//! owns it; both sides zeroed), `template_id` (dedup-owned) and with `seq`
//! re-stamped 1..n on the actual side — exactly what the runner does at
//! capture time.
//!
//! Adding a parser (T8-T10 pattern): create `tests/fixtures/<name>/`
//! cases, then add one arm to [`parser_for`].

use std::fs;
use std::path::{Path, PathBuf};

use loglense_parsers::{Event, FeedOutcome, LineCtx, Parser, Stream};

/// Fixture root, resolved from the crate manifest so the test works from any
/// CWD.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// One golden case: parser dir name plus the `<case>.log` path.
struct Case {
    parser: String,
    log: PathBuf,
}

/// Constructor map: fixture dir name → fresh parser instance. This is the
/// single extension point new parsers touch (one match arm per parser).
fn parser_for(dir: &str) -> Option<Box<dyn Parser>> {
    match dir {
        "generic" => Some(Box::new(loglense_parsers::bases::generic::GenericParser)),
        "json" => Some(Box::new(loglense_parsers::bases::json::JsonParser)),
        "logfmt" => Some(Box::new(loglense_parsers::bases::logfmt::LogfmtParser)),
        _ => None,
    }
}

/// Discover every `<parser>/<case>.log` under [`FIXTURES`], parser dirs and
/// cases both sorted for deterministic failure output.
fn discover_cases() -> Vec<Case> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(FIXTURES)
        .expect("fixtures root exists")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    let mut cases = Vec::new();
    for dir in dirs {
        let mut logs: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("fixture dir readable")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
            .collect();
        logs.sort();
        for log in logs {
            cases.push(Case {
                parser: dir
                    .file_name()
                    .expect("dir name")
                    .to_string_lossy()
                    .into_owned(),
                log,
            });
        }
    }
    cases
}

/// Feed all lines of `log` through a fresh state of `parser`, stamp the
/// collected events like the runner would (`seq` 1..n, `ts_ms` 0, no
/// template), and return them.
fn parse_events(parser: &dyn Parser, log: &Path) -> Vec<Event> {
    let text = fs::read_to_string(log).expect("case .log readable");
    let mut state = parser.new_run();
    let mut events = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let ctx = LineCtx {
            stream: Stream::Out,
            line_no: idx as u64 + 1,
        };
        match state.feed(line, &ctx) {
            FeedOutcome::Claimed(mut batch) => events.append(&mut batch),
            FeedOutcome::Passed => {}
        }
    }
    events.extend(state.finish());
    for (idx, event) in events.iter_mut().enumerate() {
        event.seq = idx as u64 + 1;
        event.ts_ms = 0;
        event.template_id = None;
    }
    events
}

/// Normalize expected events the same way: `ts_ms` zeroed, `template_id`
/// dropped. `seq` stays as authored (must match the runner's 1..n stamping).
fn load_expected(path: &Path) -> Vec<Event> {
    let text = fs::read_to_string(path).expect("expected.json readable");
    let mut expected: Vec<Event> =
        serde_json::from_str(&text).expect("expected.json is a JSON array of events");
    for event in &mut expected {
        event.ts_ms = 0;
        event.template_id = None;
    }
    expected
}

#[test]
fn golden_fixtures_match_expected_events() {
    let cases = discover_cases();
    assert!(
        !cases.is_empty(),
        "golden runner found no fixture cases under {FIXTURES} — the runner itself is broken"
    );

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let name = case.log.file_name().expect("case name").to_string_lossy();
        let case_id = format!("{}/{}", case.parser, name);
        let Some(parser) = parser_for(&case.parser) else {
            failures.push(format!(
                "case {case_id}: no constructor for fixture dir '{}' — extend parser_for() in tests/golden.rs",
                case.parser
            ));
            continue;
        };
        let expected_path = case.log.with_extension("expected.json");
        if !expected_path.exists() {
            failures.push(format!(
                "case {case_id}: missing {} — promoted fixtures fail until filled",
                expected_path.display()
            ));
            continue;
        }
        let actual = parse_events(parser.as_ref(), &case.log);
        let expected = load_expected(&expected_path);
        if actual.len() != expected.len() {
            failures.push(format!(
                "case {case_id}: expected {} events, got {} (expected: {expected:?}, actual: {actual:?})",
                expected.len(),
                actual.len()
            ));
            continue;
        }
        for (idx, (exp, act)) in expected.iter().zip(&actual).enumerate() {
            if exp != act {
                failures.push(format!(
                    "case {case_id}: event[{idx}] differs\n  expected: {exp:?}\n  actual:   {act:?}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "golden fixture failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
