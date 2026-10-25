//! `ls`: runs newest first, bounded; corrupt runs warn and are skipped.

use loglense_store::{list_runs, DerivedState, RunSummary, Status};

use crate::cli::StatusFilter;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

#[derive(serde::Serialize)]
struct Row {
    run_id: String,
    /// Persisted status, or `interrupted` when a derived substate applies.
    status: &'static str,
    /// `orphaned` / `lost` when the supervisor died; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    derived: Option<&'static str>,
    cmd_head: String,
    age_s: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
    /// Severity counts from the summary; absent until the run finishes.
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Counts>,
}

#[derive(serde::Serialize)]
struct Counts {
    error: u64,
    warning: u64,
}

pub(crate) fn ls(ctx: &Ctx, max: u64, filter: Option<StatusFilter>) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let now = context::now_ms();
    let mut rows = Vec::new();
    let mut matched = 0usize;
    for run in list_runs(&state) {
        let (Some(status), Some(started)) = (run.status, run.started_at_ms) else {
            eprintln!(
                "loglense: warning: skipping corrupt run {} (unreadable meta.json)",
                run.run_id
            );
            continue;
        };
        if !filter.is_none_or(|f| matches(f, status, run.derived)) {
            continue;
        }
        matched += 1;
        if rows.len() as u64 >= max {
            continue;
        }
        rows.push(row(&state, &run, status, now - started));
    }
    if ctx.json {
        output::json(&serde_json::json!({
            "runs": rows,
            "total": matched,
            "shown": rows.len(),
        }));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("no runs");
        return Ok(0);
    }
    println!(
        "{:<16} {:>4}  {:<22} {:>4} {:>4}  CMD",
        "RUN", "AGE", "STATUS", "E", "W"
    );
    for row in &rows {
        let status = match (row.derived, row.exit_code) {
            (Some(derived), _) => format!("interrupted ({derived})"),
            (None, Some(code)) => format!("{} {code}", row.status),
            (None, None) => row.status.to_string(),
        };
        let (e, w) = row.counts.as_ref().map_or(("-".into(), "-".into()), |c| {
            (c.error.to_string(), c.warning.to_string())
        });
        println!(
            "{:<16} {:>4}  {:<22} {:>4} {:>4}  {}",
            row.run_id,
            output::age(row.age_s * 1000),
            status,
            e,
            w,
            output::clip(&row.cmd_head)
        );
    }
    if matched > rows.len() {
        println!("({} of {matched} runs shown; --max to change)", rows.len());
    }
    Ok(0)
}

fn row(state: &std::path::Path, run: &RunSummary, status: Status, age_ms: i64) -> Row {
    let summary = super::read_summary(&state.join(loglense_store::RUNS_DIR).join(&run.run_id));
    let derived = run.derived.and_then(derived_name);
    Row {
        run_id: run.run_id.clone(),
        status: if derived.is_some() {
            "interrupted"
        } else {
            output::status_name(status)
        },
        derived,
        cmd_head: run.cmd_head.clone(),
        age_s: age_ms / 1000,
        exit_code: summary.as_ref().and_then(|s| s["exit_code"].as_i64()),
        counts: summary.as_ref().map(|s| {
            let (error, warning) = super::error_warning_counts(s);
            Counts { error, warning }
        }),
    }
}

fn derived_name(derived: DerivedState) -> Option<&'static str> {
    match derived {
        DerivedState::Orphaned => Some("orphaned"),
        DerivedState::Lost => Some("lost"),
        DerivedState::Corrupt => None,
    }
}

fn matches(filter: StatusFilter, status: Status, derived: Option<DerivedState>) -> bool {
    let derived = derived.and_then(derived_name);
    match filter {
        StatusFilter::Interrupted => derived.is_some(),
        StatusFilter::Orphaned => derived == Some("orphaned"),
        StatusFilter::Lost => derived == Some("lost"),
        _ if derived.is_some() => false,
        StatusFilter::Starting => status == Status::Starting,
        StatusFilter::Running => status == Status::Running,
        StatusFilter::Exited => status == Status::Exited,
        StatusFilter::Signaled => status == Status::Signaled,
        StatusFilter::FailedToStart => status == Status::FailedToStart,
    }
}
