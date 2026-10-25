//! Retention garbage collection, flock-serialized on `gc.lock`.

use std::path::Path;

use crate::StoreError;

/// Outcome of one gc pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Run directories removed.
    pub runs_removed: usize,
    /// Bytes reclaimed (logical file sizes of removed runs).
    pub bytes_freed: u64,
}

/// Deletes runs past retention, serialized by flock on `<state>/gc.lock`.
///
/// Removal candidates: runs whose `ended_at_ms` is older than
/// `now_ms - max_age_days`, then — while the remaining total exceeds
/// `max_total_mb` (decimal MB) — the oldest runs first. Runs whose status
/// is running **and** whose supervisor is alive are never touched.
/// Deletion renames the run dir to `<state>/.trash-<ts>-<id>` before
/// removal so racing readers never observe a half-deleted directory;
/// `ENOENT` races are ignored.
pub fn gc(
    state: &Path,
    max_age_days: u64,
    max_total_mb: u64,
    now_ms: i64,
) -> Result<GcReport, StoreError> {
    use fs4::fs_std::FileExt as _;

    let lock_path = state.join(GC_LOCK);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| StoreError::Io {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive().map_err(|source| StoreError::Io {
        path: lock_path.clone(),
        source,
    })?;
    let report = gc_under_lock(state, max_age_days, max_total_mb, now_ms);
    let unlock = lock.unlock().map_err(|source| StoreError::Io {
        path: lock_path,
        source,
    });
    unlock?;
    report
}

/// Name of the gc serialization lock file inside the state dir.
pub const GC_LOCK: &str = "gc.lock";

/// One scanned run directory with the facts gc decides on.
struct RunEntry {
    dir: std::path::PathBuf,
    ended_at_ms: Option<i64>,
    started_at_ms: i64,
    live: bool,
    bytes: u64,
}

fn gc_under_lock(
    state: &Path,
    max_age_days: u64,
    max_total_mb: u64,
    now_ms: i64,
) -> Result<GcReport, StoreError> {
    let entries = scan_runs(state);
    let age_cutoff = now_ms - (max_age_days as i64) * 24 * 60 * 60 * 1000;
    let budget_bytes = max_total_mb.saturating_mul(1_000_000);

    let mut doomed: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.live && e.ended_at_ms.is_some_and(|ended| ended < age_cutoff))
        .map(|(i, _)| i)
        .collect();
    let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
    let mut rest: Vec<usize> = (0..entries.len())
        .filter(|i| !entries[*i].live && !doomed.contains(i))
        .collect();
    rest.sort_by_key(|i| entries[*i].started_at_ms);
    for i in rest {
        if total <= budget_bytes {
            break;
        }
        total -= entries[i].bytes;
        doomed.push(i);
    }

    let mut report = GcReport::default();
    for i in doomed {
        if delete_run(state, &entries[i].dir, now_ms)? {
            report.runs_removed += 1;
            report.bytes_freed += entries[i].bytes;
        }
    }
    Ok(report)
}

/// Scans `runs/*` for entries; read errors degrade to conservative facts
/// (unknown meta, not live) rather than aborting the pass.
fn scan_runs(state: &Path) -> Vec<RunEntry> {
    let runs_dir = state.join(crate::meta::RUNS_DIR);
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let dir = e.path();
            let meta = crate::meta::read_meta(&dir).ok();
            let live = meta.as_ref().is_some_and(|m| {
                m.status == crate::Status::Running && crate::listing::proc_alive(m.supervisor_pid)
            });
            RunEntry {
                ended_at_ms: meta.as_ref().and_then(|m| m.ended_at_ms),
                started_at_ms: meta.as_ref().map_or(i64::MIN, |m| m.started_at_ms),
                live,
                bytes: dir_size(&dir),
                dir,
            }
        })
        .collect()
}

/// Renames the run dir to `<state>/.trash-<ts>-<id>` (atomic disappearance
/// from `runs/`) then removes it. Returns `false` when a racing gc peer
/// already took the dir (`ENOENT`), so callers count each removal once.
fn delete_run(state: &Path, dir: &Path, now_ms: i64) -> Result<bool, StoreError> {
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let trash = state.join(format!(".trash-{now_ms}-{id}"));
    match std::fs::rename(dir, &trash) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(StoreError::Io {
                path: dir.to_path_buf(),
                source,
            })
        }
        _ => {}
    }
    match std::fs::remove_dir_all(&trash) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(StoreError::Io {
            path: trash,
            source: e,
        }),
        _ => Ok(true),
    }
}

/// Recursive logical size of a directory (sparse files count as their
/// `len()`, matching how the budget is reasoned about).
fn dir_size(dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&entry.path(), total);
            } else if let Ok(md) = entry.metadata() {
                *total += md.len();
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total
}
