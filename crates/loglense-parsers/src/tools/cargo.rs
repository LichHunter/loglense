//! The `cargo` tool parser (docs/parsers.md §5.2), extends `rust`.
//!
//! Cargo-specific single-line rules — `Compiling` / `Finished` / `Running`
//! phase markers, `test result:` totals, `error: could not compile …` —
//! layered over the embedded `rust` base state, so rustc diagnostic and
//! panic blocks inside cargo output still collapse into one event each.
//! While the base block is open it absorbs continuation lines; the cargo
//! rules claim the block-closing line itself (java/maven precedent).

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity};
use regex::Regex;

use crate::bases::rust::RustState;
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

static COMPILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*Compiling (.+)$").unwrap());
static FINISHED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*Finished (.+)$").unwrap());
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*Running (.+)$").unwrap());
static TEST_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^test result: (ok|FAILED)\. (.*)$").unwrap());
static COULD_NOT_COMPILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error: could not compile .+$").unwrap());
static CARGO_VOCAB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:Compiling|Finished|Doc-tests|Downloaded) |^\s*Running |^test result: |^error: could not compile ")
        .unwrap()
});

/// The `cargo` module: cargo rules layered over the `rust` base grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoParser;

/// Per-run state of [`CargoParser`]: the cargo rules plus the embedded rust
/// base state that owns the diagnostic and panic block grammar.
pub struct CargoState {
    rust: RustState,
    pending: Vec<Event>,
}

impl Parser for CargoParser {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn extends(&self) -> Option<&'static str> {
        Some("rust")
    }

    /// Cargo vocabulary (`Compiling`/`Finished`/`Running`/`test result:`/
    /// `could not compile`…): one line is weak evidence (25); two or more
    /// score `min(90, 50 + 20·n)` — strictly above the rust base's 85 cap
    /// so cargo output picks cargo, while bare rustc output (no cargo
    /// words) scores 0 here and the base wins.
    fn detect(&self, sample: &[&str]) -> u8 {
        let hits = sample
            .iter()
            .filter(|line| CARGO_VOCAB.is_match(line))
            .count();
        match hits {
            0 => 0,
            1 => 25,
            n => (50 + 20 * n).min(90) as u8,
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(CargoState {
            rust: RustState::default(),
            pending: Vec::new(),
        })
    }
}

impl ParseState for CargoState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let mut events = std::mem::take(&mut self.pending);
        let block_was_open = self.rust.block_open();
        // Derived rules claim first — but never while the base's block is
        // absorbing its continuation lines.
        if !block_was_open {
            if let Some(event) = cargo_event(line, ctx) {
                events.push(event);
                return FeedOutcome::Claimed(events);
            }
        }
        match self.rust.feed(line, ctx) {
            FeedOutcome::Claimed(mut batch) => {
                events.append(&mut batch);
                FeedOutcome::Claimed(events)
            }
            // The base declined — either idle, or it just closed a block on
            // this very line, in which case the closing line is still up
            // for the cargo rules.
            FeedOutcome::Passed => {
                events.extend(self.rust.take_pending());
                if block_was_open {
                    if let Some(event) = cargo_event(line, ctx) {
                        events.push(event);
                        return FeedOutcome::Claimed(events);
                    }
                }
                if events.is_empty() {
                    return FeedOutcome::Passed;
                }
                self.pending = events;
                FeedOutcome::Passed
            }
        }
    }

    fn finish(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(self.rust.finish());
        events
    }
}

/// The cargo-only event for `line`, or `None` when no cargo rule matches.
fn cargo_event(line: &str, ctx: &LineCtx) -> Option<Event> {
    let phase_event = |phase: String| Event {
        stream: ctx.stream,
        severity: Severity::Info,
        kind: Kind::Phase,
        message: line.to_string(),
        phase: Some(phase),
        lines: vec![ctx.line_no],
        ..Event::default()
    };
    if let Some(caps) = COMPILING.captures(line) {
        return Some(phase_event(caps[1].to_string()));
    }
    if FINISHED.is_match(line) {
        return Some(phase_event("finished".to_string()));
    }
    if let Some(caps) = RUNNING.captures(line) {
        return Some(phase_event(caps[1].to_string()));
    }
    if TEST_RESULT.is_match(line) {
        let failed = TEST_RESULT
            .captures(line)
            .is_some_and(|caps| &caps[1] == "FAILED");
        return Some(Event {
            stream: ctx.stream,
            severity: if failed {
                Severity::Error
            } else {
                Severity::Info
            },
            kind: Kind::Summary,
            message: line.to_string(),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    if COULD_NOT_COMPILE.is_match(line) {
        return Some(Event {
            stream: ctx.stream,
            severity: Severity::Error,
            kind: Kind::Summary,
            message: line.to_string(),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    None
}
