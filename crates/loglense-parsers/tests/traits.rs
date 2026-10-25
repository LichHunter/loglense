//! The module contract (docs/parsers.md §2) as object-safe traits: pass vs
//! claim is expressible, states stamp events from the [`LineCtx`], and every
//! member is usable through `dyn` trait objects.

use common::{MockBehavior, MockParser};
use loglense_parsers::{Event, FeedOutcome, LineCtx, Parser, Stream};

mod common;

fn out_ctx(line_no: u64) -> LineCtx {
    LineCtx {
        stream: Stream::Out,
        line_no,
    }
}

#[test]
fn parser_trait_is_object_safe_and_reports_identity() {
    let parser: Box<dyn Parser> =
        Box::new(MockParser::new("maven").with_extends("java").with_score(80));
    assert_eq!(parser.name(), "maven");
    assert_eq!(parser.extends(), Some("java"));
    assert_eq!(parser.detect(&["anything"]), 80);
}

#[test]
fn claim_all_state_stamps_stream_and_line_number() {
    let parser = MockParser::new("mock").with_behavior(MockBehavior::ClaimAll);
    let mut state = parser.new_run();
    let outcome = state.feed(
        "boom",
        &LineCtx {
            stream: Stream::Err,
            line_no: 7,
        },
    );
    match outcome {
        FeedOutcome::Claimed(events) => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].stream, Stream::Err, "stream comes from ctx");
            assert_eq!(events[0].lines, vec![7], "line number comes from ctx");
            assert_eq!(events[0].severity, loglense_parsers::Severity::Info);
            assert!(events[0].message.contains("boom"));
        }
        FeedOutcome::Passed => panic!("ClaimAll mock must claim"),
    }
}

#[test]
fn passed_and_silent_claim_are_distinct_outcomes() {
    // The reason `feed` returns an enum and not a bare Vec: a block-buffering
    // module consuming a continuation line (Claimed with no events) must be
    // distinguishable from a module declining the line (Passed).
    let passer = MockParser::new("passer");
    let mut passer_state = passer.new_run();
    assert_eq!(passer_state.feed("any", &out_ctx(1)), FeedOutcome::Passed);

    let blocker = MockParser::new("blocker").with_behavior(MockBehavior::OpenBlock("TRACE"));
    let mut blocker_state = blocker.new_run();
    assert_eq!(
        blocker_state.feed("TRACE begins", &out_ctx(2)),
        FeedOutcome::Claimed(Vec::new())
    );
    assert_eq!(
        blocker_state.feed("continuation", &out_ctx(3)),
        FeedOutcome::Claimed(Vec::new())
    );

    let flushed = blocker_state.finish();
    assert_eq!(flushed.len(), 1, "open block flushes at finish");
    assert!(flushed[0].message.contains("blocker"));
}

#[test]
fn finish_without_open_state_is_empty() {
    let parser = MockParser::new("mock").with_behavior(MockBehavior::ClaimAll);
    let mut state = parser.new_run();
    state.feed("line", &out_ctx(1));
    assert!(state.finish().is_empty());
}

#[test]
fn default_event_is_neutral() {
    // Pack rules and mocks build events via `..Event::default()`; the neutral
    // baseline must be info/line on stdout with no seq offset surprises.
    let event = Event::default();
    assert_eq!(event.severity, loglense_parsers::Severity::Info);
    assert_eq!(event.kind, loglense_parsers::Kind::Line);
    assert_eq!(event.stream, Stream::Out);
    assert_eq!(event.seq, 0);
    assert_eq!(event.ts_ms, 0);
}
