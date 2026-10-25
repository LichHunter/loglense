//! Subcommand implementations. Each returns the process exit code on
//! success; errors map through [`CliError::exit_code`].

mod events;
mod feedback;
mod gc;
mod init;
mod kill;
mod ls;
mod parsers;
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
        Command::Parsers(cmd) => parsers::parsers(&ctx, cmd),
        Command::Feedback(cmd) => feedback::feedback(&ctx, cmd),
        Command::Notify(_) => Err(CliError::NotImplemented("notify")),
        Command::Mcp => {
            loglense_mcp::serve_stdio().map_err(|err| CliError::Mcp(err.to_string()))?;
            Ok(0)
        }
        Command::Version => {
            println!("loglense {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
    }
}
