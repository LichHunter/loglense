//! Project-local `.loglense/` run store: discovery, atomic metadata, tolerant readers.
//!
//! The filesystem is the state (D-001/D-002): every consumer is a pure file
//! reader over a `.loglense/` directory. All metadata writes are atomic
//! (temp file + rename) and all readers tolerate partially written or
//! corrupted files without panicking.

mod discovery;
mod events;
mod feedback;
mod gc;
mod interleave;
mod listing;
mod meta;

use std::path::{Path, PathBuf};

use loglense_core::RunIdError;

pub use discovery::{create, discover, resolve, resolve_from, STATE_DIR_NAME};
pub use events::{read_events, scan_events, EventsRead, EventsWriter};
pub use feedback::{
    feedback_index_path, list_feedback, read_feedback, read_feedback_index, resolve_feedback,
    write_feedback, FeedbackPointer, FeedbackRecord, FeedbackStatus,
};
pub use gc::{gc, GcReport};
pub use interleave::{last_segments, InterleaveWriter, Segment, INTERLEAVE_FILE};
pub use listing::{list_runs, DerivedState, RunSummary};
pub use meta::{
    create_run_dir, create_run_dir_at, read_meta, write_meta, Meta, OnComplete, Status, META_FILE,
    RUNS_DIR,
};

/// Errors from every store operation — every variant names the path (or id)
/// involved so users can act on the message directly.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// No `.loglense/` directory found between `start` and its walk-up
    /// boundary (git root / home).
    #[error(
        "no `{STATE_DIR_NAME}/` state directory found walking up from {start} \
         (search stops at the git root and $HOME); run `loglense init` here, or point \
         --dir / LOGLENSE_STATE_DIR at an existing state directory"
    )]
    StateDirNotFound {
        /// Directory the upward walk started from.
        start: PathBuf,
    },
    /// Creation was refused in `/` or `$HOME` without `--force`.
    #[error(
        "refusing to create `{STATE_DIR_NAME}/` in {parent} without --force; \
         pick a project directory or set --dir / LOGLENSE_STATE_DIR"
    )]
    RefusedCreate {
        /// Directory whose child `.loglense/` was refused.
        parent: PathBuf,
    },
    /// A `.loglense` path exists but is a regular file, not a directory.
    #[error(
        "{path} exists as a regular file; a `{STATE_DIR_NAME}/` state directory was \
         expected — remove the file or point --dir / LOGLENSE_STATE_DIR elsewhere"
    )]
    StateDirIsFile {
        /// The offending regular-file path.
        path: PathBuf,
    },
    /// Filesystem I/O failure; the message includes the os error text.
    #[error("{path}: {source}")]
    Io {
        /// Path the failing operation targeted.
        path: PathBuf,
        /// Underlying error (EACCES, EROFS, ENOTDIR, …).
        source: std::io::Error,
    },
    /// `meta.json` could not be parsed (truncated or corrupt).
    #[error("parsing run metadata {path} failed: {source}")]
    MetaParse {
        /// The offending meta.json path.
        path: PathBuf,
        /// Serde error describing the parse failure.
        source: serde_json::Error,
    },
    /// Serializing a store record to JSON failed.
    #[error("serializing {what} failed: {source}")]
    Serialize {
        /// What was being serialized (e.g. `events.jsonl line`).
        what: &'static str,
        /// Serde error describing the failure.
        source: serde_json::Error,
    },
    /// A feedback record id has no backing file.
    #[error("feedback record {fid} not found under {dir}")]
    FeedbackNotFound {
        /// The feedback id, e.g. `f-1764000000-3`.
        fid: String,
        /// The searched `.loglense/feedback/` directory.
        dir: PathBuf,
    },
    /// A feedback record file could not be parsed.
    #[error("parsing feedback record {path} failed: {source}")]
    FeedbackParse {
        /// The offending record path.
        path: PathBuf,
        /// Serde error describing the parse failure.
        source: serde_json::Error,
    },
    /// Feedback id generation could not find a free slot.
    #[error("no free feedback id under {dir} after {tries} tries")]
    FeedbackIdExhausted {
        /// The `.loglense/feedback/` directory.
        dir: PathBuf,
        /// Attempts made.
        tries: u32,
    },
    /// Run-directory creation failed at the id layer.
    #[error(transparent)]
    RunId(#[from] RunIdError),
}

/// Writes `bytes` to `path` atomically: `path.tmp` is written and synced,
/// then renamed over `path`. Readers observe either the old or the new file,
/// never a partial one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = atomic_tmp_path(path);
    std::fs::write(&tmp, bytes)
        .and_then(|()| {
            let file = std::fs::File::open(&tmp)?;
            file.sync_all()
        })
        .map_err(|source| StoreError::Io {
            path: tmp.clone(),
            source,
        })?;
    std::fs::rename(&tmp, path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The `<file>.tmp` sibling used by [`write_atomic`].
pub(crate) fn atomic_tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Shorthand for the pervasive `Io { path, source }` mapping.
pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.into(),
        source,
    }
}
