//! Kill controls and wait fast-fail (docs/architecture.md §4.1,
//! docs/surfaces.md §3).
//!
//! `kill` on a live run does two things: it drops a `control.kill` marker
//! into the run directory — the supervisor polls it and owns the TERM → 5 s
//! grace → KILL escalation, so a CLI that exits right away still gets a
//! guaranteed kill — and it sends TERM to the child's process group at once.
//! Every signal is preceded by the pid-reuse guard: the child's pid must
//! still carry the `/proc` starttime recorded in meta. Orphaned runs (the
//! supervisor died) are escalated by the caller directly.

use std::path::Path;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use loglense_store::{read_meta, Meta, Status};

use crate::sys;

/// Marker file whose presence requests the kill sequence.
pub const CONTROL_KILL: &str = "control.kill";

/// Grace between TERM and KILL.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// Derived liveness for `wait`/`kill` fast-fails (mirrors the store's
/// listing classification; never persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Supervisor alive; nothing terminal yet.
    Running,
    /// Terminal persisted state (exited / signaled / failed-to-start).
    Terminal(Status),
    /// Supervisor dead but the child is still ours (starttime-verified).
    Orphaned,
    /// Supervisor and child both dead with a non-terminal persisted state.
    Lost,
    /// `meta.json` missing or unparseable.
    Corrupt,
}

/// Fast-fail hook used by `wait`: returns immediately with the terminal or
/// derived state instead of polling to a timeout.
pub fn check_terminal(run_dir: &Path) -> Lifecycle {
    observe(run_dir).map_or(Lifecycle::Corrupt, |(_, lifecycle)| lifecycle)
}

/// Reads and classifies the run. A dead-supervisor verdict on a live-status
/// meta may be a stale read (the supervisor writes its terminal meta and
/// only then exits); death is permanent, so one re-read after the liveness
/// check is authoritative.
fn observe(run_dir: &Path) -> Option<(Meta, Lifecycle)> {
    let meta = read_meta(run_dir).ok()?;
    let lifecycle = classify(&meta);
    if matches!(lifecycle, Lifecycle::Orphaned | Lifecycle::Lost) {
        let meta = read_meta(run_dir).ok()?;
        let lifecycle = classify(&meta);
        return Some((meta, lifecycle));
    }
    Some((meta, lifecycle))
}

fn classify(meta: &Meta) -> Lifecycle {
    match meta.status {
        Status::Exited | Status::Signaled | Status::FailedToStart => {
            Lifecycle::Terminal(meta.status)
        }
        Status::Starting | Status::Running if supervisor_alive(meta) => Lifecycle::Running,
        Status::Starting | Status::Running if child_is_ours(meta) => Lifecycle::Orphaned,
        Status::Starting | Status::Running => Lifecycle::Lost,
    }
}

/// How a [`wait_run`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitOutcome {
    /// Final observed lifecycle (terminal, derived dead substate, or still
    /// running when timed out).
    pub lifecycle: Lifecycle,
    /// Child exit code when the run exited.
    pub exit_code: Option<i32>,
    /// Whether the timeout elapsed while the run was still live.
    pub timed_out: bool,
}

/// Blocks until the run is terminal, derived-dead (orphaned / lost /
/// corrupt — returned at once, never polled to the timeout), or `timeout`
/// elapses. `None` waits indefinitely. `poll` is the meta poll cadence
/// (CLI 250 ms, MCP 100 ms per docs/surfaces.md §3).
pub fn wait_run(run_dir: &Path, timeout: Option<Duration>, poll: Duration) -> WaitOutcome {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let observed = observe(run_dir);
        let lifecycle = observed
            .as_ref()
            .map_or(Lifecycle::Corrupt, |(_, lifecycle)| *lifecycle);
        let exit_code = observed.as_ref().and_then(|(meta, _)| meta.exit_code);
        if lifecycle != Lifecycle::Running {
            return WaitOutcome {
                lifecycle,
                exit_code,
                timed_out: false,
            };
        }
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d) {
            return WaitOutcome {
                lifecycle,
                exit_code,
                timed_out: true,
            };
        }
        let nap = deadline.map_or(poll, |d| poll.min(d.saturating_duration_since(now)));
        thread::sleep(nap);
    }
}

/// What [`kill_run`] did (docs/surfaces.md `kill` / `kill_run`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillAction {
    /// TERM sent to the child's process group (escalation to KILL owned by
    /// the supervisor, or done inline for an orphaned run).
    Signaled,
    /// The run already reached a terminal state; nothing was signaled.
    AlreadyTerminal,
    /// Nothing was signaled because doing so would be unsafe or pointless
    /// (pid reuse, lost run, corrupt meta); `detail` says which.
    Refused,
}

/// Result of a [`kill_run`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillOutcome {
    /// What was done.
    pub action: KillAction,
    /// Lifecycle observed before acting.
    pub lifecycle: Lifecycle,
    /// Human-readable explanation of what was signaled or why not.
    pub detail: String,
}

