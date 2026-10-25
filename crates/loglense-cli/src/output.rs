//! Shared rendering helpers: JSON emission and human time formats. Item
//! caps live in `loglense_store::query`, shared with the MCP surface.

/// Prints `value` as one compact JSON line on stdout.
pub(crate) fn json(value: &impl serde::Serialize) {
    match serde_json::to_string(value) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("loglense: cannot serialize output: {err}"),
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
