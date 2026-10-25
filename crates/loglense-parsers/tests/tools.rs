//! Tool parser wiring (task T8 deliverable e): `maven` registers over the
//! `java` base, chains order derived-before-base, autodetection prefers
//! maven on maven logs and java on plain java logs, the java block state
//! machine claims continuations silently and emits one event per block,
//! and maven lines interrupting an open block behave per the chain
//! contract.

use loglense_parsers::bases::java::JavaParser;
use loglense_parsers::tools::maven::MavenParser;
use loglense_parsers::{
    bases, detect_from_sample, tools, FeedOutcome, Kind, LineCtx, Parser, ParserRegistry,
    Registration, Source, Stream,
};

fn registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    bases::register(&mut registry);
    tools::register(&mut registry);
    registry
}

fn ctx(line_no: u64) -> LineCtx {
    LineCtx {
        stream: Stream::Out,
        line_no,
    }
}

const MAVEN_SAMPLE: [&str; 4] = [
    "[INFO] Scanning for projects...",
    "[INFO] --- maven-surefire-plugin:3.0.0:test (default-test) @ core ---",
    "[ERROR] Tests run: 4, Failures: 1, Errors: 1, Skipped: 0",
    "[INFO] BUILD FAILURE",
];

const JAVA_SAMPLE: [&str; 4] = [
    "[INFO] Server started on port 8080",
    "java.lang.NullPointerException: cart is null",
    "    at com.example.OrderService.validate(OrderService.java:120)",
    "    at com.example.OrderService.total(OrderService.java:87)",
];

#[test]
fn maven_registers_builtin_extending_java_and_chains_derived_first() {
    let registry = registry();
    let maven = registry
        .registrations()
        .into_iter()
        .find(|row: &Registration| row.name == "maven")
        .expect("maven registered");
    assert_eq!(maven.source, Source::Builtin);
    assert_eq!(maven.extends, Some("java"));
    registry.resolve().expect("extends target known, no cycle");
    assert_eq!(
        registry.build_chain("maven").expect("chain").parser_names(),
        vec!["maven", "java", "generic"]
    );
    assert_eq!(
        registry.build_chain("java").expect("chain").parser_names(),
        vec!["java", "generic"]
    );
}

#[test]
fn maven_outscores_java_base_on_maven_log() {
    assert_eq!(detect_from_sample(&registry(), &MAVEN_SAMPLE), "maven");
    assert!(
        MavenParser.detect(&MAVEN_SAMPLE) > JavaParser.detect(&MAVEN_SAMPLE),
        "maven {} must beat java {} on maven text",
        MavenParser.detect(&MAVEN_SAMPLE),
        JavaParser.detect(&MAVEN_SAMPLE)
    );
}

#[test]
fn java_base_outscores_maven_on_plain_java_log() {
    assert_eq!(detect_from_sample(&registry(), &JAVA_SAMPLE), "java");
    assert!(
        JavaParser.detect(&JAVA_SAMPLE) > MavenParser.detect(&JAVA_SAMPLE),
        "java {} must beat maven {} on plain java text",
        JavaParser.detect(&JAVA_SAMPLE),
        MavenParser.detect(&JAVA_SAMPLE)
    );
}

#[test]
fn java_block_claims_continuations_silently_then_emits_one_event() {
    let mut state = JavaParser.new_run();
    assert_eq!(
        state.feed("java.lang.IllegalStateException: boom", &ctx(1)),
        FeedOutcome::Claimed(Vec::new())
    );
    assert_eq!(
        state.feed("    at com.example.Cart.checkout(Cart.java:45)", &ctx(2)),
        FeedOutcome::Claimed(Vec::new())
    );
    assert_eq!(
        state.feed(
            "Caused by: java.lang.NullPointerException: null cart",
            &ctx(3)
        ),
        FeedOutcome::Claimed(Vec::new())
    );
    let FeedOutcome::Claimed(events) = state.feed("        ... 2 more", &ctx(4)) else {
        panic!("terminator must claim");
    };
    assert_eq!(events.len(), 1, "one event per block");
    let event = &events[0];
    assert_eq!(event.severity, loglense_parsers::Severity::Error);
    assert_eq!(event.kind, Kind::Line);
    assert_eq!(
        event.message,
        "java.lang.IllegalStateException: boom ... caused by java.lang.NullPointerException: null cart"
    );
    assert_eq!(event.lines, vec![1, 2, 3, 4]);
    assert_eq!(
        event.frames,
        vec!["at com.example.Cart.checkout(Cart.java:45)"]
    );
}

#[test]
fn java_block_close_leaves_closing_line_unconsumed() {
    let mut state = JavaParser.new_run();
    state.feed("java.lang.NullPointerException: no frames follow", &ctx(1));
    assert_eq!(
        state.feed("unrelated prose closes the block", &ctx(2)),
        FeedOutcome::Passed,
        "the closing line stays unconsumed for the next chain member"
    );
    // The completed block event ships with the next claim, ordered before it.
    let FeedOutcome::Claimed(events) = state.feed("[INFO] after the block", &ctx(3)) else {
        panic!("bracket line must be claimed");
    };
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].message,
        "java.lang.NullPointerException: no frames follow"
    );
    assert_eq!(events[0].lines, vec![1]);
    assert_eq!(events[1].message, "after the block");
}

