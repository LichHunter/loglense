//! Runner-internal binary, the stand-in for `loglense` until the CLI (T12)
//! routes the same argv:
//!
//! - `__supervisor <run-dir>` — the detached supervisor entry point,
//!   identical to what `loglense __supervisor <run-dir>` runs.
//! - `spawn <state-dir> <cmd>...` — a CLI-parent stand-in for integration
//!   tests: starts a run, prints `<run-id> <supervisor-pid> <child-pid>`,
//!   and exits, so tests can kill the "CLI" and its whole process group.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some(loglense_runner::SUPERVISOR_ARG) if args.len() == 2 => {
            match loglense_runner::supervisor_main(Path::new(&args[1])) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("loglense-supervisor: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("spawn") if args.len() >= 3 => spawn(&args[1], args[2..].to_vec()),
        _ => {
            eprintln!(
                "usage: loglense-runner-supervisor {} <run-dir>\n       loglense-runner-supervisor spawn <state-dir> <cmd>...",
                loglense_runner::SUPERVISOR_ARG
            );
            ExitCode::from(2)
        }
    }
}

fn spawn(state: &str, cmd: Vec<String>) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    match loglense_runner::spawn_run(
        Path::new(state),
        loglense_runner::SpawnRequest::new(cmd, cwd),
    ) {
        Ok(outcome) => {
            println!(
                "{} {} {}",
                outcome.run_id, outcome.supervisor_pid, outcome.child_pid
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("loglense-runner-supervisor: {err}");
            ExitCode::FAILURE
        }
    }
}
