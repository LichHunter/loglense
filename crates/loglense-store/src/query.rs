//! Bounded read queries shared by every surface (CLI, MCP): run listing
//! rows, run detail, filtered event pages, and log tails. Each query caps
//! its item count and every item's size, so no surface can return an
//! unbounded payload (docs/surfaces.md §1.3, NFR-05).

use std::collections::VecDeque;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use loglense_core::{Event, Kind, Severity, Stream};
use serde_json::Value;

use crate::interleave::{last_segments, Segment};
use crate::listing::{list_runs, summarize_run, DerivedState, RunSummary};
use crate::{read_meta, scan_events, Meta, Status, StoreError, RUNS_DIR};

/// Longest message / line any query returns (bytes, cut on a char
/// boundary and marked): one pathological 1 MB line cannot flood a reader.
pub const MAX_TEXT: usize = 4096;
/// Most stack frames returned per event.
pub const MAX_FRAMES: usize = 20;

/// `text` capped at [`MAX_TEXT`] bytes with a marker naming what was cut.
pub fn clip(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_string();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} more bytes]", &text[..end], text.len() - end)
}

/// `summary.json` as loose JSON: absent while the run is live, and an
/// unparseable file reads as absent (never a hard failure).
pub fn read_summary(run_dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(run_dir.join("summary.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// `(errors, warnings)` from a summary's severity counts.
fn error_warning_counts(summary: &Value) -> (u64, u64) {
    let count = |key: &str| summary["severities"][key].as_u64().unwrap_or(0);
    (count("error"), count("warning"))
}

/// Run state as one label: the persisted status, or `orphaned` / `lost`
/// when the supervisor died (derived on read, never persisted).
pub fn state_label(status: Status, derived: Option<DerivedState>) -> &'static str {
    match derived {
        Some(DerivedState::Orphaned) => "orphaned",
        Some(DerivedState::Lost) => "lost",
        Some(DerivedState::Corrupt) => "corrupt",
        None => status.as_str(),
    }
}

// ---------------------------------------------------------------- listing

/// Status families a listing can filter on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    /// Persisted `starting` with a live supervisor.
    Starting,
    /// Persisted `running` with a live supervisor.
    Running,
    /// Child exited with a code.
    Exited,
    /// Child terminated by a signal.
    Signaled,
    /// Never started.
    FailedToStart,
    /// Orphaned or lost.
    Interrupted,
    /// Supervisor dead, child alive.
    Orphaned,
    /// Supervisor and child dead.
    Lost,
}

impl StatusFilter {
    fn matches(self, status: Status, derived: Option<DerivedState>) -> bool {
        match (self, derived) {
            (StatusFilter::Interrupted, d) => d.is_some(),
            (StatusFilter::Orphaned, d) => d == Some(DerivedState::Orphaned),
            (StatusFilter::Lost, d) => d == Some(DerivedState::Lost),
            (_, Some(_)) => false,
            (StatusFilter::Starting, None) => status == Status::Starting,
            (StatusFilter::Running, None) => status == Status::Running,
            (StatusFilter::Exited, None) => status == Status::Exited,
            (StatusFilter::Signaled, None) => status == Status::Signaled,
            (StatusFilter::FailedToStart, None) => status == Status::FailedToStart,
        }
    }
}

/// Error / warning counts of a finished run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    /// Error events.
    pub error: u64,
    /// Warning events.
    pub warning: u64,
}

/// One listing row.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunRow {
    /// Run id.
    pub run_id: String,
    /// Persisted status, or `interrupted` when a derived substate applies.
    pub status: &'static str,
    /// `orphaned` / `lost` when the supervisor died.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived: Option<&'static str>,
    /// First token of the wrapped command (clipped).
    pub cmd_head: String,
    /// Seconds since the run started.
    pub age_s: i64,
    /// Child exit code once the run exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Severity counts once the summary exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Counts>,
}

/// A bounded run listing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunList {
    /// Newest-first rows, at most the requested limit.
    pub runs: Vec<RunRow>,
    /// Rows matching the filter before the limit.
    pub total: usize,
    /// `runs.len()`.
    pub shown: usize,
    /// Run directories skipped because their meta is unreadable.
    #[serde(skip)]
    pub corrupt: Vec<String>,
}

/// Lists runs newest first: filtered, capped at `limit`, corrupt runs set
/// aside in [`RunList::corrupt`] for the caller to warn about.
pub fn list(state: &Path, filter: Option<StatusFilter>, limit: usize, now_ms: i64) -> RunList {
    let mut list = RunList {
        runs: Vec::new(),
        total: 0,
        shown: 0,
        corrupt: Vec::new(),
    };
    for run in list_runs(state) {
        let (Some(status), Some(started)) = (run.status, run.started_at_ms) else {
            list.corrupt.push(run.run_id);
            continue;
        };
        if !filter.is_none_or(|f| f.matches(status, run.derived)) {
            continue;
        }
        list.total += 1;
        if list.runs.len() < limit {
            list.runs.push(row(state, &run, status, now_ms - started));
        }
    }
    list.shown = list.runs.len();
    list
}

