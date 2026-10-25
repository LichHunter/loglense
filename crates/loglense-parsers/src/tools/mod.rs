//! Tool parsers (docs/parsers.md §5.2): build-tool modules that `extends` a
//! lang base. Each tool adds its own single-line rules on top of the base
//! grammar; the chain puts the tool first, so its rules claim lines before
//! the base's (derived overrides base, docs/parsers.md §3.2).
//!
//! Built-in registration mirrors [`bases`](crate::bases): implement
//! [`Parser`](crate::traits::Parser) in `src/tools/<name>.rs`, declare
//! `pub mod <name>;` here, and add one line to [`register`].
//!
//! Built-ins: `maven` → `java`, `pytest` → `python`, `cargo` → `rust`,
//! `gotest` → `go`, plus the standalone `nix` (no lang base).

pub mod cargo;
pub mod gotest;
pub mod maven;
pub mod nix;
pub mod pytest;

use crate::registry::{ParserRegistry, Source};

/// Register every built-in tool parser under its name with
/// [`Source::Builtin`]. Call after [`bases::register`](crate::bases::register)
/// so `extends` targets resolve.
pub fn register(registry: &mut ParserRegistry) {
    registry.register(
        "maven",
        || Box::new(maven::MavenParser),
        Source::Builtin,
        Some("java".to_string()),
    );
    registry.register(
        "pytest",
        || Box::new(pytest::PytestParser),
        Source::Builtin,
        Some("python".to_string()),
    );
    registry.register(
        "cargo",
        || Box::new(cargo::CargoParser),
        Source::Builtin,
        Some("rust".to_string()),
    );
    registry.register(
        "gotest",
        || Box::new(gotest::GotestParser),
        Source::Builtin,
        Some("go".to_string()),
    );
    registry.register("nix", || Box::new(nix::NixParser), Source::Builtin, None);
}
