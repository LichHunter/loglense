//! The `json` format base (docs/parsers.md §5.1): one JSON object per line,
//! mapped through the common keys. Non-object lines (prose, arrays,
//! scalars, broken JSON) are passed so the chain's later members — usually
//! `generic` — claim them. `ts_ms` stays 0: the runner owns timestamps
//! (traits contract).

use loglense_core::{Event, Kind, Severity, Source as EventSource};
use serde_json::Value;

use super::{map_level, percentage};
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// The `json` module: stateless, claims JSON-object lines only.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonParser;

/// Per-run state of [`JsonParser`] — empty; the type satisfies the
/// one-state-per-stream contract.
pub struct JsonState;

impl Parser for JsonParser {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extends(&self) -> Option<&'static str> {
        None
    }

    fn detect(&self, sample: &[&str]) -> u8 {
        percentage(sample, is_json_object)
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(JsonState)
    }
}

impl ParseState for JsonState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(line) else {
            return FeedOutcome::Passed;
        };
        FeedOutcome::Claimed(vec![Event {
            stream: ctx.stream,
            severity: first_string(&fields, &["level", "severity", "lvl"])
                .map_or(Severity::Info, |level| map_level(&level)),
            kind: Kind::Line,
            message: first_string(&fields, &["msg", "message"]).unwrap_or_else(|| line.to_string()),
            source: source_from(&fields),
            lines: vec![ctx.line_no],
            ..Event::default()
        }])
    }

    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// Whether the whole line parses as a JSON object (not array/scalar).
fn is_json_object(line: &str) -> bool {
    matches!(serde_json::from_str::<Value>(line), Ok(Value::Object(_)))
}

/// First present key among `keys` whose value is a string.
fn first_string(fields: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| fields.get(*key).and_then(Value::as_str).map(str::to_string))
}

/// `source` event field from `file`/`line`/`module` when any is present;
/// absent keys contribute their zero values.
fn source_from(fields: &serde_json::Map<String, Value>) -> Option<EventSource> {
    let file = fields.get("file").and_then(Value::as_str);
    let module = fields.get("module").and_then(Value::as_str);
    let line = fields.get("line").and_then(Value::as_u64);
    if file.is_none() && module.is_none() && line.is_none() {
        return None;
    }
    Some(EventSource {
        file: file.unwrap_or_default().to_string(),
        line: line.unwrap_or_default(),
        module: module.map(str::to_string),
    })
}
