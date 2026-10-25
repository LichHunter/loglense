//! Layered configuration: project `.loglense/config.toml` over user
//! `~/.config/loglense/config.toml` over built-in defaults.
//!
//! Layers merge field by field — a value set in a higher layer replaces the
//! lower one; unset fields keep the lower layer's (or the default) value.
//! Unknown keys in any layer are hard errors naming the file and the key, so
//! typos never silently disable retention or output caps.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Errors from loading a configuration layer.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but cannot be read.
    #[error("cannot read config file {path}: {source}")]
    Read {
        /// The offending file.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The file is not valid TOML for the config grammar.
    #[error("invalid config file {path}: {source}")]
    Invalid {
        /// The offending file.
        path: PathBuf,
        /// The parse failure (names the key and position).
        source: toml::de::Error,
    },
}

/// Garbage-collection policy for stored runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Delete runs older than this many days.
    pub max_age_days: u64,
    /// Keep total spool size under this many megabytes (oldest first).
    pub max_total_mb: u64,
    /// Refuse to start a run when the state dir's filesystem has less than
    /// this many megabytes free (0 disables the check).
    pub min_free_mb: u64,
}

/// Output caps protecting agent context (bounded reads are a hard invariant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    /// Max events returned by `errors`/`warnings`.
    pub max_events: usize,
    /// Max lines returned by `tail`.
    pub max_tail_lines: usize,
    /// Max deduped templates shown in `show`.
    pub max_templates: usize,
}

/// Resolved configuration (all layers merged; every field is final).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Parser to use when the flag is absent (`auto` = detect).
    pub default_parser: String,
    /// Retention policy.
    pub retention: Retention,
    /// Output caps.
    pub output: Output,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_parser: "auto".to_string(),
            retention: Retention {
                max_age_days: 14,
                max_total_mb: 2048,
                min_free_mb: 100,
            },
            output: Output {
                max_events: 50,
                max_tail_lines: 100,
                max_templates: 20,
            },
        }
    }
}

impl Config {
    /// Loads and merges the two layers, `user_config` first (lower), then
    /// `project_config` (higher). Absent files contribute nothing; unreadable
    /// or invalid files are errors naming the file.
    pub fn load(project_config: &Path, user_config: &Path) -> Result<Config, ConfigError> {
        Config::load_layers([user_config, project_config].map(Path::to_path_buf))
    }

    /// Discovers the layers from conventional locations: the project's
    /// `<project_root>/.loglense/config.toml` and the user's
    /// `$HOME/.config/loglense/config.toml` (skipped when `HOME` is unset).
    pub fn discover(project_root: &Path) -> Result<Config, ConfigError> {
        let user_config = std::env::var_os("HOME").map(|home| {
            Path::new(&home)
                .join(".config")
                .join("loglense")
                .join("config.toml")
        });
        let project_config = project_root.join(".loglense").join("config.toml");
        let mut layers = user_config.into_iter().collect::<Vec<_>>();
        layers.push(project_config);
        Config::load_layers(layers)
    }

    fn load_layers(paths: impl IntoIterator<Item = PathBuf>) -> Result<Config, ConfigError> {
        let mut config = Config::default();
        for path in paths {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ConfigError::Read { path, source }),
            };
            let layer: Layer =
                toml::from_str(&text).map_err(|source| ConfigError::Invalid { path, source })?;
            config.apply(layer);
        }
        Ok(config)
    }

    /// Overlays `layer` onto `self` (present values replace, absent keep).
    fn apply(&mut self, layer: Layer) {
        if let Some(default_parser) = layer.default_parser {
            self.default_parser = default_parser;
        }
        if let Some(retention) = layer.retention {
            if let Some(max_age_days) = retention.max_age_days {
                self.retention.max_age_days = max_age_days;
            }
            if let Some(max_total_mb) = retention.max_total_mb {
                self.retention.max_total_mb = max_total_mb;
            }
            if let Some(min_free_mb) = retention.min_free_mb {
                self.retention.min_free_mb = min_free_mb;
            }
        }
        if let Some(output) = layer.output {
            if let Some(max_events) = output.max_events {
                self.output.max_events = max_events;
            }
            if let Some(max_tail_lines) = output.max_tail_lines {
                self.output.max_tail_lines = max_tail_lines;
            }
            if let Some(max_templates) = output.max_templates {
                self.output.max_templates = max_templates;
            }
        }
    }
}

/// One config file's contents: every field optional, unknown keys rejected.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct Layer {
    default_parser: Option<String>,
    retention: Option<RetentionLayer>,
    output: Option<OutputLayer>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct RetentionLayer {
    max_age_days: Option<u64>,
    max_total_mb: Option<u64>,
    min_free_mb: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct OutputLayer {
    max_events: Option<usize>,
    max_tail_lines: Option<usize>,
    max_templates: Option<usize>,
}
