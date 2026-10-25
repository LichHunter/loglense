//! Daemonless detached supervisor that owns the wrapped child process.
//!
//! Two roles share this crate (docs/architecture.md §4):
//! - [`spawn_run`]: the CLI/MCP parent side — creates the run atomically,
//!   spawns the detached supervisor, waits for the running ack (200 ms), and
//!   returns instantly.
//! - [`supervisor_main`]: the supervisor side — owns the child, captures both
//!   pipes into pristine raw spools, parses on write into `events.jsonl`,
//!   finalizes `meta.json` and writes `summary.json`.
//!
//! Detach (D-016): the supervisor is the same binary re-exec'd as
//! `__supervisor <run-dir>` in a new session (`setsid`), so it survives the
//! caller's terminal and process group. All libc calls live in `sys`.

mod ansi;
mod capture;
mod control;
mod pump;
mod reader;
mod spawn;
mod summary;
mod supervisor;
mod sys;

pub use control::{
    check_terminal, kill_run, wait_run, KillAction, KillOutcome, Lifecycle, WaitOutcome,
    CONTROL_KILL,
};
pub use spawn::{spawn_run, SpawnOutcome, SpawnRequest, SUPERVISOR_ARG};
pub use summary::{Summary, TemplateEntry};
pub use supervisor::supervisor_main;

use std::path::PathBuf;

/// Errors from the runner: spawn/handshake failures, child-spawn failures
/// inside the supervisor, and wrapped store/registry/config errors.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The wrapped command could not be spawned by the supervisor.
    #[error("cannot spawn `{cmd}`: {source}")]
    SpawnFailed {
        /// The command's argv[0].
        cmd: String,
        /// Underlying I/O failure (usually ENOENT or EACCES).
        source: std::io::Error,
    },
    /// The supervisor did not reach `running` within the 200 ms ack budget
    /// (timeout, early exit, or spawn failure).
    #[error("run {run_id} failed to start: {reason}")]
    StartFailed {
        /// The run that never started.
        run_id: String,
        /// Why: ack timeout, supervisor exit status, or spawn error.
        reason: String,
    },
    /// The state dir's filesystem is below the configured free-space floor
    /// (`retention.min_free_mb`); no run directory was created.
    #[error("only {free_mb} MB free under {path} (floor {floor_mb} MB, config retention.min_free_mb); free space or lower the floor")]
    LowDisk {
        /// The state directory checked.
        path: PathBuf,
        /// Megabytes available.
        free_mb: u64,
        /// Configured floor in megabytes.
        floor_mb: u64,
    },
    /// An explicit `--parser` names an unregistered module.
    #[error("unknown parser `{name}`")]
    UnknownParser {
        /// The requested parser name.
        name: String,
    },
    /// The run directory layout is not usable (missing parent, bad meta).
    #[error("invalid run directory {run_dir}: {reason}")]
    InvalidRunDir {
        /// The offending run directory.
        run_dir: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A filesystem path operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Path the failing operation targeted.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Wrapped store error.
    #[error(transparent)]
    Store(#[from] loglense_store::StoreError),
    /// Wrapped parser-registry error.
    #[error(transparent)]
    Registry(#[from] loglense_parsers::RegistryError),
    /// Wrapped config error.
    #[error(transparent)]
    Config(#[from] loglense_core::ConfigError),
}
