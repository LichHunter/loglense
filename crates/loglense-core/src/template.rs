//! Template normalization and dedup grouping (docs/parsers.md §1.1
//! `template_id`, docs/architecture.md §4.3).
//!
//! Normalization collapses volatile detail so similar lines share one
//! template: digit runs → `N`, hex runs ≥ 6 chars → `H` (`0x` prefix kept as
//! `0xH`), double-quoted strings → `S`, path-like tokens → `P`. A template id
//! is the first 12 hex chars of the normalized text's SHA-256, so ids are
//! stable across processes and runs. [`TemplateGroups`] tracks occurrences
//! (count, first/last line, example) under an LRU cap so a GB-scale run can
//! never grow an unbounded index.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

/// Default LRU cap on tracked templates.
pub const DEFAULT_TEMPLATE_CAP: usize = 10_000;

/// Length of a template id in hex characters.
const TEMPLATE_ID_LEN: usize = 12;

/// Minimum length for a hex run to collapse to `H`.
const HEX_MIN_LEN: usize = 6;

/// Normalizes a log message into its template form.
///
/// Token-by-token (whitespace-separated), most-specific rule first: a token
/// that is a double-quoted string becomes `S`; otherwise each `:`-separated
/// segment becomes `H` (hex run), `P` (path-like), or has every digit run
/// replaced by a single `N`. The output is idempotent
/// (`normalize(normalize(x)) == normalize(x)`).
pub fn normalize(message: &str) -> String {
    message
        .split_whitespace()
        .map(normalize_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Computes the template id: 12 lowercase hex chars of the SHA-256 of
/// `normalized` (the output of [`normalize`]).
pub fn template_id(normalized: &str) -> String {
    let digest = Sha256::digest(normalized.as_bytes());
    digest[..TEMPLATE_ID_LEN / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Occurrence accounting for one template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateGroup {
    /// Line number of the first occurrence.
    pub first_seen: u64,
    /// Line number of the most recent occurrence.
    pub last_seen: u64,
    /// Total occurrences recorded.
    pub count: u64,
    /// The first occurrence's original (un-normalized) message.
    pub example: String,
}

/// Tracks templates and their occurrences under an LRU cap.
///
/// Recording a message already tracked refreshes it (an LRU touch); recording
/// a new template while at cap evicts the least recently used one. The cap
/// keeps memory bounded on GB-scale runs; the surviving top groups feed the
/// run summary's template list.
#[derive(Debug)]
pub struct TemplateGroups {
    cap: usize,
    clock: u64,
    groups: HashMap<String, (TemplateGroup, u64)>,
}

impl Default for TemplateGroups {
    fn default() -> Self {
        TemplateGroups::with_cap(DEFAULT_TEMPLATE_CAP)
    }
}

impl TemplateGroups {
    /// Creates a tracker with the default cap ([`DEFAULT_TEMPLATE_CAP`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a tracker holding at most `cap` templates.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            cap,
            clock: 0,
            groups: HashMap::new(),
        }
    }

    /// Records one occurrence of `message` seen at raw line `line`, and
    /// returns its template id.
    pub fn record(&mut self, message: &str, line: u64) -> String {
        let id = template_id(&normalize(message));
        self.clock += 1;
        match self.groups.get_mut(&id) {
            Some((group, touch)) => {
                group.count += 1;
                group.last_seen = line;
                *touch = self.clock;
            }
            None => {
                if self.groups.len() >= self.cap {
                    self.evict_least_recent();
                }
                self.groups.insert(
                    id.clone(),
                    (
                        TemplateGroup {
                            first_seen: line,
                            last_seen: line,
                            count: 1,
                            example: message.to_string(),
                        },
                        self.clock,
                    ),
                );
            }
        }
        id
    }

    /// Number of tracked templates (never exceeds the cap).
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether no templates are tracked.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Occurrence accounting for `template_id`, if still tracked.
    pub fn get(&self, template_id: &str) -> Option<&TemplateGroup> {
        self.groups.get(template_id).map(|(group, _)| group)
    }

    /// Drops the entry with the smallest touch clock value.
    fn evict_least_recent(&mut self) {
        if let Some(oldest) = self
            .groups
            .iter()
            .min_by_key(|(_, (_, touch))| *touch)
            .map(|(id, _)| id.clone())
        {
            self.groups.remove(&oldest);
        }
    }
}

/// Normalizes one whitespace-delimited token.
fn normalize_token(token: &str) -> String {
    if token.len() >= 2 && token.starts_with('"') && token.ends_with('"') {
        return "S".to_string();
    }
    token
        .split(':')
        .map(normalize_segment)
        .collect::<Vec<_>>()
        .join(":")
}

/// Normalizes one `:`-separated segment: hex > path > digit runs.
fn normalize_segment(segment: &str) -> String {
    let hex_digits = segment
        .strip_prefix("0x")
        .or_else(|| segment.strip_prefix("0X"));
    match hex_digits {
        Some(hex) if hex.len() >= HEX_MIN_LEN && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            return format!("{}H", &segment[..2]);
        }
        None if segment.len() >= HEX_MIN_LEN && segment.bytes().all(|b| b.is_ascii_hexdigit()) => {
            return "H".to_string();
        }
        _ => {}
    }
    if is_path_like(segment) {
        return "P".to_string();
    }
    replace_digit_runs(segment)
}

/// Whether a segment looks like a filesystem path: contains `/` and either a
/// `.` or a second `/`.
fn is_path_like(segment: &str) -> bool {
    let slashes = segment.bytes().filter(|&b| b == b'/').count();
    slashes >= 1 && (segment.contains('.') || slashes >= 2)
}

/// Replaces every maximal run of ASCII digits with a single `N`.
fn replace_digit_runs(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut in_digits = false;
    for ch in segment.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('N');
                in_digits = true;
            }
        } else {
            out.push(ch);
            in_digits = false;
        }
    }
    out
}
