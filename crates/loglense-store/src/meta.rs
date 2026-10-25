//! Run metadata (`meta.json`) and run-directory layout.

use std::path::{Path, PathBuf};

use loglense_core::RunId;
use time::Date;

use crate::StoreError;

/// Persisted lifecycle state of a run (see docs/architecture.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Run dir created, supervisor not yet acked.
    Starting,
    /// Supervisor spawned the child.
    Running,
    /// Child exited normally with a code.
    Exited,
    /// Child was terminated by a signal.
    Signaled,
    /// Supervisor failed to spawn or ack in time.
    FailedToStart,
}

/// Completion-callback request stored in meta (D-011).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OnComplete {
    /// Callback command line, executed by the supervisor at terminal state.
    pub command: String,
    /// Callback timeout in milliseconds (default 30 000).
    pub timeout_ms: u64,
}

/// The run record persisted at `runs/<id>/meta.json`.
///
/// Every field except the optionals is required on write; readers tolerate
/// absent optional fields and unknown extra fields (forwards compatibility).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Meta {
    /// Id of this run; matches the directory name.
    pub run_id: RunId,
    /// Wrapped command line, `argv[0]` first.
    pub cmd: Vec<String>,
    /// Working directory the command was started in.
    pub cwd: PathBuf,
    /// Pid of the owning supervisor process.
    pub supervisor_pid: u32,
    /// Pid of the wrapped child, once spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    /// `/proc/<pid>/stat` field 22 of the child — pid-reuse guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_starttime: Option<u64>,
    /// Selected parser name (`auto` while undetected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<String>,
    /// Persisted lifecycle state.
    pub status: Status,
    /// Exit code when [`Status::Exited`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Terminating signal when [`Status::Signaled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Epoch milliseconds when the run started.
    pub started_at_ms: i64,
    /// Epoch milliseconds when the run reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
    /// loglense version that wrote this record.
    pub tool_version: String,
    /// On-disk meta schema version (currently 1).
    pub schema_version: u32,
    /// Requested completion callback, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<OnComplete>,
}

/// Reads and parses `run_dir/meta.json`. Fails with [`StoreError::Io`] when
/// absent and [`StoreError::MetaParse`] when truncated or corrupt.
pub fn read_meta(run_dir: &Path) -> Result<Meta, StoreError> {
    let path = run_dir.join(META_FILE);
    let bytes = std::fs::read(&path).map_err(|source| StoreError::Io {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| StoreError::MetaParse { path, source })
}

/// Atomically writes `meta` to `run_dir/meta.json` (`.tmp` + rename).
pub fn write_meta(run_dir: &Path, meta: &Meta) -> Result<(), StoreError> {
    let path = run_dir.join(META_FILE);
    let bytes = serde_json::to_vec(meta).map_err(|source| StoreError::MetaParse {
        path: path.clone(),
        source,
    })?;
    crate::write_atomic(&path, &bytes)
}

/// Creates `state/runs/<id>/` with a collision-aware id (`EEXIST` → new id,
/// at most 5 tries) and empty `meta.json`, `stdout.raw`, `stderr.raw`,
/// `events.jsonl`. Returns the id and the run directory.
pub fn create_run_dir_at(
    state: &Path,
    run_date: Date,
    entropy: &mut dyn FnMut() -> u32,
) -> Result<(RunId, PathBuf), StoreError> {
    let runs = state.join(RUNS_DIR);
    std::fs::create_dir_all(&runs).map_err(|source| StoreError::Io {
        path: runs.clone(),
        source,
    })?;
    let (id, dir) = RunId::create_run_dir(&runs, run_date, entropy)?;
    for name in RUN_FILES {
        std::fs::File::create(dir.join(name)).map_err(|source| StoreError::Io {
            path: dir.join(name),
            source,
        })?;
    }
    Ok((id, dir))
}

/// [`create_run_dir_at`] with today's date and process-seeded entropy.
pub fn create_run_dir(state: &Path) -> Result<(RunId, PathBuf), StoreError> {
    let today = time::OffsetDateTime::now_utc().date();
    create_run_dir_at(state, today, &mut seeded_entropy)
}

/// Std-only randomness: every `RandomState` is keyed from a per-thread
/// counter seeded by the OS, so hashing a fixed value still varies per call.
fn seeded_entropy() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(0);
    (hasher.finish() >> 32) as u32
}

/// File name of the run record inside a run directory.
pub const META_FILE: &str = "meta.json";
/// Directory holding run directories inside the state dir.
pub const RUNS_DIR: &str = "runs";
/// Files every fresh run directory contains (all empty until the runner
/// writes them).
const RUN_FILES: [&str; 4] = ["meta.json", "stdout.raw", "stderr.raw", "events.jsonl"];
