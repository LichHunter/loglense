//! Per-stream capture reader threads (docs/architecture.md §4.3): spool
//! pristine bytes to the raw file while decoding lines for the parser —
//! lossy UTF-8 + ANSI strip — with binary detection (NUL in the first 4 KB
//! disables parsing for that stream; raw capture continues).

use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use loglense_core::Stream;

use crate::ansi;

/// Read-chunk size: large enough to keep GB-scale throughput, small enough
/// that memory stays flat.
const CHUNK: usize = 64 * 1024;
/// A NUL byte within this prefix marks the stream binary.
const BINARY_SNIFF_BYTES: usize = 4096;
/// Lines longer than this are split at the cap so one pathological
/// newline-free write can never grow an unbounded line buffer.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Process-wide pipe-read counter shared by both reader threads: the true
/// cross-stream arrival order (the channel can reorder whole bursts).
static READ_SEQ: AtomicU64 = AtomicU64::new(0);

/// One reader-thread event for the capture loop.
pub(crate) enum CapMsg {
    /// A decoded, ANSI-stripped line at its 1-based raw line number.
    Line {
        /// Which stream produced the line.
        stream: Stream,
        /// 1-based line number in the stream's raw spool.
        line_no: u64,
        /// Decoded, stripped line text.
        text: String,
        /// [`READ_SEQ`] of the pipe read that completed this line.
        read_seq: u64,
    },
    /// NUL seen in the first 4 KB: parsing disabled for this stream.
    Binary {
        /// Which stream went binary.
        stream: Stream,
    },
    /// The raw spool write failed (ENOSPC …): parsing must stop.
    RawFailed {
        /// Which stream's spool failed.
        stream: Stream,
        /// The write failure.
        error: String,
    },
    /// Stream closed; all bytes spooled.
    Eof {
        /// Which stream ended.
        stream: Stream,
    },
}

/// Spawns the reader thread for one stream: `pipe` is the child's stdout or
/// stderr, `raw` the append-mode spool file, `tx` the channel to the
/// capture loop. The channel is bounded: when parsing falls behind, the
/// reader blocks, the pipe fills and the child is throttled — memory stays
/// flat instead of queueing the backlog (NFR-03).
pub(crate) fn spawn_reader<R>(
    mut pipe: R,
    mut raw: File,
    stream: Stream,
    tx: SyncSender<CapMsg>,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buf = [0u8; CHUNK];
        let mut pending: Vec<u8> = Vec::new();
        let mut line_no: u64 = 0;
        let mut seen: usize = 0;
        let mut binary = false;
        let mut raw_failed = false;
        let mut read_seq = 0;
        loop {
            let read = match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    let _ = tx.send(CapMsg::RawFailed {
                        stream,
                        error: err.to_string(),
                    });
                    break;
                }
            };
            read_seq = READ_SEQ.fetch_add(1, Ordering::SeqCst);
            seen += read;
            if let Err(err) = raw.write_all(&buf[..read]) {
                if !raw_failed {
                    raw_failed = true;
                    let _ = tx.send(CapMsg::RawFailed {
                        stream,
                        error: err.to_string(),
                    });
                }
            }
            if !binary && seen - read < BINARY_SNIFF_BYTES {
                let sniff = &buf[..read.min(BINARY_SNIFF_BYTES)];
                if sniff.contains(&0) {
                    binary = true;
                    pending.clear();
                    let _ = tx.send(CapMsg::Binary { stream });
                }
            }
            if !binary {
                pending.extend_from_slice(&buf[..read]);
                emit_lines(&tx, stream, &mut pending, &mut line_no, read_seq);
            }
        }
        if !binary && !pending.is_empty() {
            line_no += 1;
            let _ = tx.send(CapMsg::Line {
                stream,
                line_no,
                text: decode(&pending),
                read_seq,
            });
        }
        let _ = raw.flush();
        let _ = tx.send(CapMsg::Eof { stream });
    })
}

/// Splits `pending` on newlines, sending one message per complete line;
/// a newline-free run past [`MAX_LINE_BYTES`] is flushed as a split line.
fn emit_lines(
    tx: &SyncSender<CapMsg>,
    stream: Stream,
    pending: &mut Vec<u8>,
    line_no: &mut u64,
    read_seq: u64,
) {
    loop {
        let Some(nl) = pending.iter().position(|b| *b == b'\n') else {
            if pending.len() >= MAX_LINE_BYTES {
                *line_no += 1;
                let text = decode(pending);
                pending.clear();
                let _ = tx.send(CapMsg::Line {
                    stream,
                    line_no: *line_no,
                    text,
                    read_seq,
                });
            }
            return;
        };
        let line: Vec<u8> = pending.drain(..=nl).collect();
        *line_no += 1;
        let _ = tx.send(CapMsg::Line {
            stream,
            line_no: *line_no,
            text: decode(&line[..line.len() - 1]),
            read_seq,
        });
    }
}

/// Lossy UTF-8 decode + ANSI strip (docs/parsers.md §1.4).
fn decode(bytes: &[u8]) -> String {
    ansi::strip(&String::from_utf8_lossy(bytes))
}
