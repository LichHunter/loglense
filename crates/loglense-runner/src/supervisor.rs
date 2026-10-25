//! The supervisor: owns the child, captures both pipes, parses on write,
//! and finalizes `meta.json` + `summary.json` atomically.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use loglense_core::Config;
use loglense_parsers::{bases, load_pack_dir, tools, Parser, ParserRegistry, Source};
use loglense_store::{read_meta, write_meta, Meta, Status};

use crate::capture::run_capture;
use crate::control::proc_starttime;
use crate::pump::Selection;
use crate::summary::{write_summary, Summary, TemplateEntry};
use crate::{sys, RunnerError};

/// The parser name meaning "detect per stream".
const AUTO: &str = "auto";

/// Supervises the run recorded in `run_dir` until its terminal state:
/// traps TERM/INT (forwarded to the child group by the capture loop),
/// spawns the wrapped command in its own process group, transitions meta
/// `starting → running` (the spawner's ack — done before any config or
/// registry work so the ack budget only covers fork + exec), captures +
/// parses both streams, then writes the summary and the terminal meta
/// atomically, in that order.
///
/// The completion-callback hook (T16, D-011) fires here — after this
/// function's atomic finalization — never before it.
pub fn supervisor_main(run_dir: &Path) -> Result<(), RunnerError> {
    let mut meta = read_meta(run_dir)?;
    let Some(state) = run_dir.parent() else {
        return Err(RunnerError::InvalidRunDir {
            run_dir: run_dir.to_path_buf(),
            reason: "run directory has no parent state directory",
        });
    };
    sys::trap_stop_signals().map_err(|source| RunnerError::Io {
        path: run_dir.to_path_buf(),
        source,
    })?;

    let mut child = Command::new(meta.cmd.first().cloned().unwrap_or_default())
        .args(&meta.cmd[1..])
        .current_dir(&meta.cwd)
        .env("LOGLENSE_RUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| RunnerError::SpawnFailed {
            cmd: meta.cmd.first().cloned().unwrap_or_default(),
            source,
        })
        .inspect_err(|_| mark_failed_to_start(run_dir, &meta))?;
    meta.supervisor_pid = std::process::id();
    meta.child_pid = Some(child.id());
    meta.child_starttime = proc_starttime(child.id());
    meta.status = Status::Running;
    write_meta(run_dir, &meta)?;

    // The spawner already validated config and parser name; a config that
    // turned bad in between degrades to defaults rather than orphaning the
    // now-running child.
    let config = load_config(state).unwrap_or_default();
    let registry = build_registry(state);
    let selection = match meta.parser.as_deref() {
        Some(name) if name != AUTO => Selection::Named(name.to_string()),
        _ => Selection::Auto,
    };
    let (capture, status) = run_capture(&mut child, run_dir, &registry, selection)?;

    let ended_at = crate::pump::now_ms();
    let (final_status, exit_code, signal) = exit_fields(&status);
    meta.status = final_status;
    meta.exit_code = exit_code;
    meta.signal = signal;
    meta.ended_at_ms = Some(ended_at);
    if meta.parser.as_deref() == Some(AUTO) {
        meta.parser = Some(
            capture
                .parser_out
                .clone()
                .or_else(|| capture.parser_err.clone())
                .unwrap_or_else(|| AUTO.to_string()),
        );
    }

    let templates = top_templates(&capture, config.output.max_templates);
    let mut severities = capture.severities;
    let summary = Summary {
        status: final_status,
        exit_code,
        signal,
        duration_ms: ended_at - meta.started_at_ms,
        events: capture.events,
        severities: std::mem::take(&mut severities),
        tests: capture.tests,
        templates,
        truncated: capture.truncated.is_some(),
        truncated_reason: capture.truncated,
    };
    // Summary before the terminal meta: any reader that observes a
    // terminal status is guaranteed to find summary.json.
    write_summary(run_dir, &summary)?;
    write_meta(run_dir, &meta)?;
    // TODO(T16): fire the on_complete callback here — strictly after the
    // atomic meta + summary finalization above (docs/architecture.md §7).
    Ok(())
}

/// Persists `failed-to-start` for a spawn failure (idempotent, best-effort:
/// the caller's `SpawnFailed` error is the primary diagnostic).
fn mark_failed_to_start(run_dir: &Path, meta: &Meta) {
    let mut meta = meta.clone();
    if meta.status == Status::FailedToStart {
        return;
    }
    meta.status = Status::FailedToStart;
    meta.supervisor_pid = std::process::id();
    meta.ended_at_ms = Some(crate::pump::now_ms());
    let _ = write_meta(run_dir, &meta);
}

/// Maps a child exit status onto the persisted status fields. `(None,
/// None)` cannot occur on Linux; it maps to exit code −1 defensively.
fn exit_fields(status: &std::process::ExitStatus) -> (Status, Option<i32>, Option<i32>) {
    if let Some(code) = status.code() {
        (Status::Exited, Some(code), None)
    } else if let Some(signal) = status.signal() {
        (Status::Signaled, None, Some(signal))
    } else {
        (Status::Exited, Some(-1), None)
    }
}

/// Top-N error/warning templates: errors before warnings, then by count
/// (ties broken by id for determinism).
fn top_templates(capture: &crate::pump::Capture, max: usize) -> Vec<TemplateEntry> {
    let mut rows: Vec<_> = capture.hot.iter().collect();
    rows.sort_by(|a, b| {
        severity_rank(a.1.severity)
            .cmp(&severity_rank(b.1.severity))
            .then_with(|| b.1.count.cmp(&a.1.count))
            .then_with(|| a.0.cmp(b.0))
    });
    rows.into_iter()
        .take(max)
        .map(|(id, hot)| TemplateEntry {
            template_id: id.clone(),
            severity: crate::summary::severity_key(hot.severity).to_string(),
            count: hot.count,
            first_seen: hot.first_seen,
            last_seen: hot.last_seen,
            example: hot.example.clone(),
        })
        .collect()
}

fn severity_rank(severity: loglense_core::Severity) -> u8 {
    match severity {
        loglense_core::Severity::Error => 0,
        _ => 1,
    }
}

/// Loads the effective config for a state directory: the project's
/// `<state>/config.toml` over the user's `$HOME/.config/loglense/config.toml`.
pub(crate) fn load_config(state: &Path) -> Result<Config, loglense_core::ConfigError> {
    let user_config = std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".config/loglense/config.toml"))
        .unwrap_or_else(|| PathBuf::from("/.config/loglense/config.toml"));
    Config::load(&state.join("config.toml"), &user_config)
}

/// Builds the parser registry: built-ins first, then user packs, then
/// project packs (docs/parsers.md §3.5); broken packs were already warned
/// and skipped by the loader.
pub(crate) fn build_registry(state: &Path) -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    bases::register(&mut registry);
    tools::register(&mut registry);
    if let Some(home) = std::env::var_os("HOME") {
        let dir = Path::new(&home).join(".config/loglense/formats.d");
        register_packs(&mut registry, &dir, Source::UserPack);
    }
    register_packs(&mut registry, &state.join("formats.d"), Source::ProjectPack);
    registry
}

fn register_packs(registry: &mut ParserRegistry, dir: &Path, source: Source) {
    for pack in load_pack_dir(dir)
        .into_iter()
        .filter_map(|(_, result)| result.ok())
    {
        let name = pack.name().to_string();
        let extends = pack.extends().map(str::to_string);
        let pack = Arc::new(pack);
        registry.register(&name, move || Box::new((*pack).clone()), source, extends);
    }
}
