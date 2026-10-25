//! `interleave.idx` — the arrival order of stdout and stderr lines.
//!
//! The raw spools keep each stream's bytes but not how the two streams
//! interleaved, and event `seq` is emission order (autodetect buffering and
//! multi-line blocks emit late). Each pipe read in the supervisor takes a
//! process-wide read sequence number; the capture loop appends one record
//! per read chunk, `o<line>@<seq>` / `e<line>@<seq>` (first raw line the
//! chunk completed, its read seq). Records land in channel order, which can
//! lag read order by a bounded window, so readers sort by seq. Ordering is
//! exact up to pipe-read granularity: lines one read returned stay
//! together.

use std::io::{BufRead as _, Write as _};
use std::path::Path;

use loglense_core::Stream;

use crate::StoreError;

/// File name inside a run directory.
pub const INTERLEAVE_FILE: &str = "interleave.idx";

/// Append side, owned by the supervisor. Write failures disable the
/// journal (readers then fall back to per-stream order) — never the run.
pub struct InterleaveWriter {
    file: Option<std::fs::File>,
    /// Last recorded read seq per stream (out, err).
    last: [Option<u64>; 2],
}

impl InterleaveWriter {
    /// Creates (truncates) `run_dir/interleave.idx`.
    pub fn create(run_dir: &Path) -> Result<Self, StoreError> {
        let path = run_dir.join(INTERLEAVE_FILE);
        let file = std::fs::File::create(&path).map_err(|source| crate::io(path, source))?;
        Ok(Self {
            file: Some(file),
            last: [None; 2],
        })
    }

    /// Notes raw line `line` of `stream`, completed by pipe read `read_seq`;
    /// writes a record for the first line of each read chunk only.
    pub fn record(&mut self, stream: Stream, line: u64, read_seq: u64) {
        let slot = &mut self.last[usize::from(stream == Stream::Err)];
        if *slot == Some(read_seq) {
            return;
        }
        *slot = Some(read_seq);
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let tag = match stream {
            Stream::Out => 'o',
            Stream::Err => 'e',
        };
        // One small write per read chunk: readers of a live run see it at
        // once.
        if file
            .write_all(format!("{tag}{line}@{read_seq}\n").as_bytes())
            .is_err()
        {
            self.file = None;
        }
    }
}

/// One arrival segment: `stream` lines from `start` up to (excluding) the
/// stream's next segment start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Which stream the segment belongs to.
    pub stream: Stream,
    /// First raw line number of the segment (1-based).
    pub start: u64,
    /// Read sequence number (cross-stream arrival order).
    pub read_seq: u64,
}

/// Records kept beyond `keep` before sorting: covers the channel's bounded
/// reordering window so the newest `keep` by read seq are all present.
const REORDER_SLACK: usize = 1024;

/// Reads the journal and returns its newest `keep` segments in arrival
/// (read seq) order — each segment holds ≥ 1 line, so `keep` = N suffices
/// for an N-line tail. Missing file → empty; malformed or torn records are
/// skipped.
pub fn last_segments(run_dir: &Path, keep: usize) -> Result<Vec<Segment>, StoreError> {
    let path = run_dir.join(INTERLEAVE_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(crate::io(path, source)),
    };
    if keep == 0 {
        return Ok(Vec::new());
    }
    let window = keep.saturating_add(REORDER_SLACK);
    let mut ring = std::collections::VecDeque::with_capacity(window.min(8192));
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|source| crate::io(&path, source))?;
        let Some(segment) = parse_record(&line) else {
            continue;
        };
        if ring.len() == window {
            ring.pop_front();
        }
        ring.push_back(segment);
    }
    let mut segments: Vec<Segment> = ring.into();
    segments.sort_by_key(|segment| segment.read_seq);
    let excess = segments.len().saturating_sub(keep);
    segments.drain(..excess);
    Ok(segments)
}

/// `o<line>@<seq>` / `e<line>@<seq>`.
fn parse_record(record: &str) -> Option<Segment> {
    let (stream, rest) = match record.split_at_checked(1)? {
        ("o", rest) => (Stream::Out, rest),
        ("e", rest) => (Stream::Err, rest),
        _ => return None,
    };
    let (start, read_seq) = rest.split_once('@')?;
    Some(Segment {
        stream,
        start: start.parse().ok()?,
        read_seq: read_seq.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_one_line_per_read_chunk_and_sorts_by_read_seq() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = InterleaveWriter::create(dir.path()).unwrap();
        // Channel order: out chunk 0 (lines 1-2), out chunk 3 (line 3)
        // delivered before err chunk 1 (line 1) — read order is 0, 1, 3.
        for (stream, line, seq) in [
            (Stream::Out, 1, 0),
            (Stream::Out, 2, 0),
            (Stream::Out, 3, 3),
            (Stream::Err, 1, 1),
        ] {
            writer.record(stream, line, seq);
        }
        drop(writer);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(INTERLEAVE_FILE)).unwrap(),
            "o1@0\no3@3\ne1@1\n"
        );
        let order: Vec<(Stream, u64)> = last_segments(dir.path(), 10)
            .unwrap()
            .into_iter()
            .map(|s| (s.stream, s.start))
            .collect();
        assert_eq!(
            order,
            [(Stream::Out, 1), (Stream::Err, 1), (Stream::Out, 3)]
        );
        let newest = last_segments(dir.path(), 1).unwrap();
        assert_eq!((newest[0].stream, newest[0].start), (Stream::Out, 3));
    }

    #[test]
    fn missing_or_garbage_journal_reads_tolerantly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(last_segments(dir.path(), 10).unwrap().is_empty());
        std::fs::write(
            dir.path().join(INTERLEAVE_FILE),
            "o1@0\nzz\ne@1\ne7\no2@4\ne3@",
        )
        .unwrap();
        let segments = last_segments(dir.path(), 10).unwrap();
        assert_eq!(segments.len(), 2, "garbage skipped: {segments:?}");
    }
}
