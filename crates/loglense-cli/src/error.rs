//! CLI errors and the exit-code matrix (docs/surfaces.md §1.4).
//!
//! loglense's own failures always exit 2; 1 is reserved for "the wrapped
//! run failed" (produced by `wait`), 124 for `wait` timeouts.

use std::path::PathBuf;

/// Usage error, unknown id, corrupt run, or any loglense-side failure.
pub(crate) const EXIT_USAGE: u8 = 2;
/// `wait`: the observed run did not succeed.
pub(crate) const EXIT_RUN_FAILED: u8 = 1;
/// `wait`: the timeout elapsed with the run still live.
pub(crate) const EXIT_TIMEOUT: u8 = 124;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    /// Bad flag combination or argument value clap could not catch.
    #[error("{0}")]
    Usage(String),
    /// No such run directory in the store.
    #[error("unknown run id `{id}` in {state} (see `loglense ls`)")]
    UnknownRun { id: String, state: PathBuf },
    /// The run exists but its records are unreadable.
    #[error("run {id} is corrupt: {reason}")]
    CorruptRun { id: String, reason: String },
    /// The requested subcommand ships in a later task.
    #[error("`loglense {0}` is not implemented yet")]
    NotImplemented(&'static str),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] loglense_store::StoreError),
    #[error(transparent)]
    Runner(#[from] loglense_runner::RunnerError),
    #[error(transparent)]
    Config(#[from] loglense_core::ConfigError),
}

impl CliError {
    /// Every loglense-side failure maps to the usage/unknown/corrupt code.
    pub(crate) fn exit_code(&self) -> u8 {
        EXIT_USAGE
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CliError::Io {
            path: path.into(),
            source,
        }
    }
}
