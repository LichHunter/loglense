//! Two-tier modular log parsing: language bases, tool parsers, TOML packs.
//!
//! Framework layout (task T6):
//! - [`traits`] — the module contract every parser satisfies.
//! - [`registry`] — name → parser with source precedence and `extends`
//!   resolution.
//! - [`chain`] — per-run, per-stream first-claim-wins intake.
//! - [`detect`] — autodetection over the buffered sample.
//! - [`pack`] — TOML pack grammar (schema = 1) loader.
//!
//! Concrete parsers (task T7+): [`bases`] ships the format bases
//! `generic`/`json`/`logfmt`; lang bases and tool parsers land in later
//! waves, registered through [`bases::register`].

pub mod bases;
pub mod chain;
pub mod detect;
pub mod pack;
pub mod registry;
pub mod traits;

pub use chain::{Chain, MultiChainState};
pub use detect::detect_from_sample;
pub use pack::{load_pack, load_pack_dir, CompiledRule, Pack, PackError, MAX_RULES};
pub use registry::{ParserRegistry, Registration, RegistryError, Source, GENERIC};
pub use traits::{FeedOutcome, LineCtx, ParseState, Parser};

// Core event model re-exports: parsers consume these types (docs/parsers.md
// §1). `Source` is renamed: the registry's `Source` (registration origin)
// shadows the event's `Source` (file/line origin).
pub use loglense_core::Source as EventSource;
pub use loglense_core::{Event, Kind, Severity, Stream, TestInfo};
