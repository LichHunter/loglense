//! TOML pack grammar, schema = 1 (docs/parsers.md §4): the no-recompile
//! extension path. Loading is strict (`deny_unknown_fields` everywhere),
//! compiles every regex at load — a bad pattern is a load-time error, never
//! a parse-time surprise — and a loaded [`Pack`] satisfies the module
//! contract like any built-in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use loglense_core::{Event, Kind, Severity};
use regex::Regex;
use serde::Deserialize;

use crate::traits::{FeedOutcome, LineCtx, ParseState, Parser};

/// Maximum rules per pack. Larger packs are rejected at load with a clear
/// error; packs are authored by hand and a four-digit rule count is far more
/// likely a generation bug than a real format grammar.
pub const MAX_RULES: usize = 1000;

/// The only grammar version accepted by this loader (docs/parsers.md §4.1).
pub const SCHEMA_VERSION: u32 = 1;

/// Pack loading failures. Every variant names the file; rule-regex failures
/// additionally name the rule id and the `pattern` field
/// (docs/parsers.md §3.4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// The pack file could not be read.
    #[error("{file}: {source}")]
    Io {
        /// Pack path as passed to [`load_pack`].
        file: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML for the grammar (parse error, unknown
    /// field, wrong type, unknown enum value, missing field).
    #[error("{file}: {source}")]
    Parse {
        /// Pack path.
        file: String,
        /// Underlying serde/TOML error.
        #[source]
        source: toml::de::Error,
    },
    /// `schema` is not [`SCHEMA_VERSION`].
    #[error("{file}: unsupported schema version {found}")]
    UnsupportedSchema {
        /// Pack path.
        file: String,
        /// The rejected value.
        found: u32,
    },
    /// `detect.min_score` is below 1.
    #[error("{file}: detect.min_score must be >= 1, found {found}")]
    MinScore {
        /// Pack path.
        file: String,
        /// The rejected value.
        found: u32,
    },
    /// More than [`MAX_RULES`] rules.
    #[error("{file}: too many rules ({count}); the cap is {MAX_RULES}")]
    TooManyRules {
        /// Pack path.
        file: String,
        /// Rule count found in the file.
        count: usize,
    },
    /// A rule pattern failed to compile.
    #[error("{file}:rule[id={id}].pattern: {source}")]
    BadRegex {
        /// Pack path.
        file: String,
        /// The offending rule's id.
        id: String,
        /// Underlying regex error.
        #[source]
        source: regex::Error,
    },
    /// The `detect.pattern` failed to compile.
    #[error("{file}:detect.pattern: {source}")]
    BadDetectRegex {
        /// Pack path.
        file: String,
        /// Underlying regex error.
        #[source]
        source: regex::Error,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackToml {
    schema: u32,
    name: String,
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    detect: Option<DetectToml>,
    #[serde(default)]
    rule: Vec<RuleToml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetectToml {
    pattern: String,
    min_score: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleToml {
    id: String,
    severity: Severity,
    pattern: String,
    message: String,
    kind: Kind,
}

/// A compiled rule: ordered single-line pattern plus the event template
/// fields. Earlier rules win among a pack's own rules (docs/parsers.md
/// §4.1).
#[derive(Debug)]
pub struct CompiledRule {
    /// Stable rule identifier; a derived rule with the same id overrides the
    /// inherited one (rule-id override lands with the T7-T10 rule-merging
    /// parsers).
    pub id: String,
    /// Severity for events from this rule.
    pub severity: Severity,
    /// Rendering kind for events from this rule.
    pub kind: Kind,
    /// Compiled line pattern (linear-time `regex` crate dialect).
    pub pattern: Regex,
    /// Message template: `$1`…`$9` interpolate numbered captures, `$$` is a
    /// literal dollar (docs/parsers.md §4.2).
    pub message: String,
}

#[derive(Debug)]
struct DetectSpec {
    pattern: Regex,
    min_score: u32,
}

/// A loaded TOML pack implementing the module contract. Detection is
/// synthesized per docs/parsers.md §3.3 item 5: `m` matching sample lines
/// and threshold `min_score` score `0` when `m = 0`, else
/// `min(100, ceil(100·m / min_score))`.
#[derive(Clone)]
pub struct Pack {
    name: &'static str,
    extends: Option<&'static str>,
    description: Option<String>,
    detect: Option<Arc<DetectSpec>>,
    rules: Arc<Vec<CompiledRule>>,
}

impl std::fmt::Debug for Pack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pack")
            .field("name", &self.name)
            .field("extends", &self.extends)
            .field("rules", &self.rules.len())
            .finish_non_exhaustive()
    }
}

impl Pack {
    /// Compiled rules in file order.
    pub fn rules(&self) -> &[CompiledRule] {
        &self.rules
    }

    /// Free-form author note from the header.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

impl Parser for Pack {
    fn name(&self) -> &'static str {
        self.name
    }

    fn extends(&self) -> Option<&'static str> {
        self.extends
    }

    fn detect(&self, sample: &[&str]) -> u8 {
        let Some(spec) = &self.detect else {
            return 0;
        };
        let matches = sample
            .iter()
            .filter(|line| spec.pattern.is_match(line))
            .count() as u32;
        if matches == 0 {
            return 0;
        }
        let score = (100 * matches).div_ceil(spec.min_score);
        u8::try_from(score.min(100)).unwrap_or(100)
    }

    fn new_run(&self) -> Box<dyn ParseState> {
        Box::new(PackState {
            rules: Arc::clone(&self.rules),
        })
    }
}

/// Stateless intake for a [`Pack`]: first matching rule claims the line with
/// one interpolated event; no match passes. Pack rules are single-line —
/// multi-line blocks come from inherited built-in bases in the chain
/// (docs/parsers.md §4.4 boundary).
pub struct PackState {
    rules: Arc<Vec<CompiledRule>>,
}

impl ParseState for PackState {
    fn feed(&mut self, line: &str, ctx: &LineCtx) -> FeedOutcome {
        for rule in self.rules.iter() {
            if let Some(caps) = rule.pattern.captures(line) {
                let mut message = String::new();
                caps.expand(&rule.message, &mut message);
                return FeedOutcome::Claimed(vec![Event {
                    stream: ctx.stream,
                    severity: rule.severity,
                    kind: rule.kind,
                    message,
                    lines: vec![ctx.line_no],
                    ..Event::default()
                }]);
            }
        }
        FeedOutcome::Passed
    }

    fn finish(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

/// Load and validate one pack file. The header's `name`/`extends` live for
/// the process (the trait returns `&'static str`); packs load once per
/// process, so the leak is bounded by the number of pack files.
pub fn load_pack(path: &Path) -> Result<Pack, PackError> {
    let file = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        file: file.clone(),
        source,
    })?;
    let raw: PackToml = toml::from_str(&text).map_err(|source| PackError::Parse {
        file: file.clone(),
        source,
    })?;
    if raw.schema != SCHEMA_VERSION {
        return Err(PackError::UnsupportedSchema {
            file,
            found: raw.schema,
        });
    }
    if raw.rule.len() > MAX_RULES {
        return Err(PackError::TooManyRules {
            file,
            count: raw.rule.len(),
        });
    }
    let detect = match raw.detect {
        Some(detect) if detect.min_score < 1 => {
            return Err(PackError::MinScore {
                file,
                found: detect.min_score,
            });
        }
        Some(detect) => Some(Arc::new(DetectSpec {
            pattern: compile(&file, None, &detect.pattern)?,
            min_score: detect.min_score,
        })),
        None => None,
    };
    let mut rules = Vec::with_capacity(raw.rule.len());
    for rule in raw.rule {
        rules.push(CompiledRule {
            pattern: compile(&file, Some(&rule.id), &rule.pattern)?,
            id: rule.id,
            severity: rule.severity,
            kind: rule.kind,
            message: rule.message,
        });
    }
    Ok(Pack {
        name: Box::leak(raw.name.into_boxed_str()),
        extends: raw
            .extends
            .map(|extends| &*Box::leak(extends.into_boxed_str())),
        description: raw.description,
        detect,
        rules: Arc::new(rules),
    })
}

/// Load every `*.toml` pack in `dir`, sorted by file name for deterministic
/// load order. A missing directory is simply no packs; each broken pack is
/// warned about on stderr naming the file and skipped — a bad pack never
/// kills a run (docs/parsers.md §3.4). Per-file results are returned so
/// `parsers validate` can report them verbatim.
pub fn load_pack_dir(dir: &Path) -> Vec<(PathBuf, Result<Pack, PackError>)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let result = load_pack(&path);
            if let Err(err) = &result {
                eprintln!("warning: skipping broken pack {}: {err}", path.display());
            }
            (path, result)
        })
        .collect()
}

/// Compile one pattern at load; rule patterns get the `rule[id=…].pattern`
/// context, the detect pattern gets `detect.pattern`.
fn compile(file: &str, rule_id: Option<&str>, pattern: &str) -> Result<Regex, PackError> {
    Regex::new(pattern).map_err(|source| match rule_id {
        Some(id) => PackError::BadRegex {
            file: file.to_string(),
            id: id.to_string(),
            source,
        },
        None => PackError::BadDetectRegex {
            file: file.to_string(),
            source,
        },
    })
}
