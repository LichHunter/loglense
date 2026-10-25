//! Pump unit tests: block cap, sink-failure truncation, template dedup,
//! autodetect buffering. Real parsers run through the real registry; the
//! only synthetic piece is the `hoarder` (a worst-case block-buffering
//! parser) and the failing sink seam.

use std::cell::RefCell;
use std::rc::Rc;

use loglense_core::{Event, Stream};
use loglense_parsers::traits::{FeedOutcome, LineCtx, ParseState, Parser};
use loglense_parsers::Source;
use loglense_store::StoreError;

use super::{EventSink, Pump, Selection, BLOCK_CAP};

/// Records appended events into a shared log the test inspects while the
/// pump still holds the sink.
struct RecordingSink {
    events: Rc<RefCell<Vec<Event>>>,
}

fn recording() -> (RecordingSink, Rc<RefCell<Vec<Event>>>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    (
        RecordingSink {
            events: Rc::clone(&events),
        },
        events,
    )
}

impl EventSink for RecordingSink {
    fn append(&mut self, event: &Event) -> Result<(), StoreError> {
        self.events.borrow_mut().push(event.clone());
        Ok(())
    }
}

struct FailingSink;

impl EventSink for FailingSink {
    fn append(&mut self, _event: &Event) -> Result<(), StoreError> {
        Err(StoreError::Io {
            path: "events.jsonl".into(),
            source: std::io::Error::other("simulated ENOSPC"),
        })
    }
}

/// Claims every line but stays silent until `finish` — a worst-case
/// block-buffering parser that exercises the block cap.
struct Hoarder;

const HOARD_NAME: &str = "hoarder";

impl Parser for Hoarder {
    fn name(&self) -> &'static str {
        HOARD_NAME
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    fn detect(&self, _sample: &[&str]) -> u8 {
        0
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(HoardState { held: 0 })
    }
}

/// Lines held since the last flush; an idle state flushes nothing (like
/// every real block parser).
struct HoardState {
    held: u64,
}

impl ParseState for HoardState {
    fn feed(&mut self, _line: &str, _ctx: &LineCtx) -> FeedOutcome {
        self.held += 1;
        FeedOutcome::Claimed(Vec::new())
    }

    fn finish(&mut self) -> Vec<Event> {
        if std::mem::take(&mut self.held) == 0 {
            return Vec::new();
        }
        vec![Event {
            message: "hoard-flush".to_string(),
            ..Event::default()
        }]
    }
}

fn registry() -> loglense_parsers::ParserRegistry {
    let mut reg = loglense_parsers::ParserRegistry::new();
    loglense_parsers::bases::register(&mut reg);
    reg
}

fn hoard_registry() -> loglense_parsers::ParserRegistry {
    let mut reg = registry();
    reg.register(HOARD_NAME, || Box::new(Hoarder), Source::Builtin, None);
    reg
}

#[test]
fn test_block_cap_force_flushes_silent_blocks() {
    let (mut sink, events) = recording();
    let reg = hoard_registry();
    let mut pump = Pump::new(&mut sink, &reg, Selection::Named(HOARD_NAME.to_string()));
    for line in 1..=(BLOCK_CAP + 2) {
        pump.handle_line(Stream::Out, line, "x");
    }
    assert!(
        !events.borrow().is_empty(),
        "the cap must flush the open block before EOF"
    );
    assert_eq!(events.borrow()[0].message, "hoard-flush");
    pump.handle_eof(Stream::Out);
    assert_eq!(
        events.borrow().len(),
        2,
        "post-cap lines flush again at EOF"
    );
}

#[test]
fn test_sink_failure_truncates_and_stops_parsing() {
    let mut sink = FailingSink;
    let reg = registry();
    let mut pump = Pump::new(&mut sink, &reg, Selection::Named("generic".to_string()));
    pump.handle_line(Stream::Out, 1, "ERROR boom");
    assert!(
        pump.capture.truncated.is_some(),
        "a write failure must truncate the run"
    );
    pump.handle_line(Stream::Out, 2, "more");
    pump.handle_eof(Stream::Out);
    let capture = pump.into_capture();
    assert_eq!(capture.events, 0);
    assert!(
        capture
            .truncated
            .as_deref()
            .is_some_and(|r| r.contains("simulated ENOSPC")),
        "the truncation reason names the write failure"
    );
}

#[test]
fn test_templates_dedupe_similar_lines() {
    let (mut sink, events) = recording();
    let reg = registry();
    let mut pump = Pump::new(&mut sink, &reg, Selection::Named("generic".to_string()));
    pump.handle_line(Stream::Out, 1, "ERROR boom 1");
    pump.handle_line(Stream::Out, 2, "ERROR boom 2");
    pump.handle_eof(Stream::Out);
    let capture = pump.into_capture();
    assert_eq!(capture.events, 2);
    assert_eq!(
        events.borrow()[0].template_id,
        events.borrow()[1].template_id
    );
    let id = events.borrow()[0].template_id.clone().unwrap_or_default();
    assert!(
        capture.templates.get(&id).is_some_and(|g| g.count == 2),
        "both lines collapse into one template of count 2"
    );
}

#[test]
fn test_autodetect_buffers_until_eof_then_resolves() {
    let (mut sink, events) = recording();
    let reg = registry();
    let mut pump = Pump::new(&mut sink, &reg, Selection::Auto);
    for i in 1..=3 {
        pump.handle_line(
            Stream::Out,
            i,
            &format!("{{\"level\":\"info\",\"msg\":\"n {i}\"}}"),
        );
    }
    assert!(events.borrow().is_empty(), "lines buffer while detecting");
    pump.handle_eof(Stream::Out);
    let capture = pump.into_capture();
    assert_eq!(capture.parser_out.as_deref(), Some("json"));
    assert_eq!(capture.events, 3, "buffered lines parse after resolve");
}

#[test]
fn test_sample_deadline_release_picks_a_parser() {
    let (mut sink, events) = recording();
    let reg = registry();
    let mut pump = Pump::new(&mut sink, &reg, Selection::Auto);
    pump.handle_line(Stream::Err, 1, "just one line held open");
    // before the 2 s window elapses nothing is emitted
    pump.release_deadlines(std::time::Instant::now());
    assert!(events.borrow().is_empty());
    // after the window the sample resolves and parses
    pump.release_deadlines(std::time::Instant::now() + std::time::Duration::from_secs(3));
    pump.handle_eof(Stream::Err);
    let capture = pump.into_capture();
    assert_eq!(capture.events, 1);
    assert_eq!(capture.parser_err.as_deref(), Some("generic"));
}
