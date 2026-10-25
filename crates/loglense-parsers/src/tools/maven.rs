//! The `maven` tool parser (docs/parsers.md §5.2), extends `java`.
//!
//! Maven-specific single-line rules — `BUILD SUCCESS`/`BUILD FAILURE`
//! verdicts, `Tests run: X, Failures: Y, Errors: Z, Skipped: W` totals,
//! `Reactor Summary` module rows, and the maven bracket prefixes (same
//! shape as the base's, claimed first per the chain's derived-before-base
//! order). Everything else falls through to the embedded `java` base state,
//! so exception blocks and `Caused by:` chains inside maven output still
//! collapse into one event each — including blocks whose closing line is a
//! maven line (the base closes its block and `Pass`es the line; the event
//! ships with this module's next claim).

use std::sync::LazyLock;

use loglense_core::{Event, Kind, Severity};
use regex::Regex;

use crate::bases::java::{bracket_event, JavaState, BRACKET};
use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// `BUILD SUCCESS` / `BUILD FAILURE`, with or without a bracket prefix.
static VERDICT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\[(?:ERROR|WARNING|INFO|DEBUG)\]\s*)?(BUILD SUCCESS|BUILD FAILURE)").unwrap()
});

/// Test totals, with or without a bracket prefix and trailing extras
/// (`Time elapsed: … -- in com.example.FooTest`).
static TOTALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\[(?:ERROR|WARNING|INFO|DEBUG)\]\s*)?Tests run: (\d+), Failures: (\d+), Errors: (\d+), Skipped: (\d+)",
    )
    .unwrap()
});

/// Reactor Summary module row: `[INFO] core ..... FAILURE [  0.512 s]`.
static REACTOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(?:INFO|ERROR|WARNING|DEBUG)\]\s+([\w.-]+)\s+\.+\s+(SUCCESS|FAILURE|SKIPPED)(?:\s+\[[^\]]*\])?$")
        .unwrap()
});

/// The `maven` module: maven rules layered over the `java` base grammar.
#[derive(Debug, Default, Clone, Copy)]
pub struct MavenParser;

/// Per-run state of [`MavenParser`]: the maven rules plus the embedded
/// java base state that owns the exception-block grammar.
pub struct MavenState {
    java: JavaState,
    pending: Vec<Event>,
}

impl Parser for MavenParser {
    fn name(&self) -> &'static str {
        "maven"
    }

    fn extends(&self) -> Option<&'static str> {
        Some("java")
    }

    /// 90 when ≥2 bracket lines co-occur with maven vocabulary (`Tests
    /// run:`, `BUILD SUCCESS`/`BUILD FAILURE`, `Reactor Summary`,
    /// `maven`) — above the java base's 60/70 so maven logs pick maven,
    /// while plain java output (no vocabulary) scores 0 here.
    fn detect(&self, sample: &[&str]) -> u8 {
        let brackets = sample.iter().filter(|line| BRACKET.is_match(line)).count();
        if brackets >= 2 && sample.iter().any(|line| is_maven_vocab(line)) {
            90
        } else {
            0
        }
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(MavenState {
            java: JavaState::default(),
            pending: Vec::new(),
        })
    }
}

/// Maven vocabulary markers for detection (ASCII-case-insensitive).
fn is_maven_vocab(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    lowered.contains("tests run:")
        || lowered.contains("build success")
        || lowered.contains("build failure")
        || lowered.contains("reactor summary")
        || lowered.contains("maven")
}

/// The maven-only event for `line`, or `None` when no maven rule matches
/// (bracket prefixes fall back to the shared java rendering).
fn maven_event(line: &str, ctx: &LineCtx) -> Option<Event> {
    if let Some(caps) = VERDICT.captures(line) {
        let verdict = &caps[1];
        return Some(Event {
            stream: ctx.stream,
            severity: if verdict == "BUILD FAILURE" {
                Severity::Error
            } else {
                Severity::Info
            },
            kind: Kind::Summary,
            message: verdict.to_string(),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    if let Some(caps) = TOTALS.captures(line) {
        let (failures, errors) = (
            caps[2].parse::<u64>().unwrap_or(0),
            caps[3].parse::<u64>().unwrap_or(0),
        );
        return Some(Event {
            stream: ctx.stream,
            severity: if failures > 0 || errors > 0 {
                Severity::Error
            } else {
                Severity::Info
            },
            kind: Kind::Summary,
            message: format!(
                "Tests run: {}, Failures: {}, Errors: {}, Skipped: {}",
                &caps[1], &caps[2], &caps[3], &caps[4]
            ),
            lines: vec![ctx.line_no],
            // Per-class lines carry trailing `, Time elapsed … -- in X`;
            // only the bare module-level line is an aggregate, so summing
            // never double-counts.
            totals: if line.trim_end().len() == caps[0].len() {
                maven_totals(&caps)
            } else {
                std::collections::BTreeMap::new()
            },
            ..Event::default()
        });
    }
    if let Some(caps) = REACTOR.captures(line) {
        let (module, verdict) = (&caps[1], &caps[2]);
        return Some(Event {
            stream: ctx.stream,
            severity: if verdict == "FAILURE" {
                Severity::Error
            } else {
                Severity::Info
            },
            kind: Kind::Phase,
            message: format!("{module}: {verdict}"),
            phase: Some(module.to_string()),
            lines: vec![ctx.line_no],
            ..Event::default()
        });
    }
    bracket_event(line, ctx)
}

impl ParseState for MavenState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        let mut events = std::mem::take(&mut self.pending);
        let block_was_open = self.java.block_open();
        // Derived rules claim first — but never while the base's exception
        // block is absorbing its continuation lines.
        if !block_was_open {
            if let Some(event) = maven_event(line, ctx) {
                events.push(event);
                return FeedOutcome::Claimed(events);
            }
        }
        match self.java.feed(line, ctx) {
            FeedOutcome::Claimed(mut batch) => {
                events.append(&mut batch);
                FeedOutcome::Claimed(events)
            }
            // The base declined — either idle, or it just closed a block on
            // this very line, in which case the closing line is still up
            // for the maven rules.
            FeedOutcome::Passed => {
                events.extend(self.java.take_pending());
                if block_was_open {
                    if let Some(event) = maven_event(line, ctx) {
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
        events.extend(self.java.finish());
        events
    }
}

/// `Tests run: 10, Failures: 2, Errors: 1, Skipped: 1` →
/// `{passed: 6, failed: 2, error: 1, skipped: 1}` (zeros omitted).
fn maven_totals(caps: &regex::Captures<'_>) -> std::collections::BTreeMap<String, u64> {
    let n = |i: usize| caps[i].parse::<u64>().unwrap_or(0);
    let (run, failed, error, skipped) = (n(1), n(2), n(3), n(4));
    let passed = run.saturating_sub(failed + error + skipped);
    [
        ("passed", passed),
        ("failed", failed),
        ("error", error),
        ("skipped", skipped),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(key, count)| (key.to_string(), count))
    .collect()
}
