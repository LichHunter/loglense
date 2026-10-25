//! The `gotest` tool parser (docs/parsers.md §5.2), extends `go`.
//!
//! go test rules — `=== RUN` phase lines, `--- FAIL|PASS|SKIP:` test
//! outcomes (fail → error, pass → info, skip → notice), `FAIL [pkg …]` /
//! `PASS` / `ok  \tpkg…` summaries — layered over the embedded `go` base
//! state, so panics inside failing tests still collapse into one error
//! event with frames. While the base block is open it absorbs continuation
//! lines; the gotest rules claim the block-closing line itself
//! (java/maven precedent).

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, TestInfo};
use regex::Regex;

use crate::bases::go::GoState;
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

static RUN_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^=== RUN\s+(.+)$").unwrap());
static OUTCOME_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^--- (FAIL|PASS|SKIP): (.*?) \((\d+(?:\.\d+)?)s\)$").unwrap());
static FAIL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^FAIL(?:\s+\S+)?(?:\s+[\d.]+s)?$").unwrap());
static OK_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ok  \s*\S+.*$").unwrap());
static PASS_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^PASS$").unwrap());

/// The `gotest` module: go test rules layered over the `go` base grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct GotestParser;

/// Per-run state of [`GotestParser`]: the go test rules plus the embedded
/// go base state that owns the panic-block grammar.
pub struct GotestState {
    go: GoState,
    pending: Vec<Event>,
}

impl Parser for GotestParser {
    fn name(&self) -> &'static str {
        "gotest"
    }

    fn extends(&self) -> Option<&'static str> {
        Some("go")
    }

    /// go test markers score `min(95, 40 + 20·m + 15·go_strong)` — strictly
    /// above the go base's 90 cap even when the run embeds a panic, so go
    /// test output always picks gotest; pure panic output has no go test
    /// markers (0 here) and the base wins.
    fn detect(&self, sample: &[&str]) -> u8 {
        let markers = sample
            .iter()
            .filter(|line| {
                RUN_LINE.is_match(line)
                    || OUTCOME_LINE.is_match(line)
                    || FAIL_LINE.is_match(line)
                    || OK_LINE.is_match(line)
                    || PASS_LINE.is_match(line)
            })
            .count();
        if markers == 0 {
            return 0;
        }
        let go_strong = sample
            .iter()
            .filter(|line| line.starts_with("panic: ") || line.starts_with("goroutine "))
            .count();
        (40 + 20 * markers + 15 * go_strong).min(95) as u8
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(GotestState {
            go: GoState::default(),
            pending: Vec::new(),
        })
    }
}

impl ParseState for GotestState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let mut events = std::mem::take(&mut self.pending);
        let block_was_open = self.go.block_open();
        // Derived rules claim first — but never while the base's panic
        // block is absorbing its continuation lines.
        if !block_was_open {
            if let Some(event) = gotest_event(line, ctx) {
                events.push(event);
                return FeedOutcome::Claimed(events);
            }
        }
        match self.go.feed(line, ctx) {
            FeedOutcome::Claimed(mut batch) => {
                events.append(&mut batch);
                FeedOutcome::Claimed(events)
            }
            // The base declined — either idle, or it just closed a block on
            // this very line, in which case the closing line is still up
            // for the gotest rules.
            FeedOutcome::Passed => {
                events.extend(self.go.take_pending());
                if block_was_open {
                    if let Some(event) = gotest_event(line, ctx) {
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
        events.extend(self.go.finish());
        events
    }
}

/// The gotest-only event for `line`, or `None` when no go test rule matches.
fn gotest_event(line: &str, ctx: &LineCtx) -> Option<Event> {
    if let Some(caps) = RUN_LINE.captures(line) {
        return Some(Event {
            stream: ctx.stream,
            severity: Severity::Info,
            kind: Kind::Phase,
            message: line.to_string(),
            phase: Some(caps[1].to_string()),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    if let Some(caps) = OUTCOME_LINE.captures(line) {
        let (severity, outcome) = match &caps[1] {
            "FAIL" => (Severity::Error, "failed"),
            "SKIP" => (Severity::Notice, "skipped"),
            _ => (Severity::Info, "passed"),
        };
        let duration_ms = (caps[3].parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u64;
        return Some(Event {
            stream: ctx.stream,
            severity,
            kind: Kind::Test,
            message: line.to_string(),
            test: Some(TestInfo {
                name: caps[2].to_string(),
                outcome: outcome.to_string(),
                duration_ms: Some(duration_ms),
            }),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    let summary = |severity| Event {
        stream: ctx.stream,
        severity,
        kind: Kind::Summary,
        message: line.to_string(),
        lines: vec![ctx.line_no],
        ..Event::default()
    };
    if FAIL_LINE.is_match(line) {
        return Some(summary(Severity::Error));
    }
    if OK_LINE.is_match(line) || PASS_LINE.is_match(line) {
        return Some(summary(Severity::Info));
    }
    None
}
