//! `.loglense/` state-directory discovery and creation (D-002).
//!
//! Git-style walk-up from a start directory to the nearest enclosing
//! `.loglense/`, never crossing above the git root or the user's home
//! directory. Writers create `.loglense/` in the start directory when no
//! existing one is found, and `LOGLENSE_STATE_DIR` short-circuits discovery
//! entirely.

use std::path::{Path, PathBuf};

use crate::StoreError;

/// Name of the state directory at a project root.
pub const STATE_DIR_NAME: &str = ".loglense";

/// Walks up from `start` to the first directory containing `.loglense/`.
///
/// The walk never crosses above the git root (nearest ancestor with a
/// `.git`) or `home`, whichever comes first; with neither present it stops
/// at the filesystem root. A `.loglense` found via symlink is canonicalized
/// once. A `.loglense` that is a regular file is
/// [`StoreError::StateDirIsFile`][crate::StoreError::StateDirIsFile].
pub fn discover(start: &Path, home: &Path) -> Result<PathBuf, StoreError> {
    let limit = walk_limit(start, home);
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(STATE_DIR_NAME);
        match std::fs::metadata(&candidate) {
            Ok(md) if md.is_file() => {
                return Err(StoreError::StateDirIsFile { path: candidate });
            }
            Ok(_) => {
                return candidate.canonicalize().map_err(|source| StoreError::Io {
                    path: candidate,
                    source,
                });
            }
            Err(_) => {}
        }
        if Some(d) == limit.as_deref() {
            break;
        }
        dir = d.parent();
    }
    Err(StoreError::StateDirNotFound {
        start: start.to_path_buf(),
    })
}

/// Creates `<start>/.loglense/` plus a self-ignore `.gitignore` (`*\n`).
///
/// Refuses (without `force`) to create inside `/` or `home`. An existing
/// `.gitignore` is left untouched; a `.loglense` regular file is a typed
/// error. The returned path is canonicalized once.
pub fn create(start: &Path, home: &Path, force: bool) -> Result<PathBuf, StoreError> {
    if !force && (start == Path::new("/") || start == home) {
        return Err(StoreError::RefusedCreate {
            parent: start.to_path_buf(),
        });
    }
    let state = start.join(STATE_DIR_NAME);
    if let Ok(md) = std::fs::metadata(&state) {
        if md.is_file() {
            return Err(StoreError::StateDirIsFile { path: state });
        }
    }
    std::fs::create_dir_all(&state).map_err(|source| StoreError::Io {
        path: state.clone(),
        source,
    })?;
    let gitignore = state.join(".gitignore");
    if !gitignore.is_file() {
        std::fs::write(&gitignore, "*\n").map_err(|source| StoreError::Io {
            path: gitignore,
            source,
        })?;
    }
    state.canonicalize().map_err(|source| StoreError::Io {
        path: state,
        source,
    })
}

/// Resolves the state directory for writers.
///
/// `LOGLENSE_STATE_DIR` short-circuits both discovery and creation (created
/// when missing, resolved canonical); otherwise discovery, otherwise
/// [`create`] in `start`.
pub fn resolve(start: &Path, home: &Path) -> Result<PathBuf, StoreError> {
    if let Some(raw) = std::env::var_os("LOGLENSE_STATE_DIR") {
        let path = PathBuf::from(raw);
        std::fs::create_dir_all(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        return path
            .canonicalize()
            .map_err(|source| StoreError::Io { path, source });
    }
    match discover(start, home) {
        Ok(dir) => Ok(dir),
        Err(StoreError::StateDirNotFound { .. }) => create(start, home, false),
        Err(other) => Err(other),
    }
}

/// [`resolve`] with `home` taken from the environment (`$HOME`, falling
/// back to `/` when unset) — the production entry point.
pub fn resolve_from(start: &Path) -> Result<PathBuf, StoreError> {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    resolve(start, &home)
}

/// The highest directory [`discover`] may examine: the shallower of the
/// nearest git root at-or-above `start` and `home` (when `home` is an
/// ancestor of `start`). `None` means: walk to the filesystem root.
fn walk_limit(start: &Path, home: &Path) -> Option<PathBuf> {
    let mut limit: Option<PathBuf> = None;
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        if d.join(".git").symlink_metadata().is_ok() {
            limit = Some(d.to_path_buf());
            break;
        }
        dir = d.parent();
    }
    let home_shallower = limit
        .as_ref()
        .is_none_or(|l| home.components().count() < l.components().count());
    if start.starts_with(home) && home_shallower {
        limit = Some(home.to_path_buf());
    }
    limit
}
