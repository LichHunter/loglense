//! `wait`: block until terminal, derived-dead (fast-fail), or timeout; the
//! exit code mirrors the run outcome (docs/surfaces.md §3).

use std::time::Duration;

use loglense_runner::{wait_run, Lifecycle};
use loglense_store::Status;

use crate::context::{self, Ctx};
use crate::error::{CliError, EXIT_RUN_FAILED, EXIT_TIMEOUT, EXIT_USAGE};
use crate::output;

/// CLI meta poll cadence.
const POLL: Duration = Duration::from_millis(250);

pub(crate) fn wait(ctx: &Ctx, run_id: &str, timeout: Option<f64>) -> Result<u8, CliError> {
    let timeout = match timeout {
        Some(secs) if secs.is_finite() && secs >= 0.0 => Some(Duration::from_secs_f64(secs)),
        Some(secs) => {
            return Err(CliError::Usage(format!(
                "--timeout must be a non-negative number of seconds, got {secs}"
            )))
        }
        None => None,
    };
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let outcome = wait_run(&run_dir, timeout, POLL);
    let status = outcome.lifecycle.as_str();
    let code = match outcome.lifecycle {
        _ if outcome.timed_out => EXIT_TIMEOUT,
        Lifecycle::Terminal(Status::Exited) if outcome.exit_code == Some(0) => 0,
        Lifecycle::Corrupt => EXIT_USAGE,
        _ => EXIT_RUN_FAILED,
    };
    if ctx.json {
        let mut body = serde_json::json!({
            "run_id": run_id,
            "status": status,
            "timed_out": outcome.timed_out,
        });
        if let Some(exit_code) = outcome.exit_code {
            body["exit_code"] = exit_code.into();
        }
        output::json(&body);
    } else if outcome.timed_out {
        println!("run {run_id} still running (timed out)");
    } else {
        match (outcome.lifecycle, outcome.exit_code) {
            (Lifecycle::Orphaned | Lifecycle::Lost, _) => println!(
                "run {run_id} interrupted ({status}): its supervisor died; see `loglense show {run_id}`"
            ),
            (_, Some(exit_code)) => println!("run {run_id} {status} {exit_code}"),
            _ => println!("run {run_id} {status}"),
        }
    }
    Ok(code)
}
