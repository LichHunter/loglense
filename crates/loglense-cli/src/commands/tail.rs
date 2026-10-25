//! `tail`: the last lines of a run's captured output (raw spools).
//!
//! `out` / `err` read one spool. `merged` restores the arrival order across
//! both from `interleave.idx` (the supervisor's stream-switch journal):
//! the newest N lines overall contain, per stream, only a suffix of at most
//! N lines, so one bounded pass per spool plus the journal's last N
//! segments rebuild the merged tail. Runs without a journal fall back to
//! stdout's tail then stderr's. Every scan streams with a per-line cap:
//! memory stays O(N · line cap).

use std::collections::VecDeque;
use std::io::BufRead as _;
use std::path::Path;

use loglense_core::Stream;
use loglense_store::{last_segments, Segment};

use crate::cli::StreamArg;
use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

#[derive(serde::Serialize)]
struct Line {
    stream: &'static str,
    line: u64,
    text: String,
}

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
    let (shown, available) = match stream {
        StreamArg::Out => single(&run_dir, Stream::Out, n)?,
        StreamArg::Err => single(&run_dir, Stream::Err, n)?,
        StreamArg::Merged => merged(&run_dir, n)?,
    };
    let truncated = available > shown.len() as u64;
    if ctx.json {
        output::json(&serde_json::json!({
            "run_id": run_id,
            "lines": shown,
            "truncated": truncated,
        }));
        return Ok(0);
    }
    for line in &shown {
        match stream {
            StreamArg::Merged => println!("{} | {}", line.stream, line.text),
            _ => println!("{}", line.text),
        }
    }
    if truncated {
        eprintln!(
            "({} of {available} lines shown; --lines to change)",
            shown.len()
        );
    }
    Ok(0)
}

/// Last `n` lines of one spool, plus the spool's total line count.
fn single(run_dir: &Path, stream: Stream, n: usize) -> Result<(Vec<Line>, u64), CliError> {
    let mut ring = VecDeque::with_capacity(n.min(4096));
    let total = scan_lines(&spool(run_dir, stream), |line, text| {
        push_bounded(&mut ring, n, (line, text));
    })?;
    let lines = ring
        .into_iter()
        .map(|(line, text)| Line {
            stream: stream_name(stream),
            line,
            text,
        })
        .collect();
    Ok((lines, total))
}

/// Last `n` lines across both spools in arrival order (see module docs).
fn merged(run_dir: &Path, n: usize) -> Result<(Vec<Line>, u64), CliError> {
    let mut tails: [VecDeque<(u64, String)>; 2] = [VecDeque::new(), VecDeque::new()];
    let mut totals = [0u64; 2];
    for stream in [Stream::Out, Stream::Err] {
        let ring = &mut tails[index(stream)];
        totals[index(stream)] = scan_lines(&spool(run_dir, stream), |line, text| {
            push_bounded(ring, n, (line, text));
        })?;
    }
    let available = totals[0] + totals[1];
    let segments = last_segments(run_dir, n)?;
    let order = if segments.is_empty() {
        // No journal: stdout's tail, then stderr's.
        [Stream::Out, Stream::Err]
            .into_iter()
            .flat_map(|stream| {
                tails[index(stream)]
                    .iter()
                    .map(move |(line, _)| (stream, *line))
            })
            .collect::<Vec<_>>()
    } else {
        arrival_order(&segments, totals, n)
    };
    let skip = order.len().saturating_sub(n);
    let lines = order
        .into_iter()
        .skip(skip)
        .filter_map(|(stream, line)| {
            let ring = &tails[index(stream)];
            // Rings hold each stream's last n lines in ascending order.
            let first = ring.front()?.0;
            let (found, text) = ring.get(usize::try_from(line.checked_sub(first)?).ok()?)?;
            (*found == line).then(|| Line {
                stream: stream_name(stream),
                line,
                text: text.clone(),
            })
        })
        .collect();
    Ok((lines, available))
}

