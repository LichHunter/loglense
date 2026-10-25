//! The `java` lang base (docs/parsers.md §5.1): the stateful JVM
//! exception-block grammar every JVM tool parser inherits, plus the
//! `[ERROR]`/`[WARNING]`/`[INFO]`/`[DEBUG]` bracket prefixes shared across
//! JVM tools.
//!
//! Block grammar: an exception header line (`java.lang.NullPointerException`
//! or `WidgetError`, optionally `: message`), `at pkg.Class(File.java:N)`
//! frame lines, `Caused by:` continuations, and a `... N more` terminator.
//! The whole block collapses into ONE error event carrying `frames` and
//! `lines` (docs/parsers.md §1.4 one-event-per-block). While a block is open
//! the state claims continuation lines silently; the first non-continuation
//! line closes the block and stays unconsumed (`Passed`) so later chain
//! members see it — the completed event rides along with the next claim or
//! the end-of-stream flush.

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Stream};
use regex::Regex;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// Exception block header: `pkg.Exc`/`pkg.Error`/`pkg.Throwable`, optional
/// `: message` (docs/parsers.md §5.1 java row).
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([\w$.]+(?:Exception|Error|Throwable))(?::\s+(.*))?$")
        .expect("static pattern compiles")
});

/// `Caused by:` continuation: same exception shape as the header.
static CAUSED_BY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Caused by: ([\w$.]+(?:Exception|Error|Throwable))(?::\s+(.*))?$")
        .expect("static pattern compiles")
});

/// Stack frame: indented `at pkg.Class(File.java:N)`.
static FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+at [\w$.]+\([^)]+\.java:\d+\)$").expect("static pattern compiles")
});

/// `... N more` common-frame terminator: closes the block (consumed).
static TERMINATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+\.\.\. \d+ more$").expect("static pattern compiles"));

/// Bracket prefix shared across JVM tools: `[ERROR] message`.
pub(crate) static BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(ERROR|WARNING|INFO|DEBUG)\]\s*(.*)$").expect("static pattern compiles")
});

/// The `java` module: stateful, claims exception blocks and bracket lines.
#[derive(Debug, Default, Clone, Copy)]
pub struct JavaParser;

/// One in-flight exception block.
struct Block {
    /// Stream the block started on (states are per-stream; recorded once).
    stream: Stream,
    /// Header exception type, e.g. `java.lang.NullPointerException`.
    exc: String,
    /// Header message, when present.
    msg: Option<String>,
    /// Most recent `Caused by:` chain link, rendered `exc: msg`.
    caused_by: Option<String>,
    /// Trimmed frame lines.
    frames: Vec<String>,
    /// Raw line numbers of every consumed line.
    lines: Vec<u64>,
}

impl Block {
    /// Collapse into the single error event for the whole block.
    fn into_event(self) -> Event {
        let mut message = match &self.msg {
            Some(msg) => format!("{}: {}", self.exc, msg),
            None => self.exc.clone(),
        };
        if let Some(cause) = &self.caused_by {
            message.push_str(" ... caused by ");
            message.push_str(cause);
        }
        Event {
            stream: self.stream,
            severity: Severity::Error,
            kind: Kind::Line,
            message,
            lines: self.lines,
            frames: self.frames,
            ..Event::default()
        }
    }
}

/// Per-run state of [`JavaParser`]: the open block plus events completed by
/// a block-closing line that were not yet deliverable (a `Passed` return
/// carries no events; the stash ships with the next claim or `finish`).
#[derive(Default)]
pub struct JavaState {
    block: Option<Block>,
    pending: Vec<Event>,
}

impl JavaState {
    /// Whether an exception block is currently absorbing continuation
    /// lines. Tool parsers that compose this state consult it to route the
    /// closing line through their own rules first.
    pub(crate) fn block_open(&self) -> bool {
        self.block.is_some()
    }

    /// Events completed by a `Passed` block close, handed to the composer
    /// (the maven tool) so they ship with its next claim.
    pub(crate) fn take_pending(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.pending)
    }

    /// Deliver `pending` plus `batch` as one claimed outcome.
    fn claimed(&mut self, batch: Vec<Event>) -> FeedOutcome {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(batch);
        FeedOutcome::Claimed(events)
    }
}

impl Parser for JavaParser {
    fn name(&self) -> &'static str {
        "java"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    /// Header evidence outweighs bracket evidence: 70 for any exception
    /// header, else 60 for ≥2 bracket lines, else 0. Tool parsers over
    /// JVM vocabulary (maven: 90) must out-score both.
    fn detect(&self, sample: &[&str]) -> u8 {
        if sample.iter().any(|line| HEADER.is_match(line)) {
            return 70;
        }
        let brackets = sample.iter().filter(|line| BRACKET.is_match(line)).count();
        if brackets >= 2 {
            60
        } else {
            0
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(JavaState::default())
    }
}

impl ParseState for JavaState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        if let Some(mut block) = self.block.take() {
            if FRAME.is_match(line) {
                block.frames.push(line.trim().to_string());
                block.lines.push(ctx.line_no);
                self.block = Some(block);
                return self.claimed(Vec::new());
            }
            if let Some(caps) = CAUSED_BY.captures(line) {
                block.caused_by = Some(render_exception(&caps[1], caps.get(2).map(|m| m.as_str())));
                block.lines.push(ctx.line_no);
                self.block = Some(block);
                return self.claimed(Vec::new());
            }
            if TERMINATOR.is_match(line) {
                block.lines.push(ctx.line_no);
                return self.claimed(vec![block.into_event()]);
            }
            // Non-continuation: close the block, leave the line unconsumed
            // for the next chain member.
            self.pending.push(block.into_event());
            return FeedOutcome::Passed;
        }
        if let Some(caps) = HEADER.captures(line) {
            self.block = Some(Block {
                stream: ctx.stream,
                exc: caps[1].to_string(),
                msg: caps.get(2).map(|m| m.as_str().to_string()),
                caused_by: None,
                frames: Vec::new(),
                lines: vec![ctx.line_no],
            });
            return self.claimed(Vec::new());
        }
        if let Some(event) = bracket_event(line, ctx) {
            return self.claimed(vec![event]);
        }
        FeedOutcome::Passed
    }

    fn finish(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.pending);
        if let Some(block) = self.block.take() {
            events.push(block.into_event());
        }
        events
    }
}

/// `Exc: msg` (or bare `Exc`) for headers and `Caused by:` links.
fn render_exception(exc: &str, msg: Option<&str>) -> String {
    match msg {
        Some(msg) => format!("{exc}: {msg}"),
        None => exc.to_string(),
    }
}

/// Bracket-prefix line event (`[ERROR] message` → severity line). Shared
/// with the maven tool, whose own rules evaluate first in its chain.
pub(crate) fn bracket_event(line: &str, ctx: &LineCtx) -> Option<Event> {
    let caps = BRACKET.captures(line)?;
    Some(Event {
        stream: ctx.stream,
        severity: super::map_level(&caps[1]),
        kind: Kind::Line,
        message: caps[2].to_string(),
        lines: vec![ctx.line_no],
        ..Event::default()
    })
}
