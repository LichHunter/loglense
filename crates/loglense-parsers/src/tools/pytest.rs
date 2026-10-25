//! The `pytest` tool parser (docs/parsers.md §5.2), extends `python`.
//!
//! pytest-specific single-line rules — `FAILED`/`PASSED`/`ERROR` short
//! summary rows, `ERROR at setup/teardown` section banners, the run
//! totals line (`N failed, M passed in Xs`, bare in `-q` runs or
//! `=`-bordered), `INTERNALERROR` lines, warning-summary locations, and
//! the `====`/`____`/`----` section banners (info phases). Everything
//! else falls through to the embedded `python` base state, so tracebacks
//! printed inside pytest runs (captured stdout, internal errors) still
//! collapse into one event each — including blocks whose closing line is
//! a pytest line (the base closes its block and `Pass`es the line; the
//! event ships with this module's next claim).
//!
//! Event shapes: `FAILED tests/x.py::t - detail` → error `kind=test` with
//! `test{name, outcome}`; the totals line → `kind=summary` with
//! `test{name:"pytest", outcome, duration_ms}` and the totals text as the
//! message; warning locations carry `source{file,line}`; section banners
//! → info `kind=phase`.

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity, Source as EventSource, TestInfo};
use regex::Regex;

use crate::bases::python::PythonState;
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// `INTERNALERROR> …` internal-error marker lines.
static INTERNALERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^INTERNALERROR>\s*(.*)$").expect("static pattern compiles"));

/// Short-summary failure row: `FAILED tests/x.py::t - detail`.
static FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^FAILED (\S+)(?: - (.*))?$").expect("static pattern compiles"));

/// Short-summary pass row (shown with `-rA`/`-v` reporting).
static PASSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^PASSED (\S+)(?: - (.*))?$").expect("static pattern compiles"));

/// Short-summary error row: `ERROR tests/x.py::t - detail`.
static ERRORED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ERROR (\S+)(?: - (.*))?$").expect("static pattern compiles"));

/// Section banner for a fixture error: `____ ERROR at setup of t ____`.
static ERROR_AT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^_{3,}\s*ERROR at (?:setup|teardown) of (.+?)\s*_{3,}$")
        .expect("static pattern compiles")
});

/// Warning-summary location: `  path/to/test.py:4: UserWarning: msg`.
static WARNING_LOC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+(\S+):(\d+): ([\w.]*Warning): (.*)$").expect("static pattern compiles")
});

/// Section banner: `==== FAILURES ====`, `____ test_x ____`,
/// `---- Captured stdout call ----`, `~~~~ short test summary info ~~~~`.
static BANNER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-=_~]{3,}\s*(.*?)\s*[-=_~]{3,}$").expect("static pattern compiles")
});

/// Run totals, bare `-q` form: `3 failed, 1 passed in 0.02s`.
static TOTALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^((?:\d+ (?:failed|passed|errors?|skipped|xfailed|xpassed|warnings?|deselected)(?:, )?)+) in (\d+(?:\.\d+)?)s$")
        .expect("static pattern compiles")
});

/// `=`-bordered verbose totals: `==== 3 failed, 1 passed in 0.02s ====`
/// (also other bordered lines; the inner text decides).
static BORDERED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-=_]{3,} (.+?) [-=_]{3,}$").expect("static pattern compiles"));

/// The `pytest` module: pytest rules layered over the `python` base
/// grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct PytestParser;

/// Per-run state of [`PytestParser`]: the pytest rules plus the embedded
/// python base state that owns the traceback-block grammar.
pub struct PytestState {
    python: PythonState,
    pending: Vec<Event>,
}

impl Parser for PytestParser {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn extends(&self) -> Option<&'static str> {
        Some("python")
    }

    /// 90 when ≥2 section banners co-occur with pytest evidence (totals
    /// line, short-summary rows or `pytest` vocabulary) — above the
    /// python base's 80, so pytest runs (even with embedded tracebacks)
    /// pick pytest, while plain python output (no banners) scores 0 here.
    fn detect(&self, sample: &[&str]) -> u8 {
        let banners = sample.iter().filter(|line| BANNER.is_match(line)).count();
        let evidence = sample.iter().any(|line| {
            TOTALS.is_match(line)
                || is_bordered_totals(line)
                || FAILED.is_match(line)
                || PASSED.is_match(line)
                || ERRORED.is_match(line)
                || line.to_ascii_lowercase().contains("pytest")
        });
        if banners >= 2 && evidence {
            90
        } else {
            0
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(PytestState {
            python: PythonState::default(),
            pending: Vec::new(),
        })
    }
}

