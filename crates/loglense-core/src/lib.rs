//! Core domain types for loglense: run ids, events, configuration, template dedup.

mod config;
mod event;
mod runid;
pub mod template;

pub use config::{Config, ConfigError, Output, Retention};
pub use event::{Event, Kind, Severity, Source, Stream, TestInfo};
pub use runid::{RunId, RunIdError};
