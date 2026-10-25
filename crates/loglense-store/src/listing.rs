//! Run listing with derived liveness (D-006: directory scan, no index file).

use std::path::Path;

/// Derived-on-read substate, never persisted (docs/architecture.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivedState {
    /// Supervisor dead but child still alive (starttime-verified).
    Orphaned,
    /// Supervisor and child both dead while status says running.
    Lost,
    /// meta.json missing or unparseable.
    Corrupt,
}

/// One row of a run listing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RunSummary {
    /// Run id (directory name for corrupt runs).
    pub run_id: String,
    /// Persisted status; `None` when the meta is corrupt.
    pub status: Option<crate::Status>,
    /// Derived substate, `None` when the persisted state is current.
    pub derived: Option<DerivedState>,
    /// First token of the wrapped command ("" for corrupt runs).
    pub cmd_head: String,
    /// Epoch ms start time; `None` for corrupt runs.
    pub started_at_ms: Option<i64>,
    /// Epoch ms end time when terminal.
    pub ended_at_ms: Option<i64>,
}

/// Lists runs under `state/runs/`, newest first.
///
/// Reads only each `runs/<id>/meta.json`. Corrupt or missing metas are
/// included as [`DerivedState::Corrupt`] placeholders — this function never
/// panics and never mutates the store.
pub fn list_runs(state: &Path) -> Vec<RunSummary> {
    let runs_dir = state.join(crate::meta::RUNS_DIR);
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut summaries: Vec<RunSummary> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| summarize_run(&entry.path()))
        .collect();
    summaries.sort_by(|a, b| {
        b.started_at_ms
            .unwrap_or(i64::MIN)
            .cmp(&a.started_at_ms.unwrap_or(i64::MIN))
    });
    summaries
}

/// Builds one row; a corrupt or missing meta yields a placeholder row named
/// after the directory.
pub fn summarize_run(run_dir: &Path) -> RunSummary {
    let dir_name = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut meta = crate::meta::read_meta(run_dir);
    // A dead-supervisor verdict on a `running` meta may be a stale read: the
    // supervisor writes its terminal meta and only then exits. Death is
    // permanent, so one re-read after the liveness check is authoritative.
    if meta.as_ref().is_ok_and(|m| derived_state(m).is_some()) {
        meta = crate::meta::read_meta(run_dir);
    }
    let Ok(meta) = meta else {
        return RunSummary {
            run_id: dir_name,
            status: None,
            derived: Some(DerivedState::Corrupt),
            cmd_head: String::new(),
            started_at_ms: None,
            ended_at_ms: None,
        };
    };
    RunSummary {
        run_id: meta.run_id.to_string(),
        status: Some(meta.status),
        derived: derived_state(&meta),
        cmd_head: meta.cmd.first().cloned().unwrap_or_default(),
        started_at_ms: Some(meta.started_at_ms),
        ended_at_ms: meta.ended_at_ms,
    }
}

/// Derived liveness (docs/architecture.md §4.1): a persisted `starting` /
/// `running` whose supervisor died splits into orphaned (child still ours,
/// starttime verified) or lost (child gone or pid reused). Never persisted.
fn derived_state(meta: &crate::Meta) -> Option<DerivedState> {
    let live_status = matches!(
        meta.status,
        crate::Status::Starting | crate::Status::Running
    );
    if !live_status || supervisor_alive(meta) {
        return None;
    }
    let child_is_ours = meta.child_pid.is_some_and(|pid| {
        proc_alive(pid) && proc_starttime(pid).is_some_and(|st| Some(st) == meta.child_starttime)
    });
    Some(if child_is_ours {
        DerivedState::Orphaned
    } else {
        DerivedState::Lost
    })
}

/// Supervisor alive AND still this run's supervisor: it is exec'd with the
/// run directory in its argv, so a recycled pid never makes a dead run look
/// live (same guard as the runner's kill/wait paths).
fn supervisor_alive(meta: &crate::Meta) -> bool {
    if meta.supervisor_pid == 0 || !proc_alive(meta.supervisor_pid) {
        return false;
    }
    let run_id = meta.run_id.to_string();
    std::fs::read(format!("/proc/{}/cmdline", meta.supervisor_pid)).is_ok_and(|cmdline| {
        cmdline
            .split(|b| *b == 0)
            .any(|arg| arg.ends_with(run_id.as_bytes()))
    })
}

/// Whether `/proc/<pid>` exists — `kill(pid, 0)` semantics without a libc
/// dependency.
pub(crate) fn proc_alive(pid: u32) -> bool {
    std::fs::metadata(format!("/proc/{pid}")).is_ok()
}

/// `/proc/<pid>/stat` field 22 (starttime, clock ticks since boot) — the
/// pid-reuse guard. Parsing splits after the closing paren of the comm
/// field, whose contents may themselves contain spaces.
pub(crate) fn proc_starttime(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}
