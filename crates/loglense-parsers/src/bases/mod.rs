//! Format base parsers (docs/parsers.md §5.1): [`generic`], [`json`] and
//! [`logfmt`] — stateless single-line modules. Lang bases (java/python/
//! rust/go) and tool parsers land in sibling modules in later waves.
//!
//! Built-in registration pattern (T8-T10 workers extend this): implement
//! [`Parser`](crate::traits::Parser) in `src/bases/<name>.rs`, declare
//! `pub mod <name>;` here, add one [`register`] line, and add one arm to
//! `parser_for` in `tests/golden.rs` so fixture directories resolve.
//!
//! [`generic`]: generic::GenericParser
//! [`json`]: json::JsonParser
//! [`logfmt`]: logfmt::LogfmtParser

pub mod generic;
pub mod json;
pub mod logfmt;

use loglense_core::Severity;

use crate::registry::{ParserRegistry, Source};

/// Register every built-in format base under its name with
/// [`Source::Builtin`]. Called once at registry construction; pack sources
/// (user/project) shadow these by name per docs/parsers.md §3.1.
pub fn register(registry: &mut ParserRegistry) {
    registry.register(
        "generic",
        || Box::new(generic::GenericParser),
        Source::Builtin,
        None,
    );
    registry.register("json", || Box::new(json::JsonParser), Source::Builtin, None);
    registry.register(
        "logfmt",
        || Box::new(logfmt::LogfmtParser),
        Source::Builtin,
        None,
    );
}

/// Case-insensitive level-string → [`Severity`] mapping shared by the
/// structured format bases: json `level`/`severity`/`lvl` values and logfmt
/// `level=` values. Prefix families (`warn…`, `error…`) so `warn`,
/// `warning`, `errors`… all fold to one severity; anything unrecognized —
/// including a missing level — is [`Severity::Info`].
pub(crate) fn map_level(raw: &str) -> Severity {
    let lowered = raw.to_ascii_lowercase();
    if lowered.starts_with("error") || lowered.starts_with("fatal") {
        Severity::Error
    } else if lowered.starts_with("warn") {
        Severity::Warning
    } else if lowered.starts_with("notice") {
        Severity::Notice
    } else if lowered.starts_with("debug") || lowered.starts_with("trace") {
        Severity::Debug
    } else {
        Severity::Info
    }
}

/// Autodetection score shared by the structured format bases: the share
/// (0–100) of sample lines satisfying `predicate`. Empty samples score 0.
pub(crate) fn percentage(sample: &[&str], predicate: fn(&str) -> bool) -> u8 {
    if sample.is_empty() {
        return 0;
    }
    let hits = sample.iter().filter(|line| predicate(line)).count();
    u8::try_from(hits * 100 / sample.len()).unwrap_or(100)
}