/// The newest ≤ `n` `(stream, line)` pairs in arrival order. A segment runs
/// from its start to just before the same stream's next segment, or to the
/// stream's last line for its final segment.
fn arrival_order(segments: &[Segment], totals: [u64; 2], n: usize) -> Vec<(Stream, u64)> {
    let mut next_start = [None::<u64>; 2];
    let mut newest_first = Vec::with_capacity(n.min(4096));
    'segments: for segment in segments.iter().rev() {
        let i = index(segment.stream);
        let end = next_start[i].map_or(totals[i], |next| next.saturating_sub(1));
        next_start[i] = Some(segment.start);
        let mut line = end;
        while line >= segment.start && line > 0 {
            if newest_first.len() == n {
                break 'segments;
            }
            newest_first.push((segment.stream, line));
            line -= 1;
        }
    }
    newest_first.reverse();
    newest_first
}

/// Streams `path` line by line as `(1-based line number, clipped lossy
/// text)`; a missing spool reads as empty. Returns the line count. Each
/// line's stored text is capped, so a newline-free GB spool stays bounded.
fn scan_lines(path: &Path, mut visit: impl FnMut(u64, String)) -> Result<u64, CliError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(CliError::io(path, source)),
    };
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut current: Vec<u8> = Vec::new();
    let mut overflow = 0usize;
    let mut line_no = 0u64;
    let mut emit = |current: &mut Vec<u8>, overflow: &mut usize, line_no: &mut u64| {
        *line_no += 1;
        let mut text = String::from_utf8_lossy(current).into_owned();
        if *overflow > 0 {
            text.push_str(&format!("… [{overflow} more bytes]"));
        }
        visit(*line_no, text);
        current.clear();
        *overflow = 0;
    };
    loop {
        let buf = reader
            .fill_buf()
            .map_err(|source| CliError::io(path, source))?;
        if buf.is_empty() {
            break;
        }
        let (chunk, newline) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (&buf[..at], true),
            None => (buf, false),
        };
        let room = output::MAX_TEXT.saturating_sub(current.len());
        current.extend_from_slice(&chunk[..chunk.len().min(room)]);
        overflow += chunk.len().saturating_sub(room);
        let consumed = chunk.len() + usize::from(newline);
        reader.consume(consumed);
        if newline {
            emit(&mut current, &mut overflow, &mut line_no);
        }
    }
    if !current.is_empty() || overflow > 0 {
        emit(&mut current, &mut overflow, &mut line_no);
    }
    Ok(line_no)
}

fn push_bounded<T>(ring: &mut VecDeque<T>, cap: usize, item: T) {
    if ring.len() == cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

fn spool(run_dir: &Path, stream: Stream) -> std::path::PathBuf {
    run_dir.join(match stream {
        Stream::Out => "stdout.raw",
        Stream::Err => "stderr.raw",
    })
}

fn index(stream: Stream) -> usize {
    match stream {
        Stream::Out => 0,
        Stream::Err => 1,
    }
}

fn stream_name(stream: Stream) -> &'static str {
    match stream {
        Stream::Out => "out",
        Stream::Err => "err",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(stream: Stream, start: u64) -> Segment {
        Segment {
            stream,
            start,
            read_seq: 0,
        }
    }

    #[test]
    fn arrival_order_interleaves_segments_and_caps_newest() {
        // out 1-2, err 1, out 3-4, err 2-3
        let segments = [
            seg(Stream::Out, 1),
            seg(Stream::Err, 1),
            seg(Stream::Out, 3),
            seg(Stream::Err, 2),
        ];
        let all = arrival_order(&segments, [4, 3], 10);
        assert_eq!(
            all,
            [
                (Stream::Out, 1),
                (Stream::Out, 2),
                (Stream::Err, 1),
                (Stream::Out, 3),
                (Stream::Out, 4),
                (Stream::Err, 2),
                (Stream::Err, 3),
            ]
        );
        assert_eq!(
            arrival_order(&segments, [4, 3], 3),
            [(Stream::Out, 4), (Stream::Err, 2), (Stream::Err, 3)]
        );
    }
}
