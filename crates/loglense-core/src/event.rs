//! The normalized [`Event`] model — the stable interface between parsers and
//! every consumer (summary, dedup, CLI, MCP, golden fixtures).
//!
//! Wire format per docs/parsers.md §1: field names are snake_case exactly as
//! listed there; `stream`/`severity`/`kind` serialize as lowercase words
//! (`out`/`err`, `error`…`debug`, `line`…`test`). Optional fields are omitted
//! when empty, and deserialization tolerates unknown fields (forwards
//! compatibility) and missing optional collections (tolerant readers).

use serde::{Deserialize, Serialize};

/// Which captured stream produced an event.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// Standard output.
    #[default]
    Out,
    /// Standard error.
    Err,
}

/// Event severity, ordered from worst to mildest.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Failures the user must act on.
    Error,
    /// Suspicious but non-fatal conditions.
    Warning,
    /// Noteworthy neutral markers.
    Notice,
    /// Routine output.
    #[default]
    Info,
    /// Verbose diagnostics.
    Debug,
}

/// How consumers render and filter an event.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Default single-line or block event.
    #[default]
    Line,
    /// Named build/test phase marker.
    Phase,
    /// Run- or section-level verdict/totals.
    Summary,
    /// One test case outcome.
    Test,
}

/// Origin info when the format carries it (compiler diagnostics, tracebacks).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Source {
    /// Source file path as printed by the tool.
    pub file: String,
    /// 1-based line number in `file`.
    pub line: u64,
    /// Module/function path, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// One test case outcome attached to a [`Kind::Test`] event.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TestInfo {
    /// Fully qualified test name.
    pub name: String,
    /// Outcome, e.g. `passed` / `failed` / `error` / `skipped`.
    pub outcome: String,
    /// Test duration in milliseconds, when the format reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The normalized event every parser module emits (docs/parsers.md §1.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    /// Monotonic sequence within the run, across both streams.
    pub seq: u64,
    /// Epoch milliseconds, set by the runner at capture time.
    pub ts_ms: i64,
    /// Which captured stream produced this event.
    pub stream: Stream,
    /// Severity level.
    pub severity: Severity,
    /// Rendering/filtering category.
    pub kind: Kind,
    /// Human-readable content (captures interpolated for pack rules).
    pub message: String,
    /// Id of the normalized template used for dedup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Origin info, when the format carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Raw 1-based line numbers in the stream's spool file this event covers.
    pub lines: Vec<u64>,
    /// Attached stack-trace frames (one event per logical block).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<String>,
    /// Named build/test phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Test case info for [`Kind::Test`] events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestInfo>,
    /// Aggregate test counts by outcome (`passed`, `failed`, `error`,
    /// `skipped`, …) carried by a tool's totals line (`2 failed, 1 passed`,
    /// `test result: …`, maven's module `Tests run:`). Zero counts are
    /// omitted. The run summary sums these when any are present (D-018).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub totals: std::collections::BTreeMap<String, u64>,
}