#[test]
fn truncated_trace_flushes_as_one_event_at_finish() {
    let mut state = JavaParser.new_run();
    state.feed("com.example.WidgetError: widget malformed", &ctx(1));
    state.feed("    at com.example.Widget.parse(Widget.java:31)", &ctx(2));
    let events = state.finish();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].message,
        "com.example.WidgetError: widget malformed"
    );
    assert_eq!(events[0].lines, vec![1, 2]);
    assert_eq!(
        events[0].frames,
        vec!["at com.example.Widget.parse(Widget.java:31)"]
    );
}

#[test]
fn header_without_message_renders_bare_exception_type() {
    let mut state = JavaParser.new_run();
    state.feed("java.lang.ClassCastException", &ctx(1));
    let events = state.finish();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message, "java.lang.ClassCastException");
}

#[test]
fn caused_by_chain_keeps_the_last_link_in_the_message() {
    let mut state = JavaParser.new_run();
    state.feed("java.lang.IllegalStateException: outer", &ctx(1));
    state.feed("    at a.B.C(B.java:1)", &ctx(2));
    state.feed(
        "Caused by: java.lang.IllegalArgumentException: first cause",
        &ctx(3),
    );
    state.feed("    at a.B.D(D.java:2)", &ctx(4));
    state.feed(
        "Caused by: java.lang.NullPointerException: root cause",
        &ctx(5),
    );
    let FeedOutcome::Claimed(events) = state.feed("        ... 3 more", &ctx(6)) else {
        panic!("terminator must claim");
    };
    assert_eq!(events.len(), 1);
    let message = &events[0].message;
    assert!(
        message.ends_with("... caused by java.lang.NullPointerException: root cause"),
        "last Caused by link wins: {message}"
    );
    assert!(!message.contains("first cause"), "dropped link: {message}");
    assert_eq!(events[0].lines, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(events[0].frames.len(), 2, "frames from both links");
}

#[test]
fn maven_info_line_interrupting_a_block_emits_trace_then_claims_itself() {
    let mut state = MavenParser.new_run();
    assert_eq!(
        state.feed("java.lang.IllegalStateException: first failure", &ctx(1)),
        FeedOutcome::Claimed(Vec::new())
    );
    assert_eq!(
        state.feed("    at a.B.C(B.java:1)", &ctx(2)),
        FeedOutcome::Claimed(Vec::new())
    );
    // The maven INFO line closes the java block AND is claimed by the
    // maven bracket rule — both events arrive together, trace first.
    let FeedOutcome::Claimed(events) = state.feed("[INFO] Results:", &ctx(3)) else {
        panic!("maven must claim its own INFO line");
    };
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].message,
        "java.lang.IllegalStateException: first failure"
    );
    assert_eq!(events[0].lines, vec![1, 2]);
    assert_eq!(events[1].message, "Results:");
    // A second trace starting right after parses identically.
    assert_eq!(
        state.feed("java.lang.IllegalStateException: second failure", &ctx(4)),
        FeedOutcome::Claimed(Vec::new())
    );
    assert_eq!(
        state.feed("    at a.B.D(D.java:2)", &ctx(5)),
        FeedOutcome::Claimed(Vec::new())
    );
    let events = state.finish();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].message,
        "java.lang.IllegalStateException: second failure"
    );
    assert_eq!(events[0].lines, vec![4, 5]);
}

#[test]
fn maven_rule_events_carry_summary_and_phase_kinds() {
    let mut state = MavenParser.new_run();
    let FeedOutcome::Claimed(events) = state.feed(
        "[ERROR] Tests run: 3, Failures: 2, Errors: 0, Skipped: 1",
        &ctx(1),
    ) else {
        panic!("totals line must be claimed");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::Summary);
    assert_eq!(events[0].severity, loglense_parsers::Severity::Error);
    assert_eq!(
        events[0].message,
        "Tests run: 3, Failures: 2, Errors: 0, Skipped: 1"
    );

    let FeedOutcome::Claimed(events) = state.feed("[INFO] BUILD SUCCESS", &ctx(2)) else {
        panic!("verdict line must be claimed");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::Summary);
    assert_eq!(events[0].severity, loglense_parsers::Severity::Info);

    let FeedOutcome::Claimed(events) = state.feed("BUILD FAILURE", &ctx(3)) else {
        panic!("bare verdict must be claimed");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].severity, loglense_parsers::Severity::Error);
    assert_eq!(events[0].message, "BUILD FAILURE");

    let FeedOutcome::Claimed(events) = state.feed(
        "[INFO] core ....................................... FAILURE [  0.512 s]",
        &ctx(4),
    ) else {
        panic!("reactor row must be claimed");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::Phase);
    assert_eq!(events[0].phase.as_deref(), Some("core"));
    assert_eq!(events[0].message, "core: FAILURE");
    assert_eq!(events[0].severity, loglense_parsers::Severity::Error);
}
