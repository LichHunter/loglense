//! Shared rendering: JSON emission, per-item size caps, state labels.

use loglense_runner::Lifecycle;
use loglense_store::Status;

/// Longest message / line any read surface prints (bytes, cut on a char
/// boundary): one pathological 1 MB line must not flood an agent context.
pub(crate) const MAX_TEXT: usize = 4096;
/// Most stack frames printed per event.
pub(crate) const MAX_FRAMES: usize = 20;

/// Prints `value` as one compact JSON line on stdout.
pub(crate) fn json(value: &impl serde::Serialize) {
    match serde_json::to_string(value) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("loglense: cannot serialize output: {err}"),
    }
}

/// `text` capped at [`MAX_TEXT`] bytes with a marker naming what was cut.
pub(crate) fn clip(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_string();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} more bytes]", &text[..end], text.len() - end)
}

/// Wire name of a persisted status.
pub(crate) fn status_name(status: Status) -> &'static str {
    match status {
        Status::Starting => "starting",
        Status::Running => "running",
        Status::Exited => "exited",
        Status::Signaled => "signaled",
        Status::FailedToStart => "failed-to-start",
    }
}

/// Wire name of an observed lifecycle (terminal status, live, or derived).
pub(crate) fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Running => "running",
        Lifecycle::Terminal(status) => status_name(status),
        Lifecycle::Orphaned => "orphaned",
        Lifecycle::Lost => "lost",
        Lifecycle::Corrupt => "corrupt",
    }
}

/// Human age: `42s`, `5m`, `3h`, `2d`.
pub(crate) fn age(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Human duration: `850ms`, `12.3s`, `4m05s`.
pub(crate) fn duration(ms: i64) -> String {
    match ms {
        ..=999 => format!("{}ms", ms.max(0)),
        1000..=59_999 => format!("{:.1}s", ms as f64 / 1000.0),
        _ => format!("{}m{:02}s", ms / 60_000, (ms / 1000) % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_short_text_and_cuts_long_on_char_boundary() {
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(MAX_TEXT);
        let clipped = clip(&long);
        assert!(clipped.len() < long.len());
        assert!(clipped.ends_with("more bytes]"));
    }

    #[test]
    fn age_and_duration_buckets() {
        assert_eq!(age(42_000), "42s");
        assert_eq!(age(300_000), "5m");
        assert_eq!(age(3 * 3_600_000), "3h");
        assert_eq!(age(2 * 86_400_000), "2d");
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(12_300), "12.3s");
        assert_eq!(duration(245_000), "4m05s");
    }
}
