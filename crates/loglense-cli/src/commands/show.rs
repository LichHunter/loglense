//! `show`: one run's meta, summary and top error/warning templates.

use loglense_runner::check_terminal;
use loglense_store::read_meta;

use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn show(ctx: &Ctx, run_id: &str, max: Option<u64>) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let meta = read_meta(&run_dir).map_err(|err| CliError::CorruptRun {
        id: run_id.to_string(),
        reason: err.to_string(),
    })?;
    let max = match max {
        Some(max) => usize::try_from(max).unwrap_or(usize::MAX),
        None => context::config(&state)?.output.max_templates,
    };
    let lifecycle = check_terminal(&run_dir);
    let mut summary = super::read_summary(&run_dir);
    let all_templates = summary
        .as_mut()
        .and_then(|s| s.as_object_mut())
        .and_then(|s| s.remove("templates"))
        .and_then(|t| match t {
            serde_json::Value::Array(rows) => Some(rows),
            _ => None,
        })
        .unwrap_or_default();
    let total_templates = all_templates.len();
    let templates: Vec<serde_json::Value> = all_templates
        .into_iter()
        .take(max)
        .map(|mut row| {
            if let Some(example) = row["example"].as_str() {
                row["example"] = output::clip(example).into();
            }
            row
        })
        .collect();

    if ctx.json {
        output::json(&serde_json::json!({
            "meta": meta,
            "state": output::lifecycle_name(lifecycle),
            "summary": summary,
            "templates": templates,
        }));
        return Ok(0);
    }

    let state_label = output::lifecycle_name(lifecycle);
    let outcome = match (meta.exit_code, meta.signal) {
        (Some(code), _) => format!("{state_label} {code}"),
        (None, Some(signal)) => format!("{state_label} (signal {signal})"),
        _ => state_label.to_string(),
    };
    let elapsed = meta.ended_at_ms.unwrap_or_else(context::now_ms) - meta.started_at_ms;
    println!(
        "run {}  {outcome}  ({})",
        meta.run_id,
        output::duration(elapsed)
    );
    println!("cmd:    {}", output::clip(&meta.cmd.join(" ")));
    println!("cwd:    {}", meta.cwd.display());
    if let Some(parser) = &meta.parser {
        println!("parser: {parser}");
    }
    let Some(summary) = summary else {
        println!(
            "summary: not available yet (run is {state_label}); see `loglense tail {}`",
            meta.run_id
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
    if templates.is_empty() {
        println!("no error or warning templates");
        return Ok(0);
    }
    println!(
        "top error/warning templates ({} of {total_templates}):",
        templates.len()
    );
    for row in &templates {
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
