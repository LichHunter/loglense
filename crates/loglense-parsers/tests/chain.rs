//! Chain intake (docs/parsers.md §1.4, §3.3): first-claim-wins over the
//! chain's parsers, per-stream state isolation, and end-of-stream flush.

mod common;

use common::{MockBehavior, MockParser};
use loglense_parsers::{FeedOutcome, LineCtx, Parser, ParserRegistry, Source, Stream};

fn ctx(stream: Stream, line_no: u64) -> LineCtx {
    LineCtx { stream, line_no }
}

fn chain_state(entries: &[(&'static str, MockBehavior)]) -> loglense_parsers::MultiChainState {
    let mut registry = ParserRegistry::new();
    let last = entries.len().saturating_sub(1);
    for (at, (name, behavior)) in entries.iter().enumerate() {
        let extends = if at == last {
            None
        } else {
            Some(entries[at + 1].0.to_string())
        };
        registry.register(
            name,
            MockParser::new(name)
                .with_behavior(*behavior)
                .with_extends(entries[(at + 1).min(last)].0)
                .ctor(),
            Source::Builtin,
            extends,
        );
    }
    let chain = registry.build_chain("top").expect("chain builds");
    chain.new_run()
}

#[test]
fn first_claim_stops_iteration() {
    let mut state = chain_state(&[
        ("top", MockBehavior::PassAll),
        ("middle", MockBehavior::PassAll),
        ("bottom", MockBehavior::ClaimAll),
    ]);
    let events = state.feed("hello", &ctx(Stream::Out, 1));
    assert_eq!(events.len(), 1, "bottom claims after two passes");
    let events = state.feed("again", &ctx(Stream::Out, 2));
    assert_eq!(events.len(), 1, "every line follows the same route");
}

#[test]
fn claim_recording_shows_who_was_fed() {
    let mut registry = ParserRegistry::new();
    let top = MockParser::new("top")
        .with_behavior(MockBehavior::PassAll)
        .with_extends("middle");
    let middle = MockParser::new("middle")
        .with_behavior(MockBehavior::PassAll)
        .with_extends("bottom");
    let bottom = MockParser::new("bottom").with_behavior(MockBehavior::ClaimAll);
    let top_rec = top.recording();
    let middle_rec = middle.recording();
    let bottom_rec = bottom.recording();
    registry.register("top", top.ctor(), Source::Builtin, Some("middle".into()));
    registry.register(
        "middle",
        middle.ctor(),
        Source::Builtin,
        Some("bottom".into()),
    );
    registry.register("bottom", bottom.ctor(), Source::Builtin, None);
    let chain = registry.build_chain("top").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["top", "middle", "bottom"]);
    let mut state = chain.new_run();
    let events = state.feed("hello", &ctx(Stream::Out, 1));
    assert_eq!(events.len(), 1, "bottom claims");
    assert_eq!(
        top_rec.fed_calls(),
        vec![("top", "hello".to_string())],
        "top was offered the line first"
    );
    assert_eq!(
        middle_rec.fed_calls(),
        vec![("middle", "hello".to_string())],
        "middle was offered second"
    );
    assert_eq!(
        bottom_rec.fed_calls(),
        vec![("bottom", "hello".to_string())],
        "bottom claimed"
    );
    let after = state.feed("second", &ctx(Stream::Out, 2));
    assert_eq!(after.len(), 1);
    assert_eq!(top_rec.fed_calls().len(), 2, "states persist across lines");
}

#[test]
fn earlier_claimer_beats_later_one() {
    let mut registry = ParserRegistry::new();
    let first = MockParser::new("first")
        .with_behavior(MockBehavior::ClaimAll)
        .with_extends("second");
    let second = MockParser::new("second").with_behavior(MockBehavior::ClaimAll);
    let second_rec = second.recording();
    registry.register(
        "first",
        first.ctor(),
        Source::Builtin,
        Some("second".into()),
    );
    registry.register("second", second.ctor(), Source::Builtin, None);
    let chain = registry.build_chain("first").expect("chain builds");
    let mut state = chain.new_run();
    let events = state.feed("x", &ctx(Stream::Out, 1));
    assert_eq!(events.len(), 1);
    assert!(
        events[0].message.starts_with("first:"),
        "first claimer wins"
    );
    assert!(
        second_rec.fed_calls().is_empty(),
        "second claimer never sees the line"
    );
}

#[test]
fn open_block_on_stdout_does_not_swallow_stderr() {
    let mut registry = ParserRegistry::new();
    let blocker = MockParser::new("blocker").with_behavior(MockBehavior::OpenBlock("TRACE"));
    let generic = MockParser::new("generic").with_behavior(MockBehavior::ClaimAll);
    let blocker_rec = blocker.recording();
    let generic_rec = generic.recording();
    registry.register(
        "blocker",
        blocker.with_extends("generic").ctor(),
        Source::Builtin,
        Some("generic".into()),
    );
    registry.register("generic", generic.ctor(), Source::Builtin, None);
    let chain = registry.build_chain("blocker").expect("chain builds");
    assert_eq!(chain.parser_names(), vec!["blocker", "generic"]);
    let mut state = chain.new_run();

    let open = state.feed("TRACE begin", &ctx(Stream::Out, 1));
    assert!(open.is_empty(), "block start is buffered, no event yet");

    let still_buffered = state.feed("plain stdout line", &ctx(Stream::Out, 2));
    assert!(
        still_buffered.is_empty(),
        "stdout line inside the open block is consumed silently"
    );

    let stderr_events = state.feed("plain stderr line", &ctx(Stream::Err, 1));
    assert_eq!(
        stderr_events.len(),
        1,
        "stderr routes to its own states and falls through to the claimer"
    );
    assert_eq!(stderr_events[0].stream, Stream::Err);
    assert!(stderr_events[0].message.starts_with("generic:"));

    let fed = generic_rec.fed_calls();
    assert_eq!(
        fed,
        vec![("generic", "plain stderr line".to_string())],
        "generic never sees stdout lines swallowed by the open block"
    );
    assert_eq!(
        blocker_rec.fed_calls().len(),
        3,
        "blocker sees both streams"
    );

    let flushed = state.finish();
    assert_eq!(flushed.len(), 1, "stdout block flushes at finish");
    assert!(flushed[0].message.contains("blocker flushed"));
    assert_eq!(
        flushed[0].stream,
        Stream::Out,
        "flush stamps the block's stream"
    );
}

#[test]
fn unclaimed_line_when_no_parser_claims_is_dropped() {
    let mut state = chain_state(&[("top", MockBehavior::PassAll)]);
    let events = state.feed("nobody wants me", &ctx(Stream::Out, 1));
    assert!(events.is_empty());
    assert!(state.finish().is_empty());
}

#[test]
fn finish_flushes_every_claiming_member_once() {
    let mut registry = ParserRegistry::new();
    let first = MockParser::new("first").with_behavior(MockBehavior::OpenBlock("A"));
    let second = MockParser::new("second").with_behavior(MockBehavior::OpenBlock("B"));
    registry.register("first", first.ctor(), Source::Builtin, None);
    registry.register(
        "second",
        second.with_extends("first").ctor(),
        Source::Builtin,
        Some("first".into()),
    );
    let chain = registry.build_chain("second").expect("chain builds");
    let mut state = chain.new_run();
    state.feed("A opens", &ctx(Stream::Out, 1));
    state.feed("B opens", &ctx(Stream::Out, 2));
    let flushed = state.finish();
    let mut messages: Vec<&str> = flushed.iter().map(|e| e.message.as_str()).collect();
    messages.sort_unstable();
    assert_eq!(messages, vec!["first flushed", "second flushed"]);
}

#[test]
fn feed_outcome_enum_is_matchable_by_callers() {
    let parser = MockParser::new("m").with_behavior(MockBehavior::ClaimAll);
    let mut raw_state = parser.new_run();
    match raw_state.feed("line", &ctx(Stream::Out, 9)) {
        FeedOutcome::Claimed(events) => assert_eq!(events[0].lines, vec![9]),
        FeedOutcome::Passed => panic!("must claim"),
    }
}
