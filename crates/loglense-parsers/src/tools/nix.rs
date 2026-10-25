//! The `nix` standalone tool parser (docs/parsers.md §5.2): nix builder
//! `error:` / `warning:` blocks, `… from` hint chains and `trace:` lines.
//!
//! `error: …` opens an error block (message = the text after the colon; a
//! bare `error:` — the modern nested-error header — falls back to the
//! message `error`), `warning: …` opens a warning block. Indented lines and
//! `…`-prefixed ellipsis hints (`… while …`, `… from https://…`) continue
//! the block and attach as `frames` (trimmed); blank lines absorb silently
//! (nix separates hint paragraphs with them) without being recorded in
//! `lines`. `at /nix/store/…:L:C` locations stay frames — a block can carry
//! several. Any other unindented line closes the block and stays
//! unconsumed so `generic` can claim it. `trace: …` lines are immediate
//! info events.
//!
//! Real-format provenance: Nix 2.34 builder failures and eval errors
//! (task-10 fixtures; see `.omo/evidence/task-10-loglense.txt`).

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Stream};
use regex::Regex;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

static ERROR_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^error: ?(.*)$").unwrap());
static WARNING_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^warning: ?(.*)$").unwrap());
static TRACE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^trace: ").unwrap());

/// The open error or warning block being accumulated into one event.
#[derive(Default)]
struct Block {
    severity: Severity,
    message: String,
    frames: Vec<String>,
    lines: Vec<u64>,
    stream: Stream,
}

impl Block {
    fn into_event(self) -> Event {
        Event {
            stream: self.stream,
            severity: self.severity,
            kind: Kind::Line,
            message: self.message,
            lines: self.lines,
            frames: self.frames,
            ..Event::default()
        }
    }
}

/// Per-run state of [`NixParser`]: the open block plus events completed by
/// a block-closing line that stayed unconsumed (java-base precedent).
#[derive(Default)]
pub struct NixState {
    block: Option<Block>,
    pending: Vec<Event>,
}

/// The `nix` standalone module: stateful error/warning block grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct NixParser;

impl Parser for NixParser {
    fn name(&self) -> &'static str {
        "nix"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    /// Requires BOTH an `error:`/`warning:` line and nix vocabulary
    /// (`derivation`, `builder`, `/nix/store`, case-insensitive) so plain
    /// `error: x` prose never picks nix. Scores `min(90, 40 + 20·vocab)`.
    fn detect(&self, sample: &[&str]) -> u8 {
        let flagged = sample
            .iter()
            .any(|line| ERROR_HEADER.is_match(line) || WARNING_HEADER.is_match(line));
        if !flagged {
            return 0;
        }
        let vocab = sample
            .iter()
            .filter(|line| {
                let lowered = line.to_ascii_lowercase();
                lowered.contains("derivation")
                    || lowered.contains("builder")
                    || lowered.contains("/nix/store")
            })
            .count();
        if vocab == 0 {
            0
        } else {
            (40 + 20 * vocab).min(90) as u8
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(NixState::default())
    }
}

impl ParseState for NixState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let closed = match self.block.take() {
            None => None,
            Some(mut block) => {
                if absorb(&mut block, line, ctx) {
                    self.block = Some(block);
                    return deliver(&mut self.pending, Vec::new());
                }
                Some(block.into_event())
            }
        };
        let mut batch: Vec<Event> = closed.into_iter().collect();
        if let Some(caps) = ERROR_HEADER.captures(line) {
            self.block = Some(open_block(Severity::Error, &caps[1], ctx));
            return deliver(&mut self.pending, batch);
        }
        if let Some(caps) = WARNING_HEADER.captures(line) {
            self.block = Some(open_block(Severity::Warning, &caps[1], ctx));
            return deliver(&mut self.pending, batch);
        }
        if TRACE_LINE.is_match(line) {
            batch.push(Event {
                stream: ctx.stream,
                severity: Severity::Info,
                kind: Kind::Line,
                message: line.to_string(),
                lines: vec![ctx.line_no],
                ..Event::default()
            });
            return deliver(&mut self.pending, batch);
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

/// Deliver `pending` plus `batch` as one claimed outcome.
fn deliver(pending: &mut Vec<Event>, batch: Vec<Event>) -> FeedOutcome {
    let mut events = std::mem::take(pending);
    events.extend(batch);
    FeedOutcome::Claimed(events)
}

/// Open a block for a header capture; an empty capture (the bare modern
/// `error:` nested-error header) falls back to the literal keyword.
fn open_block(severity: Severity, message: &str, ctx: &LineCtx) -> Block {
    Block {
        severity,
        message: if message.is_empty() {
            match severity {
                Severity::Error => "error".to_string(),
                _ => "warning".to_string(),
            }
        } else {
            message.to_string()
        },
        lines: vec![ctx.line_no],
        stream: ctx.stream,
        ..Block::default()
    }
}

/// Continuation absorption: indented lines and `…` hints attach as frames,
/// blank lines absorb silently (not recorded in `lines`).
fn absorb(block: &mut Block, line: &str, ctx: &LineCtx) -> bool {
    if line.is_empty() {
        return true;
    }
    if line.starts_with([' ', '\t']) || line.starts_with('…') {
        block.frames.push(line.trim().to_string());
        block.lines.push(ctx.line_no);
        return true;
    }
    false
}
