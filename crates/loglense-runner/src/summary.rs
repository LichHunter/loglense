//! `summary.json` — the terminal, agent-sized digest written once per run
//! (docs/architecture.md §4.3): severity counts, test totals, top deduped
//! templates, truncation flag. Written atomically (`.tmp` + rename) after
//! the terminal meta.

use std::collections::BTreeMap;
use std::path::Path;

use loglense_core::Severity;
use loglense_store::Status;

use crate::RunnerError;

/// One deduped error/warning template row (errors first, then by count).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TemplateEntry {
    /// Id of the normalized template (links back to event `template_id`).
    pub template_id: String,
    /// `error` or `warning` (severity of the first occurrence).
    pub severity: String,
    /// Total occurrences of this template.
    pub count: u64,
    /// Raw line of the first occurrence.
    pub first_seen: u64,
    /// Raw line of the most recent occurrence.
    pub last_seen: u64,
    /// The first occurrence's original message.
    pub example: String,
}

/// The run summary persisted at `runs/<id>/summary.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Summary {
    /// Terminal status (mirrors `meta.json`).
    pub status: Status,
    /// Exit code when the child exited normally.
    pub exit_code: Option<i32>,
    /// Terminating signal when the child was signaled.
    pub signal: Option<i32>,
    /// Wall-clock run duration in milliseconds.
    pub duration_ms: i64,
    /// Total events appended to `events.jsonl`.
    pub events: u64,
    /// Events per severity (`"error": 3, …`).
    pub severities: BTreeMap<String, u64>,
    /// Test events per outcome (`"passed": 4, …`), from `kind=test` events.
    pub tests: BTreeMap<String, u64>,
    /// Top error/warning templates, capped by config `max_templates`.
    pub templates: Vec<TemplateEntry>,
    /// Whether event capture was cut short (write failure / spool overflow).
    pub truncated: bool,
    /// Why capture was truncated, when it was.
    pub truncated_reason: Option<String>,
    /// Completion-callback result (`ok` / `failed` / `timeout`), recorded
    /// by a second atomic write after the callback finished (D-011).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback: Option<String>,
    /// Exit code, signal, spawn error, or timeout of the callback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_detail: Option<String>,
}

impl Summary {
    /// The summary of a run whose command never started.
    pub(crate) fn failed_to_start(duration_ms: i64) -> Self {
        Summary {
            status: Status::FailedToStart,
            exit_code: None,
            signal: None,
            duration_ms,
            events: 0,
            severities: BTreeMap::new(),
            tests: BTreeMap::new(),
            templates: Vec::new(),
            truncated: false,
            truncated_reason: None,
            callback: None,
            callback_detail: None,
        }
    }
}

/// Writes `summary` to `run_dir/summary.json` atomically.
pub(crate) fn write_summary(run_dir: &Path, summary: &Summary) -> Result<(), RunnerError> {
    let path = run_dir.join("summary.json");
    let bytes = serde_json::to_vec(summary).map_err(|source| RunnerError::Io {
        path: path.clone(),
        source: source.into(),
    })?;
    write_atomic(&path, &bytes)
}

/// `path.tmp` written and synced, then renamed over `path` (same contract as
/// the store's atomic writes; local because the store's helper is
/// crate-private).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), RunnerError> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::File::open(&tmp)?.sync_all())
        .map_err(|source| RunnerError::Io {
            path: tmp.clone(),
            source,
        })?;
    std::fs::rename(&tmp, path).map_err(|source| RunnerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Lowercase wire name of a severity (`Severity::Error` → `"error"`).
pub(crate) fn severity_key(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Notice => "notice",
        Severity::Info => "info",
        Severity::Debug => "debug",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Summary {
        let mut severities = BTreeMap::new();
        severities.insert("error".to_string(), 1);
        Summary {
            status: Status::Exited,
            exit_code: Some(3),
            signal: None,
            duration_ms: 42,
            events: 2,
            severities,
            tests: BTreeMap::new(),
            templates: vec![TemplateEntry {
                template_id: "abc123def456".to_string(),
                severity: "error".to_string(),
                count: 1,
                first_seen: 1,
                last_seen: 1,
                example: "ERROR boom".to_string(),
            }],
            truncated: false,
            truncated_reason: None,
            callback: None,
            callback_detail: None,
        }
    }

    #[test]
    fn summary_serializes_snake_case_with_status_kebab() {
        let json = serde_json::to_value(sample()).unwrap_or_default();
        assert_eq!(json["status"], "exited");
        assert_eq!(json["exit_code"], 3);
        assert_eq!(json["truncated"], false);
        assert_eq!(json["severities"]["error"], 1);
    }

    #[test]
    fn write_summary_replaces_previous_file_atomically() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        write_summary(dir.path(), &sample()).unwrap_or_else(|_| unreachable!());
        // rewrite with different content: rename replaces in place
        let mut other = sample();
        other.exit_code = Some(0);
        write_summary(dir.path(), &other).unwrap_or_else(|_| unreachable!());
        let text = std::fs::read_to_string(dir.path().join("summary.json")).unwrap_or_default();
        assert!(text.contains("\"exit_code\":0"));
        // no stray tmp files left behind
        assert!(dir
            .path()
            .join("summary.json.tmp")
            .symlink_metadata()
            .is_err());
    }
}
