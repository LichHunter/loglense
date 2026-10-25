//! The `generic` format base (docs/parsers.md §5.1): the always-last
//! fallback that claims every line no other module wanted. Severity comes
//! from keyword heuristics on the plain line; ANSI stripping happens
//! upstream in the runner, so this module sees plain text only. It is never
//! an autodetection candidate — [`GenericParser::detect`] is 0 by design and
//! the chain builder appends it last instead.

use loglense_core::{Event, Kind, Severity};

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// Anchored case-insensitive prefixes equivalent to
/// `^(ERROR|FATAL|PANIC|CRITICAL|SEVERE)\b` — a word boundary must follow.
const ERROR_PREFIXES: [&str; 5] = ["ERROR", "FATAL", "PANIC", "CRITICAL", "SEVERE"];

/// Anchored case-insensitive prefixes equivalent to `^(WARN|WARNING|CAUTION)\b`.
const WARNING_PREFIXES: [&str; 3] = ["WARN", "WARNING", "CAUTION"];

/// Case-insensitive substring keywords that mark a line as an error.
const ERROR_CONTAINS: [&str; 5] = [
    "error:",
    "exception",
    "traceback",
    "failed",
    "build failure",
];

/// Case-insensitive substring keywords that mark a line as a warning.
const WARNING_CONTAINS: [&str; 1] = ["warning:"];

/// The `generic` fallback module: stateless, claims every line.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericParser;

/// Per-run state of [`GenericParser`] — trivially empty; the type exists to
/// satisfy the one-state-per-stream contract.
pub struct GenericState;

impl Parser for GenericParser {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    fn detect(&self, _sample: &[&str]) -> u8 {
        // The permanent last resort: never competes in autodetection
        // (docs/parsers.md §3.3 item 4).
        0
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(GenericState)
    }
}

impl ParseState for GenericState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        FeedOutcome::Claimed(vec![Event {
            stream: ctx.stream,
            severity: classify(line),
            kind: Kind::Line,
            message: line.to_string(),
            lines: vec![ctx.line_no],
            ..Event::default()
        }])
    }

    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// Keyword severity for one plain line: error beats warning beats info.
fn classify(line: &str) -> Severity {
    let lowered = line.to_ascii_lowercase();
    if ERROR_PREFIXES.iter().any(|kw| has_keyword_prefix(line, kw))
        || ERROR_CONTAINS.iter().any(|kw| lowered.contains(kw))
    {
        Severity::Error
    } else if WARNING_PREFIXES
        .iter()
        .any(|kw| has_keyword_prefix(line, kw))
        || WARNING_CONTAINS.iter().any(|kw| lowered.contains(kw))
    {
        Severity::Warning
    } else {
        Severity::Info
    }
}

/// `line` starts with `keyword` (ASCII case-insensitive) followed by a word
/// boundary or end of line — the `^(KEYWORD)\b` anchored-prefix semantics
/// without compiling a regex. Pure byte/char inspection, so adversarial
/// input can never panic it.
fn has_keyword_prefix(line: &str, keyword: &str) -> bool {
    line.len() >= keyword.len()
        && line.is_char_boundary(keyword.len())
        && line[..keyword.len()].eq_ignore_ascii_case(keyword)
        && line[keyword.len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_')
}
