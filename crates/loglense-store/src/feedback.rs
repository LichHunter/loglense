// allow: SIZE_OK — 288 pure LOC vs the 250 ceiling: the T5 spec fixes the
// module list (no separate feedback-index module), and ~50 lines are serde
// field/attribute declarations for the record + pointer contracts.
//! Agent feedback records and the cross-project global index.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt as _;

use crate::{io, StoreError};

/// Directory holding feedback records inside the state dir.
pub const FEEDBACK_DIR: &str = "feedback";
/// Attempts to find a free `f-<ts>-<seq>` slot before giving up. Each
/// collision steps `seq` by 2, so this bounds how many same-second racing
/// writers one `write_feedback` can out-climb.
const ID_TRIES: u32 = 64;

/// Record lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackStatus {
    /// Newly filed, awaiting triage.
    #[default]
    Open,
    /// A fixture was produced and the parser fixed.
    Fixed,
    /// Deliberately not pursued.
    Wontfix,
}

/// One agent feedback report (docs/architecture.md §8).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeedbackRecord {
    /// Report kind, e.g. `parse-miss` / `false-positive`.
    pub kind: String,
    /// What the agent observed, in their words.
    pub detail: String,
    /// Run the feedback refers to.
    pub run_id: String,
    /// Parser that produced the offending events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<String>,
    /// loglense version that produced the run.
    pub tool_version: String,
    /// Raw line number the report points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Expected severity/kind the agent wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Offending raw text excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Triage status.
    #[serde(default)]
    pub status: FeedbackStatus,
    /// Maintainer note added at resolve time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One pointer line in the global feedback index.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeedbackPointer {
    /// Feedback id, e.g. `f-1764000000-3`.
    pub fid: String,
    /// Absolute path of the record JSON.
    pub path: PathBuf,
    /// Record status at last index write.
    pub status: FeedbackStatus,
    /// Record kind (for triage listings without opening every record).
    pub kind: String,
    /// Run the feedback refers to.
    pub run_id: String,
    /// Filing time, epoch ms.
    pub ts_ms: i64,
}

/// `<home>/.local/state/loglense/feedback-index.jsonl`.
pub fn feedback_index_path(home: &Path) -> PathBuf {
    home.join(".local/state/loglense/feedback-index.jsonl")
}

/// Writes a feedback record and appends its pointer to the global index.
///
/// The record lands at `<state>/feedback/f-<ts>-<seq>.json` (`seq` = count
/// of existing records + 1, `+2` retry on collision, claimed atomically
/// with `O_EXCL`); the index line is a single `O_APPEND` write under flock.
pub fn write_feedback(
    state: &Path,
    home: &Path,
    now_ms: i64,
    record: FeedbackRecord,
) -> Result<String, StoreError> {
    let dir = state.join(FEEDBACK_DIR);
    std::fs::create_dir_all(&dir).map_err(|source| io(dir.clone(), source))?;
    let ts = now_ms.max(0) / 1000;
    let mut seq = existing_record_count(&dir)? + 1;
    for _ in 0..ID_TRIES {
        let fid = format!("f-{ts}-{seq}");
        let path = dir.join(format!("{fid}.json"));
        match std::fs::File::create_new(&path) {
            Ok(_slot_claim) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                seq += 2;
                continue;
            }
            Err(source) => return Err(io(path.clone(), source)),
        }
        let bytes = serde_json::to_vec(&record).map_err(|source| StoreError::Serialize {
            what: "feedback record",
            source,
        })?;
        crate::write_atomic(&path, &bytes)?;
        append_index(
            home,
            &FeedbackPointer {
                fid: fid.clone(),
                path: path.canonicalize().unwrap_or_else(|_| path.clone()),
                status: record.status,
                kind: record.kind.clone(),
                run_id: record.run_id.clone(),
                ts_ms: now_ms,
            },
        )?;
        return Ok(fid);
    }
    Err(StoreError::FeedbackIdExhausted {
        dir,
        tries: ID_TRIES,
    })
}

/// Reads one feedback record by id.
pub fn read_feedback(state: &Path, fid: &str) -> Result<FeedbackRecord, StoreError> {
    let path = record_path(state, fid)?;
    let bytes = std::fs::read(&path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => not_found(state, fid),
        _ => io(path.clone(), source),
    })?;
    serde_json::from_slice(&bytes).map_err(|source| StoreError::FeedbackParse { path, source })
}

