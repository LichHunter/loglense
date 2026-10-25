//! The `python` lang base (docs/parsers.md §5.1): the stateful
//! traceback-block grammar every python tool parser inherits, plus the
//! `logging` default single-line formats.
//!
//! Block grammar: a `Traceback (most recent call last):` header line,
//! `File "path", line N, in func` frame lines, indented source-snippet
//! continuations, and a flush-left closer (`…Error`/`…Exception`/
//! `…Warning`, optionally `: message`). The whole block collapses into ONE
//! error event carrying `frames` (`file:line in func`) and `lines`
//! (docs/parsers.md §1.4 one-event-per-block). While a block is open the
//! state claims continuation lines silently; a non-continuation line
//! closes the block and stays unconsumed (`Passed`) so later chain members
//! see it — the completed event rides along with the next claim or the
//! end-of-stream flush. A traceback header arriving while a block is open
//! flushes the unclosed block and opens the new one in place, so
//! back-to-back tracebacks never merge.
//!
//! Logging formats (claimed only while no block is open): the bare
//! `LEVEL:name:message` default and its `asctime`-prefixed variant, each
//! mapped to one severity line event carrying the message text.

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Stream};
use regex::Regex;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// Traceback block header (exact line — the phrase inside prose is not a
/// header).
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Traceback \(most recent call last\):$").expect("static pattern compiles")
});

/// Stack frame: indented `File "path", line N, in func`.
static FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s+File "([^"]+)", line (\d+), in (.+)$"#).expect("static pattern compiles")
});

/// Exception closer: flush-left `pkg.Exc`/`pkg.Warning`, optional
/// `: message`. Consulted only while a traceback block is open, so prose
/// like `SyntaxError` never claims anything on its own.
static CLOSER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([\w.]+(?:Error|Exception|Warning))(?::\s+(.*))?$")
        .expect("static pattern compiles")
});

/// `logging` default format: `LEVEL:name:message` (levels are the five
/// literal upper-case names the logging module prints).
static LOGGING_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(DEBUG|INFO|WARNING|ERROR|CRITICAL):([\w.]+):(.*)$")
        .expect("static pattern compiles")
});

/// `asctime`-prefixed `logging` variant:
/// `2026-10-25 10:00:00,123 LEVEL:name:message`.
static LOGGING_ASCTIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[,.]\d{1,6})?\s+(DEBUG|INFO|WARNING|ERROR|CRITICAL):([\w.]+):(.*)$")
        .expect("static pattern compiles")
});

/// The `python` module: stateful, claims traceback blocks and logging
/// lines.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonParser;

/// One in-flight traceback block.
struct Block {
    /// Stream the block started on (states are per-stream; recorded once).
    stream: Stream,
    /// Closer exception type and message, set when the closer arrives; a
    /// block flushed without one renders an empty message.
    exc: Option<(String, Option<String>)>,
    /// Frames rendered `file:line in func`.
    frames: Vec<String>,
    /// Raw line numbers of every consumed line.
    lines: Vec<u64>,
}

impl Block {
    fn new(ctx: &LineCtx) -> Self {
        Self {
            stream: ctx.stream,
            exc: None,
            frames: Vec::new(),
            lines: vec![ctx.line_no],
        }
    }

    /// Collapse into the single error event for the whole block.
    fn into_event(self) -> Event {
        let message = match &self.exc {
            Some((exc, Some(msg))) => format!("{exc}: {msg}"),
            Some((exc, None)) => exc.clone(),
            None => String::new(),
        };
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

/// Per-run state of [`PythonParser`]: the open block plus events completed
/// by a block-closing line that were not yet deliverable (a `Passed`
/// return carries no events; the stash ships with the next claim or
/// `finish`).
#[derive(Default)]
pub struct PythonState {
    block: Option<Block>,
    pending: Vec<Event>,
}

impl PythonState {
    /// Whether a traceback block is currently absorbing continuation
    /// lines. Tool parsers that compose this state consult it to route the
    /// closing line through their own rules first.
    pub(crate) fn block_open(&self) -> bool {
        self.block.is_some()
    }

    /// Events completed by a `Passed` block close, handed to the composer
    /// (the pytest tool) so they ship with its next claim.
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

impl Parser for PythonParser {
    fn name(&self) -> &'static str {
        "python"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    /// 80 for any traceback header or ≥2 logging-format lines. Below the
    /// pytest tool's 90 (its markers outrank embedded tracebacks), above
    /// the java base's 70 (a python traceback's `ValueError:` closer
    /// matches the java exception-header grammar, but the traceback header
    /// is python-specific).
    fn detect(&self, sample: &[&str]) -> u8 {
        if sample.iter().any(|line| HEADER.is_match(line)) {
            return 80;
        }
        let logging = sample
            .iter()
            .filter(|line| parse_logging(line).is_some())
            .count();
        if logging >= 2 {
            80
        } else {
            0
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(PythonState::default())
    }
}

impl ParseState for PythonState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        if let Some(mut block) = self.block.take() {
            if HEADER.is_match(line) {
                // A fresh traceback starts before the old one closed:
                // flush the open block (no closer seen) and open the new
                // one in its place.
                let flushed = block.into_event();
                self.block = Some(Block::new(ctx));
                return self.claimed(vec![flushed]);
            }
            if let Some(caps) = FRAME.captures(line) {
                block
                    .frames
                    .push(format!("{}:{} in {}", &caps[1], &caps[2], &caps[3]));
                block.lines.push(ctx.line_no);
                self.block = Some(block);
                return self.claimed(Vec::new());
            }
            if is_indented(line) {
                // Source-snippet continuation (`    do_thing()`).
                block.lines.push(ctx.line_no);
                self.block = Some(block);
                return self.claimed(Vec::new());
            }
            if let Some(caps) = CLOSER.captures(line) {
                block.exc = Some((
                    caps[1].to_string(),
                    caps.get(2).map(|m| m.as_str().to_string()),
                ));
                block.lines.push(ctx.line_no);
                return self.claimed(vec![block.into_event()]);
            }
            // Non-continuation: close the block, leave the line unconsumed
            // for the next chain member.
            self.pending.push(block.into_event());
            return FeedOutcome::Passed;
        }
        if HEADER.is_match(line) {
            self.block = Some(Block::new(ctx));
            return self.claimed(Vec::new());
        }
        if let Some((severity, message)) = parse_logging(line) {
            return self.claimed(vec![Event {
                stream: ctx.stream,
                severity,
                kind: Kind::Line,
                message,
                lines: vec![ctx.line_no],
                ..Event::default()
            }]);
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

/// Non-blank line starting with whitespace — the shape of traceback source
/// snippets and every other continuation inside a block.
fn is_indented(line: &str) -> bool {
    line.chars().next().is_some_and(char::is_whitespace) && !line.trim().is_empty()
}

/// Parse a `logging`-format line (bare or asctime-prefixed) into its
/// severity and message text. `name` is dropped: the event carries only
/// the message.
fn parse_logging(line: &str) -> Option<(Severity, String)> {
    let caps = LOGGING_BARE
        .captures(line)
        .or_else(|| LOGGING_ASCTIME.captures(line))?;
    Some((level_severity(&caps[1]), caps[3].to_string()))
}

/// The five literal `logging` level names → severity. CRITICAL is an
/// error; the unreachable arm is grammar-guaranteed by the regexes.
fn level_severity(level: &str) -> Severity {
    match level {
        "DEBUG" => Severity::Debug,
        "INFO" => Severity::Info,
        "WARNING" => Severity::Warning,
        "ERROR" | "CRITICAL" => Severity::Error,
        _ => Severity::Info,
    }
}