/// The pytest-only event for `line`, or `None` when no pytest rule matches
/// (tracebacks and logging lines fall through to the embedded python
/// state).
fn pytest_event(line: &str, ctx: &LineCtx) -> Option<Event> {
    if INTERNALERROR.is_match(line) {
        return Some(Event {
            stream: ctx.stream,
            severity: Severity::Error,
            kind: Kind::Line,
            message: line.to_string(),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    if let Some(caps) = FAILED.captures(line) {
        return Some(test_event(ctx, line, &caps[1], "failed"));
    }
    if let Some(caps) = PASSED.captures(line) {
        return Some(test_event(ctx, line, &caps[1], "passed"));
    }
    if let Some(caps) = ERRORED.captures(line) {
        return Some(test_event(ctx, line, &caps[1], "error"));
    }
    if let Some(caps) = ERROR_AT.captures(line) {
        return Some(test_event(ctx, line, &caps[1], "error"));
    }
    if let Some(caps) = TOTALS.captures(line) {
        return Some(totals_event(ctx, line, &caps[1], &caps[2]));
    }
    if let Some(caps) = BORDERED.captures(line) {
        if let Some(totals) = TOTALS.captures(&caps[1]) {
            return Some(totals_event(ctx, &caps[1], &totals[1], &totals[2]));
        }
    }
    if let Some(caps) = WARNING_LOC.captures(line) {
        return Some(Event {
            stream: ctx.stream,
            severity: Severity::Warning,
            kind: Kind::Line,
            message: format!("{}: {}", &caps[3], &caps[4]),
            source: Some(EventSource {
                file: caps[1].to_string(),
                line: caps[2].parse().unwrap_or(0),
                module: None,
            }),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    if let Some(caps) = BANNER.captures(line) {
        let phase = caps[1].trim();
        if !phase.is_empty() {
            return Some(Event {
                stream: ctx.stream,
                severity: Severity::Info,
                kind: Kind::Phase,
                message: line.to_string(),
                phase: Some(phase.to_string()),
                lines: vec![ctx.line_no],
                ..Event::default()
            });
        }
    }
    None
}

/// `kind=test` event for a short-summary row; the outcome drives severity
/// (`failed`/`error` → error, `passed` → info).
fn test_event(ctx: &LineCtx, message: &str, name: &str, outcome: &str) -> Event {
    Event {
        stream: ctx.stream,
        severity: if outcome == "passed" {
            Severity::Info
        } else {
            Severity::Error
        },
        kind: Kind::Test,
        message: message.to_string(),
        lines: vec![ctx.line_no],
        test: Some(TestInfo {
            name: name.to_string(),
            outcome: outcome.to_string(),
            duration_ms: None,
        }),
        ..Event::default()
    }
}

/// `kind=summary` event for a totals line. `text` is the borderless
/// totals text; `counts` its comma-separated count tokens; `secs` the
/// fractional seconds figure.
fn totals_event(ctx: &LineCtx, text: &str, counts: &str, secs: &str) -> Event {
    let (failed, errors) = count_totals(counts);
    let outcome = if failed > 0 {
        "failed"
    } else if errors > 0 {
        "error"
    } else {
        "passed"
    };
    let duration_ms = secs
        .parse::<f64>()
        .map(|s| (s * 1000.0).round() as u64)
        .unwrap_or(0);
    Event {
        stream: ctx.stream,
        severity: if failed > 0 || errors > 0 {
            Severity::Error
        } else {
            Severity::Info
        },
        kind: Kind::Summary,
        message: text.to_string(),
        lines: vec![ctx.line_no],
        test: Some(TestInfo {
            name: "pytest".to_string(),
            outcome: outcome.to_string(),
            duration_ms: Some(duration_ms),
        }),
        totals: outcome_totals(counts),
        ..Event::default()
    }
}

/// `2 failed, 1 passed, 3 warnings` → `{failed: 2, passed: 1}`: test
/// outcomes only (`warnings` / `deselected` are not outcomes), `errors`
/// folded into `error`, zeros omitted.
fn outcome_totals(counts: &str) -> std::collections::BTreeMap<String, u64> {
    let mut totals = std::collections::BTreeMap::new();
    for token in counts.split(", ") {
        let mut parts = token.split(' ');
        let (Some(count), Some(word)) = (parts.next(), parts.next()) else {
            continue;
        };
        let key = match word {
            "failed" | "passed" | "skipped" | "xfailed" | "xpassed" => word,
            "error" | "errors" => "error",
            _ => continue,
        };
        match count.parse::<u64>() {
            Ok(n) if n > 0 => *totals.entry(key.to_string()).or_default() += n,
            _ => {}
        }
    }
    totals
}

/// Sum the `failed` and `error(s)` tokens of a totals count list.
fn count_totals(counts: &str) -> (u64, u64) {
    let mut failed = 0;
    let mut errors = 0;
    for token in counts.split(", ") {
        let mut parts = token.split(' ');
        let (Some(count), Some(word)) = (parts.next(), parts.next()) else {
            continue;
        };
        let count = count.parse::<u64>().unwrap_or(0);
        match word {
            "failed" => failed += count,
            "error" | "errors" => errors += count,
            _ => {}
        }
    }
    (failed, errors)
}

/// Whether `line` is a `=`-bordered totals line.
fn is_bordered_totals(line: &str) -> bool {
    BORDERED
        .captures(line)
        .is_some_and(|caps| TOTALS.is_match(&caps[1]))
}

impl ParseState for PytestState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let mut events = std::mem::take(&mut self.pending);
        let block_was_open = self.python.block_open();
        // Derived rules claim first — but never while the base's traceback
        // block is absorbing its continuation lines.
        if !block_was_open {
            if let Some(event) = pytest_event(line, ctx) {
                events.push(event);
                return FeedOutcome::Claimed(events);
            }
        }
        match self.python.feed(line, ctx) {
            FeedOutcome::Claimed(mut batch) => {
                events.append(&mut batch);
                FeedOutcome::Claimed(events)
            }
            // The base declined — either idle, or it just closed a block on
            // this very line, in which case the closing line is still up
            // for the pytest rules.
            FeedOutcome::Passed => {
                events.extend(self.python.take_pending());
                if block_was_open {
                    if let Some(event) = pytest_event(line, ctx) {
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
        events.extend(self.python.finish());
        events
    }
}