/// Terminates a run with the liveness and pid-reuse guards of
/// docs/surfaces.md: live runs get the marker + an immediate group TERM;
/// orphaned runs are escalated TERM → 5 s → KILL inline; terminal, lost,
/// corrupt, or pid-reused runs are never signaled.
pub fn kill_run(run_dir: &Path) -> KillOutcome {
    let Some((meta, lifecycle)) = observe(run_dir) else {
        return KillOutcome {
            action: KillAction::Refused,
            lifecycle: Lifecycle::Corrupt,
            detail: "meta.json missing or unreadable".to_string(),
        };
    };
    let outcome = |action, detail: String| KillOutcome {
        action,
        lifecycle,
        detail,
    };
    match lifecycle {
        Lifecycle::Terminal(status) => outcome(
            KillAction::AlreadyTerminal,
            format!("run already completed ({})", status_name(status)),
        ),
        Lifecycle::Corrupt => outcome(
            KillAction::Refused,
            "meta.json missing or unreadable".to_string(),
        ),
        Lifecycle::Lost => match meta.child_pid {
            Some(pid) if proc_alive(pid) => outcome(
                KillAction::Refused,
                format!("pid {pid} is alive but its starttime no longer matches the run's child (pid reuse); refusing to signal it"),
            ),
            _ => outcome(
                KillAction::Refused,
                "run is lost: supervisor and child are both gone".to_string(),
            ),
        },
        Lifecycle::Running => {
            if let Err(err) = std::fs::write(run_dir.join(CONTROL_KILL), b"") {
                return outcome(
                    KillAction::Refused,
                    format!(
                        "cannot write kill request into {}: {err}",
                        run_dir.display()
                    ),
                );
            }
            match meta.child_pid {
                Some(pid) if child_is_ours(&meta) => {
                    let _ = sys::signal_group(pid, sys::SIGTERM);
                    outcome(
                        KillAction::Signaled,
                        format!(
                            "TERM sent to process group {pid}; supervisor {} escalates to KILL after {}s",
                            meta.supervisor_pid,
                            TERM_GRACE.as_secs()
                        ),
                    )
                }
                Some(pid) => outcome(
                    KillAction::Refused,
                    format!("child pid {pid} no longer matches the recorded starttime (pid reuse); kill requested from supervisor {} only", meta.supervisor_pid),
                ),
                None => outcome(
                    KillAction::Signaled,
                    format!(
                        "kill requested from supervisor {} (child not spawned yet)",
                        meta.supervisor_pid
                    ),
                ),
            }
        }
        Lifecycle::Orphaned => {
            let Some(pid) = meta.child_pid else {
                return outcome(
                    KillAction::Refused,
                    "orphaned run has no child pid".to_string(),
                );
            };
            terminate_group(pid, || !child_is_ours(&meta));
            outcome(
                KillAction::Signaled,
                format!(
                    "orphaned run (supervisor {} gone): TERM then KILL sent to process group {pid}",
                    meta.supervisor_pid
                ),
            )
        }
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Starting => "starting",
        Status::Running => "running",
        Status::Exited => "exited",
        Status::Signaled => "signaled",
        Status::FailedToStart => "failed-to-start",
    }
}

/// Whether `loglense kill` was requested for this run.
pub(crate) fn kill_requested(run_dir: &Path) -> bool {
    run_dir.join(CONTROL_KILL).symlink_metadata().is_ok()
}

/// Supervisor side of the kill sequence: TERM to the child's process
/// group, 5 s grace, then KILL to the group and the direct child.
pub(crate) fn terminate_child(child: &mut Child) {
    let pgid = child.id();
    terminate_group(pgid, || child.try_wait().ok().flatten().is_some());
    let _ = child.kill();
}

/// TERM to group `pgid`, poll `gone` for up to the grace period, then KILL.
fn terminate_group(pgid: u32, mut gone: impl FnMut() -> bool) {
    let _ = sys::signal_group(pgid, sys::SIGTERM);
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        if gone() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = sys::signal_group(pgid, sys::SIGKILL);
}

/// Force-kills group `pgid` (grandchildren holding the pipes open).
pub(crate) fn kill_group(pgid: u32) {
    let _ = sys::signal_group(pgid, sys::SIGKILL);
}

/// Whether `/proc/<pid>` exists (`kill(pid, 0)` semantics).
pub(crate) fn proc_alive(pid: u32) -> bool {
    pid != 0 && std::fs::metadata(format!("/proc/{pid}")).is_ok()
}

/// Supervisor alive AND still our supervisor: its argv names this run (the
/// supervisor is exec'd with the run directory as an argument), so a
/// recycled pid can never make a dead run look live.
fn supervisor_alive(meta: &Meta) -> bool {
    if !proc_alive(meta.supervisor_pid) {
        return false;
    }
    let run_id = meta.run_id.to_string();
    std::fs::read(format!("/proc/{}/cmdline", meta.supervisor_pid)).is_ok_and(|cmdline| {
        cmdline
            .split(|b| *b == 0)
            .any(|arg| arg.ends_with(run_id.as_bytes()))
    })
}

/// `/proc/<pid>/stat` field 22 (starttime) — the pid-reuse guard. Local
/// twin of the store's helper (which is crate-private); splitting after
/// the `)` of the comm field whose contents may contain spaces.
pub(crate) fn proc_starttime(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// Child still alive and matching the recorded starttime (pid-reuse guard).
fn child_is_ours(meta: &Meta) -> bool {
    meta.child_pid.is_some_and(|pid| {
        proc_alive(pid) && proc_starttime(pid).is_some_and(|st| Some(st) == meta.child_starttime)
    })
}
