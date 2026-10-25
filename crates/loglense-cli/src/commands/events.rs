//! `errors` / `warnings`: one severity's events, oldest first, bounded.

use loglense_core::Severity;
use loglense_store::query::{self, EventFilter};

use crate::cli::EventsArgs;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

#[derive(Clone, Copy)]
pub(crate) enum Filter {
    Errors,
    Warnings,
}

impl Filter {
    fn severity(self) -> Severity {
        match self {
            Filter::Errors => Severity::Error,
            Filter::Warnings => Severity::Warning,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Filter::Errors => "errors",
            Filter::Warnings => "warnings",
        }
    }
}

pub(crate) fn events(ctx: &Ctx, args: EventsArgs, filter: Filter) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, &args.run_id)?;
    let max = match args.max {
        Some(max) => usize::try_from(max).unwrap_or(usize::MAX),
        None => context::config(&state)?.output.max_events,
    };
    let selection = EventFilter {
        severity: Some(filter.severity()),
        kind: None,
    };
    let page = query::events(&run_dir, selection, 0, max)?;
    if page.skipped_lines > 0 {
        eprintln!(
            "loglense: warning: skipped {} unreadable line(s) in {}",
            page.skipped_lines,
            run_dir.join("events.jsonl").display()
        );
    }
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": args.run_id,
            "events": page.events,
            "total": page.total,
            "shown": page.shown,
        }));
        return Ok(0);
    }
    if page.events.is_empty() {
        println!("no {} in run {}", filter.noun(), args.run_id);
        return Ok(0);
    }
    for event in &page.events {
        let line = event.lines.first().copied().unwrap_or(0);
        let source = event
            .source
            .as_ref()
            .map(|s| format!(" ({}:{})", s.file, s.line))
            .unwrap_or_default();
        println!(
            "L{line:<6} {}  {}{source}",
            query::stream_name(event.stream),
            event.message
        );
        for frame in &event.frames {
            println!("           {frame}");
        }
    }
    if page.total > page.shown {
        println!(
            "({} of {} {} shown; --max to change)",
            page.shown,
            page.total,
            filter.noun()
        );
    }
    Ok(0)
}
