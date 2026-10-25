//! Shared mock parsers for the loglense-parsers integration tests.
//!
//! Used by the T6 framework tests and by the T7-T10 parser waves. Each test
//! binary compiles this module separately, so per-binary dead-code warnings
//! are expected and silenced.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use loglense_parsers::{Event, FeedOutcome, LineCtx, ParseState, Parser, Stream};

/// What a [`MockState`] does with each offered line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockBehavior {
    /// Never claim: [`FeedOutcome::Passed`] for every line.
    PassAll,
    /// Claim every line with one stamped event (generic-like).
    ClaimAll,
    /// Open a block on the first line containing `trigger`; while the block
    /// is open, silently consume every further line on that stream
    /// (`Claimed(vec![])`, block-buffering-like); `finish` flushes one event.
    OpenBlock(&'static str),
}

/// Records every `feed` call (parser name, line) across all mock states that
/// share the recorder, in call order.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    fed: Arc<Mutex<Vec<(&'static str, String)>>>,
}

impl Recording {
    /// All feed calls so far, in order: (parser name, line).
    pub fn fed_calls(&self) -> Vec<(&'static str, String)> {
        self.fed.lock().expect("recording lock").clone()
    }

    fn record(&self, name: &'static str, line: &str) {
        self.fed
            .lock()
            .expect("recording lock")
            .push((name, line.to_string()));
    }
}

/// Fully configurable mock module: identity, inheritance, detect score and
/// claim behavior (task T6 deliverable f).
#[derive(Clone)]
pub struct MockParser {
    pub name: &'static str,
    pub extends: Option<&'static str>,
    pub score: u8,
    pub behavior: MockBehavior,
    recording: Recording,
}

impl MockParser {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            extends: None,
            score: 0,
            behavior: MockBehavior::PassAll,
            recording: Recording::default(),
        }
    }

    pub fn with_extends(mut self, parent: &'static str) -> Self {
        self.extends = Some(parent);
        self
    }

    pub fn with_score(mut self, score: u8) -> Self {
        self.score = score;
        self
    }

    pub fn with_behavior(mut self, behavior: MockBehavior) -> Self {
        self.behavior = behavior;
        self
    }

    pub fn recording(&self) -> Recording {
        self.recording.clone()
    }

    /// Constructor closure suitable for
    /// [`ParserRegistry::register`](loglense_parsers::ParserRegistry::register).
    pub fn ctor(self) -> impl Fn() -> Box<dyn Parser> + Send + Sync + 'static {
        move || Box::new(self.clone()) as Box<dyn Parser>
    }
}

impl Parser for MockParser {
    fn name(&self) -> &'static str {
        self.name
    }

    fn extends(&self) -> Option<&'static str> {
        self.extends
    }

    fn detect(&self, sample: &[&str]) -> u8 {
        if sample.is_empty() {
            0
        } else {
            self.score
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(MockState {
            parser: self.clone(),
            block_open: false,
            last_ctx: None,
        })
    }
}

struct MockState {
    parser: MockParser,
    block_open: bool,
    last_ctx: Option<LineCtx>,
}

impl MockState {
    fn stamped_event(&self, message: String, ctx: &LineCtx) -> Event {
        Event {
            stream: ctx.stream,
            message,
            lines: vec![ctx.line_no],
            ..Event::default()
        }
    }
}

impl ParseState for MockState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        self.parser.recording.record(self.parser.name, line);
        self.last_ctx = Some(*ctx);
        match self.parser.behavior {
            MockBehavior::PassAll => FeedOutcome::Passed,
            MockBehavior::ClaimAll => FeedOutcome::Claimed(vec![
                self.stamped_event(format!("{}: {}", self.parser.name, line), ctx)
            ]),
            MockBehavior::OpenBlock(trigger) => {
                if self.block_open || line.contains(trigger) {
                    self.block_open = true;
                    FeedOutcome::Claimed(Vec::new())
                } else {
                    FeedOutcome::Passed
                }
            }
        }
    }

    fn finish(&mut self) -> Vec<Event> {
        if !self.block_open {
            return Vec::new();
        }
        self.block_open = false;
        let stream = self.last_ctx.map_or(Stream::Out, |ctx| ctx.stream);
        vec![Event {
            stream,
            message: format!("{} flushed", self.parser.name),
            ..Event::default()
        }]
    }
}
