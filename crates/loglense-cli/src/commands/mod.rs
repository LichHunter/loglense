//! Subcommand implementations. Each returns the process exit code on
//! success; errors map through [`CliError::exit_code`].

mod events;
mod gc;
mod init;
mod kill;
mod ls;
mod run;
mod show;
mod tail;
mod wait;

use crate::cli::{Cli, Command};
use crate::context::Ctx;
use crate::error::CliError;

pub(crate) fn execute(cli: Cli) -> Result<u8, CliError> {
    let ctx = Ctx::new(cli.json, cli.dir);
    match cli.command {
        Command::Run(args) => run::run(&ctx, args),
        Command::Init { force } => init::init(&ctx, force),
        Command::Ls { max, status } => ls::ls(&ctx, max, status),
        Command::Show { run_id, max } => show::show(&ctx, &run_id, max),
        Command::Errors(args) => events::events(&ctx, args, events::Filter::Errors),
        Command::Warnings(args) => events::events(&ctx, args, events::Filter::Warnings),
        Command::Tail {
            run_id,
            stream,
            lines,
            follow,
        } => {
            if follow {
                return Err(CliError::Usage(
                    "`tail --follow` is not supported in v1; use `loglense wait <run-id>` then `tail`, or poll `tail`".to_string(),
                ));
            }
            tail::tail(&ctx, &run_id, stream, lines)
        }
        Command::Wait { run_id, timeout } => wait::wait(&ctx, &run_id, timeout),
        Command::Kill { run_id } => kill::kill(&ctx, &run_id),
        Command::Gc { max_age, max_total } => gc::gc(&ctx, max_age, max_total),
        Command::Parsers(_) => Err(CliError::NotImplemented("parsers")),
        Command::Feedback(_) => Err(CliError::NotImplemented("feedback")),
        Command::Notify(_) => Err(CliError::NotImplemented("notify")),
        Command::Mcp(_) => Err(CliError::NotImplemented("mcp")),
        Command::Version => {
            println!("loglense {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
    }
}

/// Reads `summary.json` as loose JSON (absent while the run is live).
/// Unparseable summaries warn and read as absent — never a hard failure.
pub(crate) fn read_summary(run_dir: &std::path::Path) -> Option<serde_json::Value> {
    let path = run_dir.join("summary.json");
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "loglense: warning: ignoring unreadable {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// `(errors, warnings)` from a summary's severity counts.
pub(crate) fn error_warning_counts(summary: &serde_json::Value) -> (u64, u64) {
    let count = |key: &str| summary["severities"][key].as_u64().unwrap_or(0);
    (count("error"), count("warning"))
}
