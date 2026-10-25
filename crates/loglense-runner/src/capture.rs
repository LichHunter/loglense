//! The capture loop: wires reader threads to the parse pump, polls the
//! kill marker, the TERM/INT trap and the child, and enforces bounded
//! teardown.

use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use loglense_core::Event;
use loglense_parsers::ParserRegistry;
use loglense_store::{EventsWriter, InterleaveWriter, StoreError};

use crate::pump::{Capture, EventSink, Pump, Selection};
use crate::reader::{spawn_reader, CapMsg};
use crate::RunnerError;
use crate::{control, sys};

/// Kill-marker / stop-trap / child poll cadence.
const POLL: Duration = Duration::from_millis(100);
/// After the child exits, how long grandchildren may keep the pipes open
/// before the whole process group is force-killed.
const PIPE_GRACE: Duration = Duration::from_secs(10);
/// Decoded lines in flight between the readers and the pump. Worst case
/// is `CHANNEL_CAP` × the reader's 1 MB line cap (64 MB); typical lines
/// keep it at a few KB.
const CHANNEL_CAP: usize = 64;

impl EventSink for EventsWriter {
    fn append(&mut self, event: &Event) -> Result<(), StoreError> {
        EventsWriter::append(self, event)
    }
}

/// Runs the capture pipeline until the child reaches a terminal state and
/// both pipes hit EOF. Returns the run's accounting and the exit status.
pub(crate) fn run_capture(
    child: &mut Child,
    run_dir: &std::path::Path,
    registry: &ParserRegistry,
    selection: Selection,
) -> Result<(Capture, ExitStatus), RunnerError> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut writer = EventsWriter::open(&run_dir.join("events.jsonl"))?;
    let mut pump = Pump::new(&mut writer, registry, selection);
    let mut interleave = InterleaveWriter::create(run_dir)?;
    let (tx, rx) = mpsc::sync_channel::<CapMsg>(CHANNEL_CAP);
    let out_raw = open_raw(run_dir, "stdout.raw")?;
    let err_raw = open_raw(run_dir, "stderr.raw")?;
    let mut handles = Vec::with_capacity(2);
    if let Some(pipe) = stdout {
        handles.push(spawn_reader(
            pipe,
            out_raw,
            loglense_core::Stream::Out,
            tx.clone(),
        ));
    }
    if let Some(pipe) = stderr {
        handles.push(spawn_reader(
            pipe,
            err_raw,
            loglense_core::Stream::Err,
            tx.clone(),
        ));
    }
    drop(tx);

    let mut kill_requested = false;
    let mut child_status: Option<ExitStatus> = None;
    let mut child_exit_at: Option<Instant> = None;
    let mut eofs_seen = 0;
    let mut readers_done = false;
    while !readers_done || child_status.is_none() {
        let wait = if readers_done {
            Duration::from_millis(50)
        } else {
            pump.next_deadline().map_or(POLL, |d| {
                d.saturating_duration_since(Instant::now()).min(POLL)
            })
        };
        match rx.recv_timeout(wait) {
            Ok(CapMsg::Line {
                stream,
                line_no,
                text,
                read_seq,
            }) => {
                interleave.record(stream, line_no, read_seq);
                pump.handle_line(stream, line_no, &text);
            }
            Ok(CapMsg::Binary { stream }) => pump.handle_binary(stream),
            Ok(CapMsg::RawFailed { stream, error }) => pump.stop_parsing(format!(
                "raw spool write failed on {stream:?} stream: {error}"
            )),
            Ok(CapMsg::Eof { stream }) => {
                pump.handle_eof(stream);
                eofs_seen += 1;
                if eofs_seen >= 2 {
                    readers_done = true;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => readers_done = true,
        }
        pump.release_deadlines(Instant::now());
        if !kill_requested && (control::kill_requested(run_dir) || sys::stop_signal().is_some()) {
            kill_requested = true;
            control::terminate_child(child);
        }
        if child_status.is_none() {
            if let Some(status) = child.try_wait().map_err(|source| RunnerError::Io {
                path: run_dir.to_path_buf(),
                source,
            })? {
                child_status = Some(status);
                child_exit_at = Some(Instant::now());
            }
        }
        // Child gone but a grandchild still holds the pipes: force-close.
        if child_exit_at.is_some_and(|at| Instant::now().duration_since(at) > PIPE_GRACE)
            && !readers_done
        {
            control::kill_group(child.id());
            let _ = child.kill();
        }
    }
    for handle in handles {
        let _ = handle.join();
    }
    let status = match child_status {
        Some(status) => status,
        None => child.wait().map_err(|source| RunnerError::Io {
            path: run_dir.to_path_buf(),
            source,
        })?,
    };
    Ok((pump.into_capture(), status))
}

/// Opens a pre-created raw spool file for appending.
fn open_raw(run_dir: &std::path::Path, name: &str) -> Result<std::fs::File, RunnerError> {
    let path = run_dir.join(name);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|source| RunnerError::Io { path, source })
}
