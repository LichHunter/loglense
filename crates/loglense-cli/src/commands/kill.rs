//! `kill`: terminate a run behind the liveness and pid-reuse guards.

use loglense_runner::{kill_run, KillAction};

use crate::context::{self, Ctx};
use crate::error::{CliError, EXIT_USAGE};
use crate::output;

pub(crate) fn kill(ctx: &Ctx, run_id: &str) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let outcome = kill_run(&run_dir);
    let action = match outcome.action {
        KillAction::Signaled => "signaled",
        KillAction::AlreadyTerminal => "already-terminal",
        KillAction::Refused => "refused",
    };
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": run_id,
            "action": action,
            "status": outcome.lifecycle.as_str(),
            "detail": outcome.detail,
        }));
    } else {
        println!("run {run_id}: {action}: {}", outcome.detail);
    }
    // A refusal means the run's records cannot be acted on safely (lost,
    // pid reuse, corrupt): the unusable-run code, never the run-outcome 1.
    Ok(if outcome.action == KillAction::Refused {
        EXIT_USAGE
    } else {
        0
    })
}
