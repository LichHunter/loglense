//! `tail`: the last lines of a run's captured output (raw spools);
//! `merged` interleaves both streams in arrival order (D-017).

use loglense_store::query::{self, LogStream};

use crate::cli::StreamArg;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

pub(crate) fn tail(
    ctx: &Ctx,
    run_id: &str,
    stream: StreamArg,
    lines: Option<u64>,
) -> Result<u8, CliError> {
    let state = ctx.reader_state()?;
    let run_dir = context::run_dir(&state, run_id)?;
    let n = match lines {
        Some(n) => usize::try_from(n).unwrap_or(usize::MAX),
        None => context::config(&state)?.output.max_tail_lines,
    };
    let selected = match stream {
        StreamArg::Out => LogStream::Out,
        StreamArg::Err => LogStream::Err,
        StreamArg::Merged => LogStream::Merged,
    };
    let page = query::tail(&run_dir, selected, n)?;
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": run_id,
            "lines": page.lines,
            "truncated": page.truncated,
        }));
        return Ok(0);
    }
    for line in &page.lines {
        match selected {
            LogStream::Merged => println!("{} | {}", line.stream, line.text),
            _ => println!("{}", line.text),
        }
    }
    if page.truncated {
        eprintln!(
            "({} of {} lines shown; --lines to change)",
            page.lines.len(),
            page.available
        );
    }
    Ok(0)
}
