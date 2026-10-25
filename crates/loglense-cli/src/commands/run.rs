//! `run` / prefix form: start the command detached and return at once.

use loglense_runner::{spawn_run, SpawnRequest};

use crate::cli::RunArgs;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn run(ctx: &Ctx, args: RunArgs) -> Result<u8, CliError> {
    let state = ctx.writer_state()?;
    let mut request = SpawnRequest::new(args.cmd, context::current_dir()?);
    request.parser = args.parser;
    request.tool_version = Some(env!("LOGLENSE_VERSION").to_string());
    request.on_complete = args.on_complete.map(|command| loglense_store::OnComplete {
        command,
        timeout_ms: args.on_complete_timeout,
    });
    let started = spawn_run(&state, request)?;
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": started.run_id.as_str(),
            "pid": started.child_pid,
            "supervisor_pid": started.supervisor_pid,
            "run_dir": started.run_dir,
        }));
    } else {
        println!("run {} started (pid {})", started.run_id, started.child_pid);
        println!("{}", started.run_dir.display());
    }
    Ok(0)
}
