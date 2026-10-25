//! The module contract (docs/parsers.md §2): every parser — compiled
//! built-in or TOML pack — satisfies the same object-safe interfaces.
//!
//! Contract mapping: identity ([`Parser::name`]), inheritance
//! ([`Parser::extends`]), detection ([`Parser::detect`]), per-run state
//! ([`Parser::new_run`]), line intake ([`ParseState::feed`]) and flush
//! ([`ParseState::finish`]).
//!
//! Stamping contract: states stamp the events they emit with `stream` and
//! `lines` from the [`LineCtx`]; the caller (runner) owns `seq` and `ts_ms`.

use loglense_core::{Event, Stream};

/// Per-line parsing context: which captured stream the line came from and
/// its 1-based line number in that stream's raw spool file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCtx {
    /// Stream that produced the line.
    pub stream: Stream,
    /// 1-based line number in the stream's spool file.
    pub line_no: u64,
}

/// Result of offering one line to a [`ParseState`].
///
/// An enum rather than a bare `Vec<Event>` because a block-buffering module
/// consuming a continuation line (claimed, zero events — docs/parsers.md §2
/// "line intake") must stay distinguishable from a module declining the line
/// (passed).
#[derive(Debug, PartialEq, Eq)]
pub enum FeedOutcome {
    /// The state consumed the line. May carry zero or more events: a
    /// block-buffering module consumes continuation lines silently and emits
    /// when the block closes.
    Claimed(Vec<Event>),
    /// The state did not consume the line; offer it to the next module in
    /// the chain.
    Passed,
}

/// A whole log-format module (docs/parsers.md §2). Registered under
/// [`name`](Parser::name); the registry resolves [`extends`](Parser::extends)
/// at load time and never at parse time.
pub trait Parser: Send + Sync {
    /// Identity: registry key, `--parser` value, `extends` target.
    fn name(&self) -> &'static str;

    /// Optional parent module name this module inherits from.
    fn extends(&self) -> Option<&'static str>;

    /// 0–100 confidence score over a sample of the run's first lines;
    /// 0 never wins autodetection (docs/parsers.md §3.3).
    fn detect(&self, sample: &[&str]) -> u8;

    /// Fresh per-run parsing state. Called once per chain member per stream
    /// so concurrent runs and streams never share mutable state.
    fn new_run(&self) -> Box<dyn ParseState>;
}

/// Per-run, per-stream parsing state (docs/parsers.md §2). Stateful modules
/// buffer while a multi-line block is open; stateless modules return
/// immediately.
pub trait ParseState: Send {
    /// One line in. Emits events for completed blocks via
    /// [`FeedOutcome::Claimed`]; returns [`FeedOutcome::Passed`] when the
    /// line is not this module's business.
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome;

    /// End-of-stream flush of any still-open partial block (best-effort
    /// event).
    fn finish(&mut self) -> Vec<Event>;
}
