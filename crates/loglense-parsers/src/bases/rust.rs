//! The `rust` lang base (docs/parsers.md §5.1): rustc diagnostic grammar.
//!
//! Block grammar (fixture-tested, real rustc 1.99 output): `error[E0xxx]: …`
//! / `error: …` / `warning: …` headers open a diagnostic block; `--> path:
//! line:col` sets [`EventSource`]; code-context (`LL | …`), caret (`  | …`)
//! and `= note:` / `help:` lines continue it. `note:`/`help:`/`= note` lines
//! attach as `frames` (trimmed); code and caret lines are covered by `lines`
//! only. `error: aborting due to N previous errors` is a standalone
//! `kind=summary` event. Panic blocks: `thread '<name>' panicked at file:
//! line:col:` opens one; the first plain line becomes the message (fallback
//! `panicked at file:line`), `note:` lines and further plain lines attach as
//! frames; a blank line closes the block.
//!
//! A block closes on any non-continuation line. If that line is not this
//! module's own grammar, it stays unconsumed (`Passed`, the closed event
//! stashed in `pending`) so later chain members — the cargo tool, `generic` —
//! can claim it (java-base precedent).

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Source as EventSource, Stream};
use regex::Regex;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

static ERROR_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error(?:\[E\d+\])?: (.+)$").unwrap());
static WARNING_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^warning(?:: (.*))?$").unwrap());
static ABORT_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^error: aborting due to \d+ previous errors?(?:; \d+ warnings? emitted)?$")
        .unwrap()
});
static PANIC_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^thread '([^']+)'(?: \(\d+\))? panicked at ([^:\s]+):(\d+):\d+:?$").unwrap()
});
static ARROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*-->\s(\S+):(\d+):\d+\s*$").unwrap());
static CODE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\d+\s*\|").unwrap());
static PIPE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\|").unwrap());
static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*=\s").unwrap());
static NOTE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(?:note|help):").unwrap());

/// The open multi-line block being accumulated into one event.
#[derive(Default)]
struct Block {
    severity: Option<Severity>,
    message: String,
    source: Option<(String, u64)>,
    panic_location: Option<(String, u64)>,
    notes: Vec<String>,
    lines: Vec<u64>,
    stream: Stream,
}

impl Block {
    fn into_event(self) -> Event {
        let (message, source) = match self.panic_location {
            Some((file, line)) => (
                if self.message.is_empty() {
                    format!("panicked at {file}:{line}")
                } else {
                    self.message
                },
                Some(EventSource {
                    file,
                    line,
                    module: None,
                }),
            ),
            None => (
                self.message,
                self.source.map(|(file, line)| EventSource {
                    file,
                    line,
                    module: None,
                }),
            ),
        };
        Event {
            stream: self.stream,
            severity: self.severity.unwrap_or(Severity::Error),
            kind: Kind::Line,
            message,
            source,
            lines: self.lines,
            frames: self.notes,
            ..Event::default()
        }
    }
}

/// Per-run state of [`RustParser`]: the open block plus events completed by
/// a block-closing line that stayed unconsumed (`Passed` carries no events;
/// the stash ships with the next claim or `finish` — java-base precedent).
#[derive(Default)]
pub struct RustState {
    block: Option<Block>,
    pending: Vec<Event>,
}

impl RustState {
    /// Whether a diagnostic or panic block is absorbing continuation lines.
    /// Tool parsers that compose this state (cargo) consult it to route the
    /// closing line through their own rules first.
    pub(crate) fn block_open(&self) -> bool {
        self.block.is_some()
    }

    /// Events completed by a `Passed` block close, handed to the composer
    /// (the cargo tool) so they ship with its next claim.
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

/// The `rust` lang base module: stateful rustc diagnostic and panic grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustParser;

impl Parser for RustParser {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    /// Strong rustc markers (`-->` refs, `error[E0xxx]:` headers, aborting
    /// summaries, panic headers): `min(85, 25 + 20·n)` when any, else 0.
    /// Capped below the cargo tool's 90-on-vocabulary so cargo output picks
    /// cargo (docs/parsers.md §5.2), while bare rustc output (cargo scores
    /// 0) still picks this base.
    fn detect(&self, sample: &[&str]) -> u8 {
        let strong = sample
            .iter()
            .filter(|line| {
                ARROW.is_match(line)
                    || line.starts_with("error[E")
                    || ABORT_SUMMARY.is_match(line)
                    || PANIC_HEADER.is_match(line)
            })
            .count();
        if strong == 0 {
            0
        } else {
            (25 + 20 * strong).min(85) as u8
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(RustState::default())
    }
}

impl ParseState for RustState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let closed = match self.block.take() {
            None => None,
            Some(mut block) => {
                if self.absorb(&mut block, line, ctx) {
                    self.block = Some(block);
                    return self.claimed(Vec::new());
                }
                Some(block.into_event())
            }
        };
        let mut batch: Vec<Event> = closed.into_iter().collect();
        if let Some(caps) = ABORT_SUMMARY.captures(line) {
            batch.push(Event {
                stream: ctx.stream,
                severity: Severity::Error,
                kind: Kind::Summary,
                message: caps[0].trim_start_matches("error: ").to_string(),
                lines: vec![ctx.line_no],
                ..Event::default()
            });
            return self.claimed(batch);
        }
        if let Some(caps) = ERROR_HEADER.captures(line) {
            self.block = Some(Block {
                severity: Some(Severity::Error),
                message: caps[1].to_string(),
                lines: vec![ctx.line_no],
                stream: ctx.stream,
                ..Block::default()
            });
            return self.claimed(batch);
        }
        if let Some(caps) = WARNING_HEADER.captures(line) {
            self.block = Some(Block {
                severity: Some(Severity::Warning),
                message: caps
                    .get(1)
                    .map_or(String::new(), |m| m.as_str().to_string()),
                lines: vec![ctx.line_no],
                stream: ctx.stream,
                ..Block::default()
            });
            return self.claimed(batch);
        }
        if let Some(caps) = PANIC_HEADER.captures(line) {
            self.block = Some(Block {
                severity: Some(Severity::Error),
                panic_location: Some((caps[2].to_string(), caps[3].parse().unwrap_or(0))),
                lines: vec![ctx.line_no],
                stream: ctx.stream,
                ..Block::default()
            });
            return self.claimed(batch);
        }
        self.pending.extend(batch);
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

impl RustState {
    /// Continuation absorption for the open block. Diagnostic blocks take
    /// `-->` refs, `LL |` code lines, `|` caret lines, `= …` directives and
    /// `note:`/`help:` lines; panic blocks take everything up to a blank
    /// line (first plain line fills the message, the rest become frames).
    fn absorb(&self, block: &mut Block, line: &str, ctx: &LineCtx) -> bool {
        if block.panic_location.is_some() {
            if line.is_empty() {
                return false;
            }
            if NOTE_LINE.is_match(line) || !block.message.is_empty() {
                block.notes.push(line.trim().to_string());
            } else {
                block.message = line.to_string();
            }
            block.lines.push(ctx.line_no);
            return true;
        }
        if let Some(caps) = ARROW.captures(line) {
            block.source = Some((caps[1].to_string(), caps[2].parse().unwrap_or(0)));
        } else if CODE_LINE.is_match(line) || PIPE_LINE.is_match(line) {
            // Visual context: covered by `lines` only.
        } else if DIRECTIVE.is_match(line) || NOTE_LINE.is_match(line) {
            block.notes.push(line.trim().to_string());
        } else {
            return false;
        }
        block.lines.push(ctx.line_no);
        true
    }
}
