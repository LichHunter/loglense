//! `gc`: retention pass (flock-serialized, live runs untouched).

use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn gc(ctx: &Ctx, max_age: Option<u64>, max_total: Option<u64>) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let retention = context::config(&state)?.retention;
    let max_age = max_age.unwrap_or(retention.max_age_days);
    let max_total = max_total.unwrap_or(retention.max_total_mb);
    let report = loglense_store::gc(&state, max_age, max_total, context::now_ms())?;
    if ctx.json {
        output::json(&serde_json::json!({
            "runs_removed": report.runs_removed,
            "bytes_freed": report.bytes_freed,
            "max_age_days": max_age,
            "max_total_mb": max_total,
        }));
    } else {
        println!(
            "removed {} run(s), freed {:.1} MB (max age {max_age} d, max total {max_total} MB)",
            report.runs_removed,
            report.bytes_freed as f64 / 1_000_000.0
        );
    }
    Ok(0)
}
