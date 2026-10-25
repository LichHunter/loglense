//! loglense command-line interface (docs/surfaces.md §1–§3).
//!
//! `loglense <cmd...>` is the time-style prefix form; argv is rewritten to
//! the explicit `run -- <cmd...>` before clap sees it ([`dispatch`]). The
//! hidden `__supervisor <run-dir>` route is the detached supervisor the
//! runner re-execs (D-016) and never reaches clap.

mod cli;
mod commands;
mod context;
mod dispatch;
mod error;
mod output;

use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os() {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(raw) => {
                eprintln!("loglense: argument is not valid UTF-8: {raw:?}");
                return ExitCode::from(error::EXIT_USAGE);
            }
        }
    }
    if args.get(1).map(String::as_str) == Some(loglense_runner::SUPERVISOR_ARG) {
        return supervise(&args);
    }
    let cli = match cli::Cli::try_parse_from(dispatch::rewrite(args)) {
        Ok(cli) => cli,
        Err(err) => {
            let code = if err.use_stderr() {
                error::EXIT_USAGE
            } else {
                0
            };
            let _ = err.print();
            return ExitCode::from(code);
        }
    };
    match commands::execute(cli) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("loglense: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

/// `loglense __supervisor <run-dir>`: own one run until its terminal state.
/// stdout/stderr are /dev/null here, so failures are only visible through
/// the run's meta (failed-to-start / lost).
fn supervise(args: &[String]) -> ExitCode {
    let [_, _, run_dir] = args else {
        return ExitCode::from(error::EXIT_USAGE);
    };
    match loglense_runner::supervisor_main(std::path::Path::new(run_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("loglense-supervisor: {err}");
            ExitCode::FAILURE
        }
    }
}
