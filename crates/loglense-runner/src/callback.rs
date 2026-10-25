//! Completion callbacks (D-011, docs/architecture.md §7): fired by the
//! supervisor strictly after the run's summary and terminal meta are
//! written. Single attempt, bounded by the spec's timeout; output captured
//! to a bounded `callback.log`; the result is recorded in the summary by a
//! second atomic write. A callback can never change the run's outcome.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use loglense_store::{Meta, OnComplete};

use crate::sys;

/// Captured callback output file inside the run dir.
pub const CALLBACK_LOG: &str = "callback.log";
/// Bytes of callback output kept; the rest is drained and counted.
const LOG_CAP: usize = 64 * 1024;
/// Exit-status poll cadence while the callback runs.
const POLL: Duration = Duration::from_millis(20);
/// How long to wait for output readers after the callback exited (a
/// backgrounded grandchild may hold the pipes open).
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// What happened to the callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallbackOutcome {
    /// `ok`, `failed` or `timeout`.
    pub result: &'static str,
    /// Human detail: exit code, signal, spawn error, or the timeout.
    pub detail: String,
}

/// Runs `spec.command` via `sh -c` in the run's working directory with the
/// callback context: `LOGLENSE_RUN_ID`, `LOGLENSE_STATUS`,
/// `LOGLENSE_EXIT_CODE` (only when the child exited), `LOGLENSE_STORE_PATH`
/// in the environment and `summary` on stdin.
pub(crate) fn fire(
    run_dir: &Path,
    state: &Path,
    meta: &Meta,
    spec: &OnComplete,
    summary: &[u8],
) -> CallbackOutcome {
    let log = Arc::new(Mutex::new(BoundedLog::create(&run_dir.join(CALLBACK_LOG))));
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&spec.command)
        .current_dir(&meta.cwd)
        .env("LOGLENSE_RUN_ID", meta.run_id.as_str())
        .env("LOGLENSE_STATUS", meta.status.as_str())
        .env("LOGLENSE_STORE_PATH", state)
        .env_remove("LOGLENSE_EXIT_CODE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(code) = meta.exit_code {
        command.env("LOGLENSE_EXIT_CODE", code.to_string());
    }
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let detail = format!("cannot spawn callback: {err}");
            lock(&log).write(format!("loglense: {detail}\n").as_bytes());
            return CallbackOutcome {
                result: "failed",
                detail,
            };
        }
    };
    // Feed stdin from a thread: a callback that never reads it must not
    // block the supervisor (EPIPE is fine).
    if let Some(mut stdin) = child.stdin.take() {
        let summary = summary.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&summary);
        });
    }
    let (done_tx, done_rx) = mpsc::channel();
    let mut readers = 0;
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        readers += 1;
        let log = Arc::clone(&log);
        let done = done_tx.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                lock(&log).write(&buf[..n]);
            }
            let _ = done.send(());
        });
    }
    drop(done_tx);

    let timeout = Duration::from_millis(spec.timeout_ms);
    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break match (
                    status.code(),
                    std::os::unix::process::ExitStatusExt::signal(&status),
                ) {
                    (Some(0), _) => CallbackOutcome {
                        result: "ok",
                        detail: "exit 0".to_string(),
                    },
                    (Some(code), _) => CallbackOutcome {
                        result: "failed",
                        detail: format!("exit {code}"),
                    },
                    (None, Some(signal)) => CallbackOutcome {
                        result: "failed",
                        detail: format!("killed by signal {signal}"),
                    },
                    (None, None) => CallbackOutcome {
                        result: "failed",
                        detail: "unknown exit status".to_string(),
                    },
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = sys::signal_group(child.id(), sys::SIGKILL);
                let _ = child.wait();
                break CallbackOutcome {
                    result: "timeout",
                    detail: format!("killed after {} ms", spec.timeout_ms),
                };
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(err) => {
                break CallbackOutcome {
                    result: "failed",
                    detail: format!("cannot wait for callback: {err}"),
                }
            }
        }
    };
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    for _ in 0..readers {
        let left = drain_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }
    lock(&log).finish();
    outcome
}

fn lock(log: &Mutex<BoundedLog>) -> std::sync::MutexGuard<'_, BoundedLog> {
    log.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Append-only file that keeps the first [`LOG_CAP`] bytes and counts the
/// rest. Open failures make it a counting sink (never fatal).
struct BoundedLog {
    file: Option<std::fs::File>,
    kept: usize,
    dropped: usize,
}

impl BoundedLog {
    fn create(path: &Path) -> Self {
        Self {
            file: std::fs::File::create(path).ok(),
            kept: 0,
            dropped: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let room = LOG_CAP.saturating_sub(self.kept);
        let keep = bytes.len().min(room);
        if keep > 0 {
            if let Some(file) = self.file.as_mut() {
                if file.write_all(&bytes[..keep]).is_err() {
                    self.file = None;
                }
            }
            self.kept += keep;
        }
        self.dropped += bytes.len() - keep;
    }

    fn finish(&mut self) {
        if self.dropped > 0 {
            let note = format!(
                "\n[loglense: {} more bytes of callback output dropped]\n",
                self.dropped
            );
            if let Some(file) = self.file.as_mut() {
                let _ = file.write_all(note.as_bytes());
            }
            self.dropped = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_log_keeps_the_cap_and_notes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CALLBACK_LOG);
        let mut log = BoundedLog::create(&path);
        log.write(&vec![b'a'; LOG_CAP - 10]);
        log.write(&[b'b'; 100]);
        log.finish();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&"a".repeat(LOG_CAP - 10)));
        assert!(text.contains(&"b".repeat(10)));
        assert!(text.ends_with("[loglense: 90 more bytes of callback output dropped]\n"));
        assert!(text.len() < LOG_CAP + 100);
    }
}
