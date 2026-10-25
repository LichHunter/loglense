//! Autodetection harness (docs/parsers.md §3.3): run every registered
//! module's `detect` over the buffered sample, pick the highest score above
//! 0, break ties by registration order (built-ins first, then packs in load
//! order), and fall back to `generic` when nothing scores.

use crate::registry::{ParserRegistry, GENERIC};

/// Select a parser name from `sample` (the buffered first lines of a run).
///
/// The buffering itself (≤ 50 lines / ≤ 2 s, EOF releases early) is owned by
/// the runner (T11); this function only scores.
pub fn detect_from_sample(registry: &ParserRegistry, sample: &[&str]) -> String {
    let mut best_name = GENERIC;
    let mut best_score = 0u8;
    for name in registry.names() {
        if let Some(score) = registry.detect_of(name, sample) {
            if score > best_score {
                best_name = name;
                best_score = score;
            }
        }
    }
    best_name.to_string()
}
