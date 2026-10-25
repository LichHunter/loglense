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
    let mut stats = EventsRead::default();
    let mut events = Vec::new();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((events, stats)),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_some() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(line) {
                Ok(event) => {
                    events.push(event);
                    stats.events += 1;
                }
                Err(_) => stats.skipped += 1,
            }
        } else if !line.is_empty() {
            stats.skipped += 1;
        }
    }
    Ok((events, stats))
}