/// Lists feedback records under `state`, newest first, optionally filtered
/// by status. Unparseable records are skipped, never fatal.
pub fn list_feedback(
    state: &Path,
    status: Option<FeedbackStatus>,
) -> Vec<(String, FeedbackRecord)> {
    let Ok(entries) = std::fs::read_dir(state.join(FEEDBACK_DIR)) else {
        return Vec::new();
    };
    let mut listed: Vec<(String, FeedbackRecord)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let fid = e.path().file_stem()?.to_string_lossy().into_owned();
            let bytes = std::fs::read(e.path()).ok()?;
            let record: FeedbackRecord = serde_json::from_slice(&bytes).ok()?;
            Some((fid, record))
        })
        .filter(|(_, record)| status.is_none_or(|want| record.status == want))
        .collect();
    listed.sort_by(|a, b| b.0.cmp(&a.0));
    listed
}

/// Resolves a record: updates its status/note and rewrites its index line
/// in place (the index never grows on resolve).
pub fn resolve_feedback(
    state: &Path,
    home: &Path,
    fid: &str,
    status: FeedbackStatus,
    note: Option<String>,
) -> Result<(), StoreError> {
    let mut record = read_feedback(state, fid)?;
    record.status = status;
    record.note = note;
    let path = record_path(state, fid)?;
    let bytes = serde_json::to_vec(&record).map_err(|source| StoreError::Serialize {
        what: "feedback record",
        source,
    })?;
    crate::write_atomic(&path, &bytes)?;
    rewrite_index_line(home, fid, status)
}

/// Tolerant reader for the global index: malformed lines and a torn trailing
/// line are skipped, dangling pointers are returned as-is.
pub fn read_feedback_index(home: &Path) -> Result<Vec<FeedbackPointer>, StoreError> {
    let bytes = match std::fs::read(feedback_index_path(home)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(StoreError::Io {
                path: feedback_index_path(home),
                source,
            })
        }
    };
    Ok(complete_lines(&String::from_utf8_lossy(&bytes))
        .filter_map(|line| serde_json::from_str::<FeedbackPointer>(line).ok())
        .collect())
}

/// Complete lines only: a trailing segment without `\n` is a torn write and
/// is dropped, exactly like the events.jsonl reader.
fn complete_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut lines = text.split('\n').peekable();
    std::iter::from_fn(move || {
        let line = lines.next()?;
        (lines.peek().is_some()).then_some(line)
    })
}

/// Record path from a validated fid; ids containing path metainformation
/// are treated as not found rather than trusted.
fn record_path(state: &Path, fid: &str) -> Result<PathBuf, StoreError> {
    if !fid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(not_found(state, fid));
    }
    Ok(state.join(FEEDBACK_DIR).join(format!("{fid}.json")))
}

fn not_found(state: &Path, fid: &str) -> StoreError {
    StoreError::FeedbackNotFound {
        fid: fid.to_string(),
        dir: state.join(FEEDBACK_DIR),
    }
}

/// Count of `*.json` records — the basis for the next sequence number.
fn existing_record_count(dir: &Path) -> Result<u32, StoreError> {
    let entries = std::fs::read_dir(dir).map_err(|source| io(dir, source))?;
    Ok(entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count() as u32)
}

/// Appends one pointer line: `O_APPEND` + a single `write_all` of the full
/// line under an exclusive flock, so concurrent leaves never interleave.
fn append_index(home: &Path, pointer: &FeedbackPointer) -> Result<(), StoreError> {
    let path = feedback_index_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    }
    let mut line = serde_json::to_vec(pointer).map_err(|source| StoreError::Serialize {
        what: "feedback index line",
        source,
    })?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| io(path.clone(), source))?;
    file.lock_exclusive()
        .and_then(|()| file.write_all(&line))
        .and_then(|()| file.unlock())
        .map_err(|source| io(path, source))
}

/// Rewrites `fid`'s index line with the new status, holding the exclusive
/// flock across the read-modify-write so racing appends cannot be lost.
/// Malformed lines and a torn tail are dropped in passing. A missing index
/// is a no-op.
fn rewrite_index_line(home: &Path, fid: &str, status: FeedbackStatus) -> Result<(), StoreError> {
    let path = feedback_index_path(home);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)
        .map_err(|source| io(path.clone(), source))?;
    file.lock_exclusive()
        .map_err(|source| io(path.clone(), source))?;
    let rewrite = (|| {
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)
            .map_err(|source| io(path.clone(), source))?;
        let mut out = String::new();
        for mut pointer in complete_lines(&String::from_utf8_lossy(&raw))
            .filter_map(|line| serde_json::from_str::<FeedbackPointer>(line).ok())
        {
            if pointer.fid == fid {
                pointer.status = status;
            }
            out.push_str(&serde_json::to_string(&pointer).map_err(|source| {
                StoreError::Serialize {
                    what: "feedback index line",
                    source,
                }
            })?);
            out.push('\n');
        }
        crate::write_atomic(&path, out.as_bytes())
    })();
    let unlock = file.unlock().map_err(|source| io(path, source));
    rewrite?;
    unlock
}
