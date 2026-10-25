//! The CLI/MCP parent side: create the run atomically, spawn the detached
//! supervisor, wait for the `running` ack (≤ 200 ms), return instantly
//! (docs/architecture.md §4.2).
//!
//! Detach (D-016): the supervisor is a re-exec of the current binary as
//! `<exe> __supervisor <run-dir>` that calls `setsid` between fork and exec
//! — a new session with no controlling terminal and its own process group,
//! so terminal hangups and signals to the caller's group never reach it.
//! The forked child is never a group leader, so `setsid` cannot hit the
//! EPERM the double-fork form guards against. The ack is the atomic
//! `starting → running` meta transition carrying `child_pid`, polled within
//! the ack budget. A detached reaper thread `wait()`s the supervisor so a
//! long-lived parent (MCP server) never accumulates zombies; a short-lived
//! CLI simply exits and the supervisor is reparented.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use loglense_core::RunId;
use loglense_store::{create_run_dir, read_meta, write_meta, Meta, Status};

use crate::supervisor::{build_registry, load_config};
use crate::{sys, RunnerError};

/// argv[1] that routes the re-exec'd binary to [`crate::supervisor_main`].
pub const SUPERVISOR_ARG: &str = "__supervisor";

/// Ack budget: supervisor must reach `running` within this window.
const ACK_BUDGET: Duration = Duration::from_millis(200);
/// Ack poll cadence.
const ACK_POLL: Duration = Duration::from_millis(2);

/// Everything needed to start a run.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Wrapped command line, `argv[0]` first.
    pub cmd: Vec<String>,
    /// Working directory for the command.
    pub cwd: PathBuf,
    /// Explicit parser name (`None` or `Some("auto")` means autodetect).
    pub parser: Option<String>,
    /// Supervisor binary to re-exec as `<exe> __supervisor <run-dir>`
    /// (defaults to the current executable; tests point it at
    /// `loglense-runner-supervisor`).
    pub supervisor_exe: Option<PathBuf>,
}

impl SpawnRequest {
    /// Request for `cmd` in `cwd`, parser autodetected, supervisor = the
    /// current executable.
    pub fn new(cmd: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            cmd,
            cwd,
            parser: None,
            supervisor_exe: None,
        }
    }
}

/// Result of a successful spawn.
#[derive(Debug, Clone)]
pub struct SpawnOutcome {
    /// The run's id.
    pub run_id: RunId,
    /// `runs/<id>/` directory.
    pub run_dir: PathBuf,
    /// Pid of the owning supervisor.
    pub supervisor_pid: u32,
    /// Pid of the wrapped child.
    pub child_pid: u32,
}

