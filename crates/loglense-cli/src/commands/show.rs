//! `show`: one run's meta, summary and top error/warning templates.

use loglense_store::query;

use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn show(ctx: &Ctx, run_id: &str, max: Option<u64>) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let max = match max {
        Some(max) => usize::try_from(max).unwrap_or(usize::MAX),
        None => context::config(&state)?.output.max_templates,
    };
    let detail = query::run_detail(&run_dir, max).map_err(|err| CliError::CorruptRun {
        id: run_id.to_string(),
        reason: err.to_string(),
    })?;
    if ctx.json {
        output::json(&detail);
        return Ok(0);
    }

    let meta = &detail.meta;
    let outcome = match (meta.exit_code, meta.signal) {
        (Some(code), _) => format!("{} {code}", detail.state),
        (None, Some(signal)) => format!("{} (signal {signal})", detail.state),
        _ => detail.state.to_string(),
    };
    let elapsed = meta.ended_at_ms.unwrap_or_else(context::now_ms) - meta.started_at_ms;
    println!(
        "run {}  {outcome}  ({})",
        meta.run_id,
        output::duration(elapsed)
    );
    println!("cmd:    {}", query::clip(&meta.cmd.join(" ")));
    println!("cwd:    {}", meta.cwd.display());
    if let Some(parser) = &meta.parser {
        println!("parser: {parser}");
    }
    let Some(summary) = &detail.summary else {
        println!(
            "summary: not available yet (run is {}); see `loglense tail {}`",
            detail.state, meta.run_id
        );
        return Ok(0);
    };
    println!(
        "events: {}{}",
        summary["events"].as_u64().unwrap_or(0),
        counts_suffix(&summary["severities"])
    );
    if let Some(tests) = summary["tests"].as_object().filter(|t| !t.is_empty()) {
        let parts: Vec<String> = tests
            .iter()
            .map(|(outcome, n)| format!("{outcome} {}", n.as_u64().unwrap_or(0)))
            .collect();
        println!("tests:  {}", parts.join(", "));
    }
    if summary["truncated"].as_bool() == Some(true) {
        println!(
            "note:   event capture truncated: {}",
            summary["truncated_reason"]
                .as_str()
                .unwrap_or("unknown reason")
        );
    }
    if detail.templates.is_empty() {
        println!("no error or warning templates");
        return Ok(0);
    }
    println!(
        "top error/warning templates ({} of {}):",
        detail.templates.len(),
        detail.total_templates
    );
    for row in &detail.templates {
        println!(
            "  {:>5}x {:<7} L{}-{}  {}",
            row["count"].as_u64().unwrap_or(0),
            row["severity"].as_str().unwrap_or("?"),
            row["first_seen"].as_u64().unwrap_or(0),
            row["last_seen"].as_u64().unwrap_or(0),
            row["example"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

/// ` (error 3, warning 1, info 8)` in severity order, empty when none.
fn counts_suffix(severities: &serde_json::Value) -> String {
    let parts: Vec<String> = ["error", "warning", "notice", "info", "debug"]
        .iter()
        .filter_map(|key| {
            severities[key]
                .as_u64()
                .filter(|n| *n > 0)
                .map(|n| format!("{key} {n}"))
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}
