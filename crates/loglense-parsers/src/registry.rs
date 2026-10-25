//! Parser registry (docs/parsers.md §3): name → constructor with source
//! precedence, `extends` resolution by name DFS at load time, and chain
//! construction. Cycles and unknown parents are load-time errors, never
//! parse-time surprises.

use std::fmt;
use std::sync::Arc;

use crate::chain::Chain;
use crate::traits::Parser;

/// Name of the always-last fallback module (docs/parsers.md §3.2): the chain
/// builder appends it after the flattened ancestors unless it is already
/// present.
pub const GENERIC: &str = "generic";

/// Where a parser registration came from. Ordered by shadowing precedence:
/// project > user > built-in (docs/parsers.md §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Compiled into `loglense-parsers`.
    Builtin,
    /// `~/.config/loglense/formats.d/*.toml`.
    UserPack,
    /// `.loglense/formats.d/*.toml`.
    ProjectPack,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Builtin => "built-in",
            Self::UserPack => "user pack",
            Self::ProjectPack => "project pack",
        })
    }
}

/// Registry construction failures (docs/parsers.md §3.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// `build_chain` target is not registered.
    #[error("unknown parser {name:?}")]
    UnknownParser {
        /// The requested parser name.
        name: String,
    },
    /// `extends` references a name that is not registered.
    #[error(
        "unknown extends: child {child:?} references parent {parent:?}, which is not registered"
    )]
    UnknownExtends {
        /// The missing parent name.
        parent: String,
        /// The child that referenced it.
        child: String,
    },
    /// `extends` chain revisits a name; `cycle` reads `a → b → a`.
    #[error("extends cycle: {cycle}")]
    ExtendsCycle {
        /// Full cycle path, first node repeated at the end.
        cycle: String,
    },
}

type Ctor = Arc<dyn Fn() -> Box<dyn Parser> + Send + Sync>;

struct Entry {
    name: String,
    ctor: Ctor,
    source: Source,
    extends: Option<String>,
}

/// One registration row, as surfaced by [`ParserRegistry::registrations`]
/// (used by `parsers list` and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration<'a> {
    /// Module name (registry key).
    pub name: &'a str,
    /// Registration origin.
    pub source: Source,
    /// Declared parent, if any.
    pub extends: Option<&'a str>,
}

/// Ordered registry of parser modules. Built-ins register first, then user
/// packs, then project packs (docs/parsers.md §3.5).
#[derive(Default)]
pub struct ParserRegistry {
    entries: Vec<Entry>,
}

impl ParserRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a parser under `name`.
    ///
    /// `ctor` produces a fresh parser instance per chain/run — a plain
    /// `fn` pointer for built-ins, a cloning closure for loaded packs.
    /// Duplicate names resolve by precedence: a higher [`Source`] replaces a
    /// lower one in place (info logged to stderr); an equal source means the
    /// later registration wins (warning logged); a lower source is ignored.
    pub fn register(
        &mut self,
        name: &str,
        ctor: impl Fn() -> Box<dyn Parser> + Send + Sync + 'static,
        source: Source,
        extends: Option<String>,
    ) {
        let ctor: Ctor = Arc::new(ctor);
        let entry = Entry {
            name: name.to_string(),
            ctor,
            source,
            extends,
        };
        let Some(slot) = self.entries.iter_mut().find(|e| e.name == entry.name) else {
            self.entries.push(entry);
            return;
        };
        let old = slot.source;
        if source > old {
            eprintln!("info: parser '{}': {} replaces {}", entry.name, source, old);
            *slot = entry;
        } else if source == old {
            eprintln!(
                "warning: parser '{}': duplicate {} registration; the later one wins",
                entry.name, source
            );
            *slot = entry;
        } else {
            eprintln!(
                "info: parser '{}': {} ignored; {} keeps the name",
                entry.name, source, old
            );
        }
    }

    /// Registration rows in registration order (built-ins first, packs in
    /// load order).
    pub fn registrations(&self) -> Vec<Registration<'_>> {
        self.entries
            .iter()
            .map(|e| Registration {
                name: &e.name,
                source: e.source,
                extends: e.extends.as_deref(),
            })
            .collect()
    }

    /// Registered names in registration order.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Detection score of a registered parser over a sample (constructs a
    /// throwaway instance). `None` when `name` is not registered.
    pub fn detect_of(&self, name: &str, sample: &[&str]) -> Option<u8> {
        self.find(name).map(|e| (e.ctor)().detect(sample))
    }

    /// Validate every `extends` declaration: unknown parents and cycles are
    /// load-time errors (docs/parsers.md §3.2).
    pub fn resolve(&self) -> Result<(), RegistryError> {
        for entry in &self.entries {
            self.walk_ancestors(&entry.name)?;
        }
        Ok(())
    }

    /// Build the runtime chain for `name`: the selected parser first, then
    /// its ancestors ordered most-derived to base-most, then `generic` last
    /// (appended only when registered and not already present). Selecting
    /// `generic` explicitly yields just `generic`.
    pub fn build_chain(&self, name: &str) -> Result<Chain, RegistryError> {
        if self.find(name).is_none() {
            return Err(RegistryError::UnknownParser {
                name: name.to_string(),
            });
        }
        let order = self.walk_ancestors(name)?;
        let mut parsers: Vec<Box<dyn Parser>> = Vec::with_capacity(order.len() + 1);
        for member in &order {
            if let Some(entry) = self.find(member) {
                parsers.push((entry.ctor)());
            }
        }
        if name != GENERIC && !order.iter().any(|m| m == GENERIC) {
            if let Some(generic) = self.find(GENERIC) {
                parsers.push((generic.ctor)());
            }
        }
        Ok(Chain::new(parsers))
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Walk `name` up its `extends` chain. Returns `[name, parent, …,
    /// base-most]`; errors on unknown parents and cycles.
    fn walk_ancestors(&self, name: &str) -> Result<Vec<String>, RegistryError> {
        let mut path = vec![name.to_string()];
        let mut child = name.to_string();
        while let Some(parent) = self.find(&child).and_then(|e| e.extends.clone()) {
            if let Some(at) = path.iter().position(|n| *n == parent) {
                let mut cycle: Vec<String> = path[at..].to_vec();
                cycle.push(parent);
                return Err(RegistryError::ExtendsCycle {
                    cycle: cycle.join(" → "),
                });
            }
            if self.find(&parent).is_none() {
                return Err(RegistryError::UnknownExtends { parent, child });
            }
            path.push(parent.clone());
            child = parent;
        }
        Ok(path)
    }
}
