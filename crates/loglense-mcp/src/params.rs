//! Typed tool parameters (docs/surfaces.md §5.2). Every run-scoped tool
//! takes an optional `cwd`: the server cannot trust its own working
//! directory (the cwd trap, §5.3), so the run's store is found from `cwd`,
//! else from stores this server already saw (`start_run` / `list_runs`).

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartRunArgs {
    /// Command argv to run detached, e.g. ["cargo", "test"].
    pub cmd: Vec<String>,
    /// Absolute project directory: the command runs here and the run is
    /// stored in this project's `.loglense/` (created when absent).
    pub cwd: String,
    /// Parser module (e.g. "cargo", "pytest", "maven"); omit to autodetect.
    pub parser: Option<String>,
    /// Command (run via `sh -c`) fired when the run finishes — e.g.
    /// `loglense notify tmux --pane %3` to ping your terminal. Gets
    /// LOGLENSE_RUN_ID / LOGLENSE_STATUS / LOGLENSE_EXIT_CODE /
    /// LOGLENSE_STORE_PATH and the summary JSON on stdin.
    pub on_complete: Option<String>,
    /// Callback timeout (default 30000 ms).
    pub on_complete_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListRunsArgs {
    /// Absolute project directory whose `.loglense/` store to list.
    pub cwd: String,
    /// Only runs in this state family.
    pub status: Option<StatusParam>,
    /// Maximum rows (default 50, clamped to 200).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunRef {
    /// Run id as returned by start_run / list_runs (e.g. "20261025-A3F8C2").
    pub run_id: String,
    /// Absolute project directory of the run; optional when this server
    /// started or listed the run already.
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetEventsArgs {
    /// Run id.
    pub run_id: String,
    /// Absolute project directory of the run (optional, see get_run).
    pub cwd: Option<String>,
    /// Only events of this severity.
    pub severity: Option<SeverityParam>,
    /// Only events of this kind.
    pub kind: Option<KindParam>,
    /// Maximum events (default 100, clamped to 1000).
    pub limit: Option<u32>,
    /// Skip this many matching events (oldest first) — pagination.
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLogArgs {
    /// Run id.
    pub run_id: String,
    /// Absolute project directory of the run (optional, see get_run).
    pub cwd: Option<String>,
    /// Which output: "out" (default), "err", or "both" (arrival order).
    pub stream: Option<LogStreamParam>,
    /// Number of lines (default 100, clamped to 10000).
    pub tail_n: Option<u32>,
    /// 1-based raw line to start from instead of tailing (out / err only).
    pub from_line: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitRunArgs {
    /// Run id.
    pub run_id: String,
    /// Absolute project directory of the run (optional, see get_run).
    pub cwd: Option<String>,
    /// How long to wait (default 10000 ms, hard-clamped to 120000).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LeaveFeedbackArgs {
    /// Run id the feedback is about.
    pub run_id: String,
    /// Absolute project directory of the run (optional, see get_run).
    pub cwd: Option<String>,
    /// What kind of report this is.
    pub kind: FeedbackKind,
    /// Free-text description.
    pub detail: String,
    /// Raw output line the feedback refers to.
    pub line: Option<u64>,
    /// The severity the agent expected (e.g. a missed error).
    pub expected_severity: Option<SeverityParam>,
    /// Quoted log fragment.
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StatusParam {
    Starting,
    Running,
    Exited,
    Signaled,
    FailedToStart,
    /// Orphaned or lost (the supervisor died).
    Interrupted,
    Orphaned,
    Lost,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SeverityParam {
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KindParam {
    Line,
    Phase,
    Summary,
    Test,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogStreamParam {
    Out,
    Err,
    Both,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackKind {
    ParseMiss,
    FalsePositive,
    Bug,
    Feature,
    Other,
}

impl FeedbackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FeedbackKind::ParseMiss => "parse-miss",
            FeedbackKind::FalsePositive => "false-positive",
            FeedbackKind::Bug => "bug",
            FeedbackKind::Feature => "feature",
            FeedbackKind::Other => "other",
        }
    }
}

impl SeverityParam {
    pub fn severity(self) -> loglense_core::Severity {
        use loglense_core::Severity;
        match self {
            SeverityParam::Error => Severity::Error,
            SeverityParam::Warning => Severity::Warning,
            SeverityParam::Notice => Severity::Notice,
            SeverityParam::Info => Severity::Info,
            SeverityParam::Debug => Severity::Debug,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SeverityParam::Error => "error",
            SeverityParam::Warning => "warning",
            SeverityParam::Notice => "notice",
            SeverityParam::Info => "info",
            SeverityParam::Debug => "debug",
        }
    }
}

impl KindParam {
    pub fn kind(self) -> loglense_core::Kind {
        use loglense_core::Kind;
        match self {
            KindParam::Line => Kind::Line,
            KindParam::Phase => Kind::Phase,
            KindParam::Summary => Kind::Summary,
            KindParam::Test => Kind::Test,
        }
    }
}

impl StatusParam {
    pub fn filter(self) -> loglense_store::query::StatusFilter {
        use loglense_store::query::StatusFilter;
        match self {
            StatusParam::Starting => StatusFilter::Starting,
            StatusParam::Running => StatusFilter::Running,
            StatusParam::Exited => StatusFilter::Exited,
            StatusParam::Signaled => StatusFilter::Signaled,
            StatusParam::FailedToStart => StatusFilter::FailedToStart,
            StatusParam::Interrupted => StatusFilter::Interrupted,
            StatusParam::Orphaned => StatusFilter::Orphaned,
            StatusParam::Lost => StatusFilter::Lost,
        }
    }
}
