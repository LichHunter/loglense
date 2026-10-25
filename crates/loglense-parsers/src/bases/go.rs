//! The `go` lang base (docs/parsers.md §5.1): runtime panic blocks and the
//! standard library `log` line format.
//!
//! Panic block: `panic: …` opens, `goroutine N [state]:` headers and
//! function-signature lines continue silently, `file.go:N [+0x…]` lines
//! collect as `frames` entries `file:line`, indented lines absorb. A blank
//! line before any frame (the `go run` gap) absorbs; after frames it closes.
//! Any other non-continuation line closes the block and stays unconsumed
//! (`Passed`, closed event stashed — java-base precedent) so the gotest
//! tool or `generic` can claim it. One panic block is ONE error event.
//!
//! Log lines: `2009/01/23 01:23:23[.ddd] file.go:123: message` (the
//! documented `log.Print` default; that timestamp is the canonical example
//! from the Go log package docs) map to info events with
//! [`EventSource`] set.

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Source as EventSource, Stream};
use regex::Regex;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

static PANIC_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^panic: (.+)$").unwrap());
static GOROUTINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^goroutine \d+ \[[^\]]*\]:$").unwrap());
static FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\S+\.go):(\d+)(?: \+0x[0-9a-fA-F]+)?$").unwrap());
/// Frame function lines: `pkg.Func(args)`, `pkg.(*T).Method(...)`, or
/// `created by pkg.Func in goroutine N`. The callee never contains a space
/// before its argument list — that keeps `--- FAIL: TestX (0.00s)` out.
static FUNC_SIG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\S+\(.*\)|created by \S+(?: in goroutine \d+)?)$").unwrap());
static LOG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?) (\S+\.go):(\d+): (.*)$").unwrap()
});

/// The open panic block being accumulated into one error event.
#[derive(Default)]
struct Block {
    message: String,
    frames: Vec<String>,
    lines: Vec<u64>,
    stream: Stream,
}

impl Block {
    fn into_event(self) -> Event {
        Event {
            stream: self.stream,
            severity: Severity::Error,
            kind: Kind::Line,
            message: self.message,
            lines: self.lines,
            frames: self.frames,
            ..Event::default()
        }
    }
}

/// Per-run state of [`GoParser`]: the open block plus events completed by a
/// block-closing line that stayed unconsumed (java-base precedent).
#[derive(Default)]
pub struct GoState {
    block: Option<Block>,
    pending: Vec<Event>,
}

impl GoState {
    /// Whether a panic block is absorbing continuation lines. The gotest
    /// tool consults this to route the closing line through its own rules.
    pub(crate) fn block_open(&self) -> bool {
        self.block.is_some()
    }

    /// Events completed by a `Passed` block close, handed to the composer
    /// (the gotest tool) so they ship with its next claim.
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

/// The `go` lang base module: stateful panic-block grammar, stateless log
/// line mapping.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoParser;

impl Parser for GoParser {
    fn name(&self) -> &'static str {
        "go"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    /// Panic and goroutine headers are strong markers: `min(90, 35 + 30·n)`.
    /// Without them, ≥2 `log.Print`-format lines score 40 (enough to beat
    /// `generic`, never a tool). Gotest out-scores this at 95 on go test
    /// output; pure panic output has no gotest markers, so this base wins.
    fn detect(&self, sample: &[&str]) -> u8 {
        let strong = sample
            .iter()
            .filter(|line| PANIC_HEADER.is_match(line) || GOROUTINE.is_match(line))
            .count();
        if strong > 0 {
            return (35 + 30 * strong).min(90) as u8;
        }
        let logged = sample.iter().filter(|line| LOG_LINE.is_match(line)).count();
        if logged >= 2 {
            40
        } else {
            0
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(GoState::default())
    }
}

impl ParseState for GoState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let closed = match self.block.take() {
            None => None,
            Some(mut block) => {
                if absorb(&mut block, line, ctx) {
                    self.block = Some(block);
                    return self.claimed(Vec::new());
                }
                Some(block.into_event())
            }
        };
        if let Some(caps) = PANIC_HEADER.captures(line) {
            self.block = Some(Block {
                message: caps[1].to_string(),
                lines: vec![ctx.line_no],
                stream: ctx.stream,
                ..Block::default()
            });
            return self.claimed(closed.into_iter().collect());
        }
        if let Some(caps) = LOG_LINE.captures(line) {
            let event = Event {
                stream: ctx.stream,
                severity: Severity::Info,
                kind: Kind::Line,
                message: caps[4].to_string(),
                source: Some(EventSource {
                    file: caps[2].to_string(),
                    line: caps[3].parse().unwrap_or(0),
                    module: None,
                }),
                lines: vec![ctx.line_no],
                ..Event::default()
            };
            return self.claimed(closed.into_iter().chain([event]).collect());
        }
        self.pending.extend(closed);
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

/// Continuation absorption for the open panic block.
fn absorb(block: &mut Block, line: &str, ctx: &LineCtx) -> bool {
    if GOROUTINE.is_match(line) || FUNC_SIG.is_match(line) {
        block.lines.push(ctx.line_no);
        return true;
    }
    if let Some(caps) = FRAME.captures(line) {
        block.frames.push(format!("{}:{}", &caps[1], &caps[2]));
        block.lines.push(ctx.line_no);
        return true;
    }
    if line.is_empty() {
        // The `go run` gap between `panic:` and `goroutine:` absorbs; after
        // frames it terminates the block.
        if block.frames.is_empty() {
            block.lines.push(ctx.line_no);
            return true;
        }
        return false;
    }
    if line.starts_with([' ', '\t']) {
        block.lines.push(ctx.line_no);
        return true;
    }
    false
}
