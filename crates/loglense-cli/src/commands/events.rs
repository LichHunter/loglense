//! `errors` / `warnings`: one severity's events, oldest first. Streams
//! `events.jsonl` (memory holds only the shown events) and counts the rest.

use loglense_core::{Event, Severity, Stream};
use loglense_store::scan_events;

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
    let mut shown: Vec<Event> = Vec::new();
    let mut total = 0usize;
    let read = scan_events(&run_dir.join("events.jsonl"), |event| {
        if event.severity != filter.severity() {
            return;
        }
        total += 1;
        if shown.len() < max {
            shown.push(bounded(event));
        }
    })?;
    if read.skipped > 0 {
        eprintln!(
            "loglense: warning: skipped {} unreadable line(s) in {}",
            read.skipped,
            run_dir.join("events.jsonl").display()
        );
    }
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": args.run_id,
            "events": shown,
            "total": total,
            "shown": shown.len(),
        }));
        return Ok(0);
    }
    if shown.is_empty() {
        println!("no {} in run {}", filter.noun(), args.run_id);
        return Ok(0);
    }
    for event in &shown {
        let stream = match event.stream {
            Stream::Out => "out",
            Stream::Err => "err",
        };
        let line = event.lines.first().copied().unwrap_or(0);
        let source = event
            .source
            .as_ref()
            .map(|s| format!(" ({}:{})", s.file, s.line))
            .unwrap_or_default();
        println!("L{line:<6} {stream}  {}{source}", event.message);
        for frame in &event.frames {
            println!("           {frame}");
        }
    }
    if total > shown.len() {
        println!(
            "({} of {total} {} shown; --max to change)",
            shown.len(),
            filter.noun()
        );
    }
    Ok(0)
}

/// Caps one event's message and frame list for output.
fn bounded(mut event: Event) -> Event {
    event.message = output::clip(&event.message);
    if event.frames.len() > output::MAX_FRAMES {
        let more = event.frames.len() - output::MAX_FRAMES;
        event.frames.truncate(output::MAX_FRAMES);
        event.frames.push(format!("… {more} more frames"));
    }
    event
}
