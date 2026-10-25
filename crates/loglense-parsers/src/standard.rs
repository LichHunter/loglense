//! The standard registry every surface uses (docs/parsers.md §3.5):
//! built-in bases and tools, then user packs, then project packs — later
//! sources shadow earlier ones by name. Broken packs are skipped and
//! reported, never fatal.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::pack::{load_pack_dir_quiet, PackError};
use crate::registry::{ParserRegistry, Source};
use crate::traits::Parser as _;
use crate::{bases, tools};

/// A pack file that failed to load.
#[derive(Debug)]
pub struct BrokenPack {
    /// The pack file.
    pub path: PathBuf,
    /// Where it was found.
    pub source: Source,
    /// Why it was skipped.
    pub error: PackError,
}

/// The registry plus the packs that were skipped.
pub struct LoadedRegistry {
    /// Built-ins + every valid pack.
    pub registry: ParserRegistry,
    /// Packs skipped as broken, in load order.
    pub broken: Vec<BrokenPack>,
}

/// Built-ins only.
pub fn builtin_registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    bases::register(&mut registry);
    tools::register(&mut registry);
    registry
}

/// Built-ins, then `user_packs` (when given), then `project_packs`.
pub fn standard_registry(project_packs: &Path, user_packs: Option<&Path>) -> LoadedRegistry {
    let mut loaded = LoadedRegistry {
        registry: builtin_registry(),
        broken: Vec::new(),
    };
    if let Some(dir) = user_packs {
        register_dir(&mut loaded, dir, Source::UserPack);
    }
    register_dir(&mut loaded, project_packs, Source::ProjectPack);
    loaded
}

fn register_dir(loaded: &mut LoadedRegistry, dir: &Path, source: Source) {
    for (path, result) in load_pack_dir_quiet(dir) {
        match result {
            Ok(pack) => {
                let name = pack.name().to_string();
                let extends = pack.extends().map(str::to_string);
                let pack = Arc::new(pack);
                loaded
                    .registry
                    .register(&name, move || Box::new((*pack).clone()), source, extends);
            }
            Err(error) => loaded.broken.push(BrokenPack {
                path,
                source,
                error,
            }),
        }
    }
}