fn row(state: &Path, run: &RunSummary, status: Status, age_ms: i64) -> RunRow {
    let summary = read_summary(&state.join(RUNS_DIR).join(&run.run_id));
    let derived = match run.derived {
        Some(DerivedState::Orphaned) => Some("orphaned"),
        Some(DerivedState::Lost) => Some("lost"),
        _ => None,
    };
    RunRow {
        run_id: run.run_id.clone(),
        status: if derived.is_some() {
            "interrupted"
        } else {
            status.as_str()
        },
        derived,
        cmd_head: clip(&run.cmd_head),
        age_s: age_ms / 1000,
        exit_code: summary.as_ref().and_then(|s| s["exit_code"].as_i64()),
        counts: summary.as_ref().map(|s| {
            let (error, warning) = error_warning_counts(s);
            Counts { error, warning }
        }),
    }
}

// ----------------------------------------------------------------- detail

/// One run in full: meta, state, summary and the top templates.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunDetail {
    /// The run record.
    pub meta: Meta,
    /// [`state_label`] of the run.
    pub state: &'static str,
    /// Terminal summary without its template list (absent while live).
    pub summary: Option<Value>,
    /// Top error/warning templates (errors first), at most the requested
    /// count, examples clipped.
    pub templates: Vec<Value>,
    /// Templates the summary holds before the cap.
    #[serde(skip)]
    pub total_templates: usize,
}

/// Reads one run's detail; an unreadable meta is an error (corrupt run).
pub fn run_detail(run_dir: &Path, max_templates: usize) -> Result<RunDetail, StoreError> {
    let meta = read_meta(run_dir)?;
    let derived = summarize_run(run_dir).derived;
    let mut summary = read_summary(run_dir);
    let all: Vec<Value> = summary
        .as_mut()
        .and_then(Value::as_object_mut)
        .and_then(|s| s.remove("templates"))
        .and_then(|t| match t {
            Value::Array(rows) => Some(rows),
            _ => None,
        })
        .unwrap_or_default();
    let total_templates = all.len();
    let templates = all
        .into_iter()
        .take(max_templates)
        .map(|mut row| {
            if let Some(example) = row["example"].as_str() {
                row["example"] = clip(example).into();
            }
            row
        })
        .collect();
    Ok(RunDetail {
        state: state_label(meta.status, derived),
        meta,
        summary,
        templates,
        total_templates,
    })
}

// ----------------------------------------------------------------- events

/// Which events a page selects; `None` fields match everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Only this severity.
    pub severity: Option<Severity>,
    /// Only this kind.
    pub kind: Option<Kind>,
}

/// A bounded page of matching events in file (emission) order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EventPage {
    /// The page, messages clipped, frames capped.
    pub events: Vec<Event>,
    /// All matching events (unpaginated).
    pub total: usize,
    /// `events.len()`.
    pub shown: usize,
    /// Malformed or torn lines skipped while scanning.
    #[serde(skip)]
    pub skipped_lines: usize,
}

/// Streams `events.jsonl`, skipping `offset` matches and keeping up to
/// `limit`; memory holds only the page.
pub fn events(
    run_dir: &Path,
    filter: EventFilter,
    offset: usize,
    limit: usize,
) -> Result<EventPage, StoreError> {
    let mut events = Vec::new();
    let mut total = 0usize;
    let read = scan_events(&run_dir.join("events.jsonl"), |event| {
        if filter.severity.is_some_and(|s| s != event.severity)
            || filter.kind.is_some_and(|k| k != event.kind)
        {
            return;
        }
        total += 1;
        if total > offset && events.len() < limit {
            events.push(bounded(event));
        }
    })?;
    Ok(EventPage {
        shown: events.len(),
        events,
        total,
        skipped_lines: read.skipped,
    })
}

fn bounded(mut event: Event) -> Event {
    event.message = clip(&event.message);
    if event.frames.len() > MAX_FRAMES {
        let more = event.frames.len() - MAX_FRAMES;
        event.frames.truncate(MAX_FRAMES);
        event.frames.push(format!("… {more} more frames"));
    }
    event
}

// -------------------------------------------------------------------- log

/// Which captured output a log query reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    /// `stdout.raw`.
    Out,
    /// `stderr.raw`.
    Err,
    /// Both, in arrival order (`interleave.idx`, D-017).
    Merged,
}

/// One numbered raw line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LogLine {
    /// `out` or `err`.
    pub stream: &'static str,
    /// 1-based raw line number within its stream.
    pub line: u64,
    /// Lossy-decoded text, clipped.
    pub text: String,
}

/// A bounded slice of captured output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LogPage {
    /// The lines.
    pub lines: Vec<LogLine>,
    /// Whether lines outside the slice exist.
    pub truncated: bool,
    /// Total lines available in the selected stream(s).
    #[serde(skip)]
    pub available: u64,
}

