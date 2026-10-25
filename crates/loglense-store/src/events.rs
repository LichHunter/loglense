//! `events.jsonl` append-only writer and tolerant reader.

use std::io::Write;
use std::path::Path;

use loglense_core::Event;

use crate::StoreError;

/// Append-only JSONL writer, flushed after every event.
#[derive(Debug)]
pub struct EventsWriter {
    inner: std::io::BufWriter<std::fs::File>,
}

impl EventsWriter {
    /// Opens (creating when absent) `path` in append mode.
    pub fn open(path: &Path) -> Result<EventsWriter, StoreError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| StoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(EventsWriter {
            inner: std::io::BufWriter::new(file),
        })
    }

    /// Appends one event as a JSON line and flushes.
    pub fn append(&mut self, event: &Event) -> Result<(), StoreError> {
        let mut line = serde_json::to_string(event).map_err(|source| StoreError::Serialize {
            what: "events.jsonl line",
            source,
        })?;
        line.push('\n');
        self.inner
            .write_all(line.as_bytes())
            .and_then(|()| self.inner.flush())
            .map_err(|source| StoreError::Io {
                path: Path::new("events.jsonl").to_path_buf(),
                source,
            })
    }
}

/// Result of a tolerant events read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventsRead {
    /// Successfully parsed events, in file order.
    pub events: usize,
    /// Lines skipped: malformed JSON lines plus an incomplete trailing line.
    pub skipped: usize,
}

/// Reads events from a JSONL file, tolerating crash artifacts.
///
/// A trailing line without `\n` (torn write) and malformed JSON lines are
/// skipped and counted in [`EventsRead::skipped`]. A missing file reads as
/// empty — a run that crashed before its first event is not an error.
pub fn read_events(path: &Path) -> Result<(Vec<Event>, EventsRead), StoreError> {
    let mut events = Vec::new();
    let stats = scan_events(path, |event| events.push(event))?;
    Ok((events, stats))
}

/// Streams events from a JSONL file to `visit` in file order with the same
/// tolerance as [`read_events`], holding one line in memory at a time — the
/// bounded read path for GB-scale runs.
pub fn scan_events(path: &Path, mut visit: impl FnMut(Event)) -> Result<EventsRead, StoreError> {
    use std::io::BufRead as _;

    let mut stats = EventsRead::default();
    let io_err = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(stats),
        Err(source) => return Err(io_err(source)),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(io_err)? == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            // Torn trailing write: the writer had not finished the line.
            stats.skipped += 1;
            break;
        }
        line.pop();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Event>(&line) {
            Ok(event) => {
                visit(event);
                stats.events += 1;
            }
            Err(_) => stats.skipped += 1,
        }
    }
    Ok(stats)
}