/// Creates the run, spawns the detached supervisor, and waits for the ack.
/// Returns within the 200 ms budget on success; on failure the meta is
/// finalized as `failed-to-start` and a typed error explains why.
pub fn spawn_run(state: &Path, req: SpawnRequest) -> Result<SpawnOutcome, RunnerError> {
    let config = load_config(state)?;
    let parser = resolve_parser(state, &req, &config)?;
    check_free_space(state, config.retention.min_free_mb)?;
    let (run_id, run_dir) = create_run_dir(state)?;
    // Canonical once at spawn: the supervisor runs from the run directory
    // and every later path (and the argv identity check) uses this form.
    let run_dir = run_dir.canonicalize().map_err(|source| RunnerError::Io {
        path: run_dir.clone(),
        source,
    })?;
    let meta = Meta {
        run_id: run_id.clone(),
        cmd: req.cmd.clone(),
        cwd: req.cwd.clone(),
        supervisor_pid: 0,
        child_pid: None,
        child_starttime: None,
        parser: Some(parser),
        status: Status::Starting,
        exit_code: None,
        signal: None,
        started_at_ms: crate::pump::now_ms(),
        ended_at_ms: None,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: 1,
        on_complete: None,
    };
    write_meta(&run_dir, &meta)?;
    let exe = match req.supervisor_exe.clone() {
        Some(exe) => exe,
        None => std::env::current_exe().map_err(|source| RunnerError::Io {
            path: PathBuf::from("current_exe"),
            source,
        })?,
    };
    let mut command = Command::new(&exe);
    command
        .arg(SUPERVISOR_ARG)
        .arg(&run_dir)
        .current_dir(&run_dir)
        .env("LOGLENSE_RUN_DIR", &run_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut supervisor = sys::new_session(&mut command).spawn().map_err(|source| {
        mark_failed_to_start(&run_dir);
        RunnerError::Io {
            path: exe.clone(),
            source,
        }
    })?;
    match wait_for_ack(&mut supervisor, &run_dir, &run_id) {
        Ok(child_pid) => {
            let supervisor_pid = supervisor.id();
            // Detached reaper: a long-lived parent never zombies supervisors.
            std::thread::spawn(move || {
                let _ = supervisor.wait();
            });
            Ok(SpawnOutcome {
                run_id,
                run_dir,
                supervisor_pid,
                child_pid,
            })
        }
        Err(err) => Err(err),
    }
}

/// Resolves the parser selection: explicit flag > config default > `auto`;
/// an explicit name must be registered (fail fast, before any run dir).
fn resolve_parser(
    state: &Path,
    req: &SpawnRequest,
    config: &loglense_core::Config,
) -> Result<String, RunnerError> {
    let explicit = req
        .parser
        .as_deref()
        .filter(|name| *name != "auto")
        .map(str::to_string)
        .or_else(|| (config.default_parser != "auto").then(|| config.default_parser.clone()));
    let Some(name) = explicit else {
        return Ok("auto".to_string());
    };
    let registry = build_registry(state);
    if !registry
        .names()
        .iter()
        .any(|registered| *registered == name)
    {
        return Err(RunnerError::UnknownParser { name });
    }
    Ok(name)
}

/// Pre-spawn disk floor: fail fast with a typed error instead of starting a
/// run that would truncate on ENOSPC. An unmeasurable filesystem passes
/// (the runtime ENOSPC path still truncates safely).
fn check_free_space(state: &Path, floor_mb: u64) -> Result<(), RunnerError> {
    if floor_mb == 0 {
        return Ok(());
    }
    let Ok(free) = sys::free_bytes(state) else {
        return Ok(());
    };
    let free_mb = free / (1024 * 1024);
    if free_mb < floor_mb {
        return Err(RunnerError::LowDisk {
            path: state.to_path_buf(),
            free_mb,
            floor_mb,
        });
    }
    Ok(())
}

/// Polls the atomic meta transition `starting → running` (the ack) until
/// the budget is spent; a supervisor exit or timeout fails the start.
fn wait_for_ack(
    supervisor: &mut std::process::Child,
    run_dir: &Path,
    run_id: &RunId,
) -> Result<u32, RunnerError> {
    let deadline = Instant::now() + ACK_BUDGET;
    loop {
        if let Ok(Some(status)) = supervisor.try_wait() {
            mark_failed_to_start(run_dir);
            let _ = supervisor.wait();
            return Err(RunnerError::StartFailed {
                run_id: run_id.to_string(),
                reason: format!("supervisor exited before ack: {status}"),
            });
        }
        if let Ok(meta) = read_meta(run_dir) {
            match meta.status {
                Status::Running => {
                    if let Some(child_pid) = meta.child_pid {
                        return Ok(child_pid);
                    }
                }
                Status::FailedToStart => {
                    let _ = supervisor.wait();
                    return Err(RunnerError::StartFailed {
                        run_id: run_id.to_string(),
                        reason: "command failed to spawn".to_string(),
                    });
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            mark_failed_to_start(run_dir);
            return Err(RunnerError::StartFailed {
                run_id: run_id.to_string(),
                reason: "ack timeout: supervisor did not reach running within 200 ms".to_string(),
            });
        }
        std::thread::sleep(ACK_POLL);
    }
}

/// Best-effort `failed-to-start` finalization (idempotent).
fn mark_failed_to_start(run_dir: &Path) {
    if let Ok(mut meta) = read_meta(run_dir) {
        if meta.status != Status::FailedToStart {
            meta.status = Status::FailedToStart;
            meta.ended_at_ms = Some(crate::pump::now_ms());
            let _ = write_meta(run_dir, &meta);
        }
    }
}