/// The last `n` lines of `stream`. `Merged` restores arrival order from
/// `interleave.idx`: the newest `n` lines overall contain, per stream, only
/// a suffix of at most `n` lines, so one bounded pass per spool plus the
/// journal's newest `n` segments suffice. Runs without a journal fall back
/// to stdout's tail then stderr's. Memory stays O(n · [`MAX_TEXT`]).
pub fn tail(run_dir: &Path, stream: LogStream, n: usize) -> Result<LogPage, StoreError> {
    let single = |s: Stream| -> Result<LogPage, StoreError> {
        let mut ring = VecDeque::with_capacity(n.min(4096));
        let total = scan_lines(&spool(run_dir, s), |line, text| {
            push_bounded(&mut ring, n, (line, text));
        })?;
        let lines = ring
            .into_iter()
            .map(|(line, text)| LogLine {
                stream: stream_name(s),
                line,
                text,
            })
            .collect::<Vec<_>>();
        Ok(page(lines, total))
    };
    match stream {
        LogStream::Out => single(Stream::Out),
        LogStream::Err => single(Stream::Err),
        LogStream::Merged => merged_tail(run_dir, n),
    }
}

/// Up to `n` lines of `stream` starting at raw line `from` (1-based).
pub fn log_from(
    run_dir: &Path,
    stream: Stream,
    from: u64,
    n: usize,
) -> Result<LogPage, StoreError> {
    let mut lines = Vec::with_capacity(n.min(4096));
    let total = scan_lines(&spool(run_dir, stream), |line, text| {
        if line >= from && lines.len() < n {
            lines.push(LogLine {
                stream: stream_name(stream),
                line,
                text,
            });
        }
    })?;
    Ok(page(lines, total))
}

fn page(lines: Vec<LogLine>, available: u64) -> LogPage {
    LogPage {
        truncated: available > lines.len() as u64,
        lines,
        available,
    }
}

fn merged_tail(run_dir: &Path, n: usize) -> Result<LogPage, StoreError> {
    let mut tails: [VecDeque<(u64, String)>; 2] = [VecDeque::new(), VecDeque::new()];
    let mut totals = [0u64; 2];
    for stream in [Stream::Out, Stream::Err] {
        let ring = &mut tails[index(stream)];
        totals[index(stream)] = scan_lines(&spool(run_dir, stream), |line, text| {
            push_bounded(ring, n, (line, text));
        })?;
    }
    let segments = last_segments(run_dir, n)?;
    let order = if segments.is_empty() {
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
            // Rings hold each stream's last n lines in ascending order.
            let ring = &tails[index(stream)];
            let first = ring.front()?.0;
            let (found, text) = ring.get(usize::try_from(line.checked_sub(first)?).ok()?)?;
            (*found == line).then(|| LogLine {
                stream: stream_name(stream),
                line,
                text: text.clone(),
            })
        })
        .collect();
    Ok(page(lines, totals[0] + totals[1]))
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

/// Streams `path` line by line as `(1-based line number, lossy text capped
/// at MAX_TEXT)`; a missing spool reads as empty. Returns the line count.
fn scan_lines(path: &Path, mut visit: impl FnMut(u64, String)) -> Result<u64, StoreError> {
    let io_err = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_err(source)),
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
        let buf = reader.fill_buf().map_err(io_err)?;
        if buf.is_empty() {
            break;
        }
        let (chunk, newline) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (&buf[..at], true),
            None => (buf, false),
        };
        let room = MAX_TEXT.saturating_sub(current.len());
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
    if cap == 0 {
        return;
    }
    if ring.len() == cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

fn spool(run_dir: &Path, stream: Stream) -> PathBuf {
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

/// `out` / `err`.
pub fn stream_name(stream: Stream) -> &'static str {
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
        assert_eq!(
            arrival_order(&segments, [4, 3], 10),
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

    #[test]
    fn clip_keeps_short_text_and_cuts_long_on_char_boundary() {
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(MAX_TEXT);
        let clipped = clip(&long);
        assert!(clipped.len() < long.len());
        assert!(clipped.ends_with("more bytes]"));
    }

    #[test]
    fn scan_lines_caps_a_newline_free_line_and_counts_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stdout.raw");
        let mut bytes = vec![b'x'; MAX_TEXT * 3];
        bytes.extend_from_slice(b"\nshort\nlast-no-newline");
        std::fs::write(&path, &bytes).unwrap();
        let mut seen = Vec::new();
        let total = scan_lines(&path, |line, text| seen.push((line, text))).unwrap();
        assert_eq!(total, 3);
        assert!(seen[0].1.len() < MAX_TEXT + 64, "huge line capped");
        assert!(seen[0]
            .1
            .ends_with(&format!("[{} more bytes]", MAX_TEXT * 2)));
        assert_eq!(seen[1], (2, "short".to_string()));
        assert_eq!(seen[2], (3, "last-no-newline".to_string()));
    }

    #[test]
    fn log_from_returns_a_window_and_flags_more() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stdout.raw"), "a\nb\nc\nd\n").unwrap();
        let page = log_from(dir.path(), Stream::Out, 2, 2).unwrap();
        let texts: Vec<&str> = page.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["b", "c"]);
        assert!(page.truncated);
        assert_eq!(page.available, 4);
    }
}
