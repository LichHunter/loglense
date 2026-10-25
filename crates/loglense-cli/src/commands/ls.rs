//! `ls`: runs newest first, bounded; corrupt runs warn and are skipped.

use loglense_store::query::{self, StatusFilter};

use crate::cli::StatusArg;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn ls(ctx: &Ctx, max: u64, status: Option<StatusArg>) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    let list = query::list(&state, status.map(filter), limit, context::now_ms());
    for id in &list.corrupt {
        eprintln!("loglense: warning: skipping corrupt run {id} (unreadable meta.json)");
    }
    if ctx.json {
        output::json(&list);
        return Ok(0);
    }
    if list.runs.is_empty() {
        println!("no runs");
        return Ok(0);
    }
    println!(
        "{:<16} {:>4}  {:<22} {:>4} {:>4}  CMD",
        "RUN", "AGE", "STATUS", "E", "W"
    );
    for row in &list.runs {
        let status = match (row.derived, row.exit_code) {
            (Some(derived), _) => format!("interrupted ({derived})"),
            (None, Some(code)) => format!("{} {code}", row.status),
            (None, None) => row.status.to_string(),
        };
        let (e, w) = row.counts.map_or(("-".into(), "-".into()), |c| {
            (c.error.to_string(), c.warning.to_string())
        });
        println!(
            "{:<16} {:>4}  {:<22} {:>4} {:>4}  {}",
            row.run_id,
            output::age(row.age_s * 1000),
            status,
            e,
            w,
            row.cmd_head
        );
    }
    if list.total > list.shown {
        println!(
            "({} of {} runs shown; --max to change)",
            list.shown, list.total
        );
    }
    Ok(0)
}

fn filter(arg: StatusArg) -> StatusFilter {
    match arg {
        StatusArg::Starting => StatusFilter::Starting,
        StatusArg::Running => StatusFilter::Running,
        StatusArg::Exited => StatusFilter::Exited,
        StatusArg::Signaled => StatusFilter::Signaled,
        StatusArg::FailedToStart => StatusFilter::FailedToStart,
        StatusArg::Interrupted => StatusFilter::Interrupted,
        StatusArg::Orphaned => StatusFilter::Orphaned,
        StatusArg::Lost => StatusFilter::Lost,
    }
}
