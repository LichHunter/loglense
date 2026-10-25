//! The `logfmt` format base (docs/parsers.md §5.1): whole-line `key=value`
//! token lines (at least two tokens; values may be double-quoted, quotes
//! then holding spaces). Severity from `level=`, message from `msg=`, both
//! mapped like the json base's keys; anything else about the line not
//! matching the token grammar is passed to the later chain members.

use loglense_core::{Event, Kind, Severity};

use super::{map_level, percentage};
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// The `logfmt` module: stateless, claims whole-line key=value logs only.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogfmtParser;

/// Per-run state of [`LogfmtParser`] — empty; the type satisfies the
/// one-state-per-stream contract.
pub struct LogfmtState;

impl Parser for LogfmtParser {
    fn name(&self) -> &'static str {
        "logfmt"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    fn detect(&self, sample: &[&str]) -> u8 {
        percentage(sample, |line| parse_tokens(line).is_some())
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(LogfmtState)
    }
}

impl ParseState for LogfmtState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let Some(tokens) = parse_tokens(line) else {
            return FeedOutcome::Passed;
        };
        let level = tokens
            .iter()
            .find(|(key, _)| *key == "level")
            .map_or(Severity::Info, |(_, value)| map_level(value));
        let message = tokens
            .iter()
            .find(|(key, _)| *key == "msg")
            .map_or_else(|| line.to_string(), |(_, value)| value.clone());
        FeedOutcome::Claimed(vec![Event {
            stream: ctx.stream,
            severity: level,
            kind: Kind::Line,
            message,
            lines: vec![ctx.line_no],
            ..Event::default()
        }])
    }

    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// Split a whole logfmt line into `key=value` tokens. Returns `None` unless
/// the entire line is whitespace-separated tokens (≥ 2) of `key=value`
/// where the value is either a bare non-whitespace run or a double-quoted
/// string (first closing quote ends it; no escape processing). Keys may not
/// be empty or contain whitespace.
fn parse_tokens(line: &str) -> Option<Vec<(&str, String)>> {
    let mut rest = line;
    let mut tokens = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let eq = rest.find('=')?;
        let key = &rest[..eq];
        if key.is_empty() || key.contains(char::is_whitespace) {
            return None;
        }
        let (value, remainder) = split_value(&rest[eq + 1..])?;
        tokens.push((key, value));
        rest = remainder;
    }
    (tokens.len() >= 2).then_some(tokens)
}

/// Split one token's value off the text after `=`: a quoted value runs to
/// the next `"`, a bare value to the next whitespace. The returned remainder
/// must be empty or start with whitespace, otherwise it is not token
/// grammar (`None`).
fn split_value(after_eq: &str) -> Option<(String, &str)> {
    if let Some(quoted) = after_eq.strip_prefix('"') {
        let close = quoted.find('"')?;
        let remainder = &quoted[close + 1..];
        if !remainder.is_empty() && !remainder.starts_with(char::is_whitespace) {
            return None;
        }
        return Some((quoted[..close].to_string(), remainder));
    }
    let end = after_eq.find(char::is_whitespace).unwrap_or(after_eq.len());
    Some((after_eq[..end].to_string(), &after_eq[end..]))
}
