//! Adversarial autodetection suite (task T7 deliverable f): hostile inputs —
//! NUL bytes, lossy-decoded UTF-16 garbage, ANSI-escape soup, long blank
//! prefixes, plain prose — must never panic `detect()` or `feed()`, plain
//! prose must never reach `error` severity via `generic`, and the structured
//! bases must pass on garbage. Parsers may assume lossy text: binary streams
//! are disabled upstream by the runner (docs/parsers.md §1.4).

use loglense_parsers::bases::{generic::GenericParser, json::JsonParser, logfmt::LogfmtParser};
use loglense_parsers::{
    detect_from_sample, FeedOutcome, LineCtx, Parser, ParserRegistry, Severity, Stream,
};

fn ctx(line_no: u64) -> LineCtx {
    LineCtx {
        stream: Stream::Out,
        line_no,
    }
}

/// Feed every line through every base parser; panics fail the test.
fn feed_all(lines: &[&str]) {
    let parsers: Vec<Box<dyn Parser>> = vec![
        Box::new(GenericParser),
        Box::new(JsonParser),
        Box::new(LogfmtParser),
    ];
    for (idx, line) in lines.iter().enumerate() {
        for parser in &parsers {
            let mut state = parser.new_run();
            let _ = state.feed(line, &ctx(idx as u64 + 1));
            let _ = state.finish();
        }
        let _ = detect_from_sample(&registry(), lines);
        for parser in &parsers {
            let _ = parser.detect(lines);
        }
    }
}

fn registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    loglense_parsers::bases::register(&mut registry);
    registry
}

/// Severity `generic` assigns to the first line (claims everything).
fn generic_severity(line: &str) -> Severity {
    let mut state = GenericParser.new_run();
    match state.feed(line, &ctx(1)) {
        FeedOutcome::Claimed(events) => events[0].severity,
        FeedOutcome::Passed => panic!("generic must claim every line, passed on {line:?}"),
    }
}

#[test]
fn nul_byte_lines_never_panic_any_base() {
    feed_all(&["\0", "a\0b", "ERROR\0: boom\0", "\0\0\0"]);
}

#[test]
fn utf16_lossy_garbage_never_panics_and_is_never_error() {
    // UTF-16LE bytes of an error-looking line, lossily decoded the way the
    // runner's line codec would: ASCII characters interleaved with NULs.
    let bytes: Vec<u8> = "ERROR: everything broke"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let lossy = String::from_utf8_lossy(&bytes).into_owned();
    let lines = [lossy.as_str()];
    feed_all(&lines);
    assert_eq!(
        generic_severity(&lossy),
        Severity::Info,
        "lossy UTF-16 garbage must not classify as error"
    );
}

#[test]
fn ansi_escape_soup_never_panics_and_is_info() {
    let lines = [
        "\u{1b}[31mERROR\u{1b}[0m something broke",
        "\u{1b}[2J\u{1b}[H\u{1b}[1;31mfatal\u{1b}[0m",
    ];
    feed_all(&lines);
    // ANSI stripping is the runner's job (upstream); unstripped soup must
    // fall through to info, never error.
    assert_eq!(generic_severity(lines[0]), Severity::Info);
}

#[test]
fn fifty_blank_lines_then_maven_text_detects_generic_and_never_panics() {
    let maven = [
        "[ERROR] Failed to execute goal on project app",
        "[INFO] BUILD FAILURE",
        "Tests run: 12, Failures: 2, Errors: 0, Skipped: 1",
    ];
    let mut lines: Vec<String> = vec![String::new(); 50];
    lines.extend(maven.iter().map(|line| line.to_string()));
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    feed_all(&refs);
    // No maven module is registered yet (T8), no line is JSON or logfmt, so
    // autodetection must fall back to generic.
    assert_eq!(detect_from_sample(&registry(), &refs), "generic");
    assert_eq!(
        generic_severity(maven[0]),
        Severity::Error,
        "contains 'failed'"
    );
    assert_eq!(
        generic_severity(maven[1]),
        Severity::Error,
        "contains 'BUILD FAILURE'"
    );
    // The test-totals line matches no generic keyword ('failures' is not
    // 'failed'); recognizing it is the maven tool parser's job (T8).
    assert_eq!(generic_severity(maven[2]), Severity::Info);
}

#[test]
fn plain_prose_is_never_error_and_structured_bases_pass() {
    let prose = [
        "the quick brown fox jumps over the lazy dog",
        "compiling module foo, 3 dependencies resolved",
        "done in 3 seconds",
    ];
    feed_all(&prose);
    for line in prose {
        assert_eq!(generic_severity(line), Severity::Info);
        assert_eq!(
            JsonParser.new_run().feed(line, &ctx(1)),
            FeedOutcome::Passed,
            "json must pass prose: {line}"
        );
        assert_eq!(
            LogfmtParser.new_run().feed(line, &ctx(1)),
            FeedOutcome::Passed,
            "logfmt must pass prose: {line}"
        );
    }
}

#[test]
fn empty_sample_detection_scores_zero_everywhere() {
    for parser in [&GenericParser as &dyn Parser, &JsonParser, &LogfmtParser] {
        assert_eq!(parser.detect(&[]), 0);
    }
    assert_eq!(detect_from_sample(&registry(), &[]), "generic");
}

#[test]
fn generic_claims_err_stream_lines_with_err_stream_tag() {
    let mut state = GenericParser.new_run();
    let err_ctx = LineCtx {
        stream: Stream::Err,
        line_no: 7,
    };
    match state.feed("boom", &err_ctx) {
        FeedOutcome::Claimed(events) => {
            assert_eq!(events[0].stream, Stream::Err);
            assert_eq!(events[0].lines, vec![7]);
        }
        FeedOutcome::Passed => panic!("generic claims everything"),
    }
}
