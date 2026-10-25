//! Store resolution for the MCP server. The server's own working directory
//! is never consulted (docs/surfaces.md §5.3): stores come from an explicit
//! `cwd`, from `LOGLENSE_STATE_DIR`, or from the bounded set of stores this
//! server already resolved for earlier calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use loglense_core::RunId;
use loglense_store::StoreError;

/// Remembered run → store mappings (bounded: oldest half dropped at cap).
const MAX_KNOWN_RUNS: usize = 4096;
/// Remembered stores scanned for unknown run ids.
const MAX_KNOWN_STORES: usize = 64;

#[derive(Default)]
pub(crate) struct KnownStores {
    inner: Mutex<Known>,
}

#[derive(Default)]
struct Known {
    runs: HashMap<String, PathBuf>,
    stores: Vec<PathBuf>,
}

impl KnownStores {
    fn remember_store(&self, state: &Path) {
        if let Ok(mut known) = self.inner.lock() {
            if !known.stores.iter().any(|s| s == state) {
                if known.stores.len() == MAX_KNOWN_STORES {
                    known.stores.remove(0);
                }
                known.stores.push(state.to_path_buf());
            }
        }
    }

    pub(crate) fn remember_run(&self, run_id: &str, state: &Path) {
        self.remember_store(state);
        if let Ok(mut known) = self.inner.lock() {
            if known.runs.len() >= MAX_KNOWN_RUNS {
                known.runs.clear();
            }
            known.runs.insert(run_id.to_string(), state.to_path_buf());
        }
    }

    fn candidates(&self, run_id: &str) -> Vec<PathBuf> {
        let Ok(known) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = known.runs.get(run_id).cloned().into_iter().collect();
        out.extend(known.stores.iter().rev().cloned());
        out
    }

    /// Store for a writer (`start_run`) at `cwd`: the env override, else
    /// discovery from `cwd`, else a new `.loglense/` in `cwd`.
    pub(crate) fn writer_state(&self, cwd: &str) -> Result<PathBuf, String> {
        let cwd = absolute_dir(cwd)?;
        let state = match env_override() {
            Some(dir) => {
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                dir.canonicalize()
                    .map_err(|e| format!("{}: {e}", dir.display()))?
            }
            None => match loglense_store::discover(&cwd, &home()) {
                Ok(state) => state,
                Err(StoreError::StateDirNotFound { .. }) => {
                    loglense_store::create(&cwd, &home(), false).map_err(|e| e.to_string())?
                }
                Err(err) => return Err(err.to_string()),
            },
        };
        self.remember_store(&state);
        Ok(state)
    }

    /// Store for a reader at `cwd`: the env override, else discovery.
    pub(crate) fn reader_state(&self, cwd: &str) -> Result<PathBuf, String> {
        let cwd = absolute_dir(cwd)?;
        let state = match env_override() {
            Some(dir) => dir
                .canonicalize()
                .map_err(|e| format!("{}: {e}", dir.display()))?,
            None => loglense_store::discover(&cwd, &home()).map_err(|e| e.to_string())?,
        };
        self.remember_store(&state);
        Ok(state)
    }

    /// Finds `runs/<run_id>/`: under `cwd`'s store when given, else under
    /// the env override or any store this server has seen.
    pub(crate) fn locate(&self, run_id: &str, cwd: Option<&str>) -> Result<PathBuf, String> {
        let id: RunId = run_id
            .parse()
            .map_err(|_| format!("invalid run id `{run_id}` (expected e.g. 20261025-A3F8C2)"))?;
        let mut stores = match cwd {
            Some(cwd) => vec![self.reader_state(cwd)?],
            None => env_override()
                .and_then(|dir| dir.canonicalize().ok())
                .into_iter()
                .collect(),
        };
        if cwd.is_none() {
            stores.extend(self.candidates(run_id));
        }
        for state in &stores {
            let run_dir = state.join(loglense_store::RUNS_DIR).join(id.as_str());
            if run_dir.is_dir() {
                self.remember_run(run_id, state);
                return Ok(run_dir);
            }
        }
        Err(match (cwd, stores.first()) {
            (Some(_), Some(state)) => {
                format!("unknown run id `{run_id}` in {}", state.display())
            }
            _ => format!(
                "unknown run id `{run_id}`: pass `cwd` (the run's absolute project directory) — this server has not seen its store yet"
            ),
        })
    }
}

fn absolute_dir(cwd: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(cwd);
    if !path.is_absolute() {
        return Err(format!(
            "`cwd` must be an absolute project directory, got `{cwd}`"
        ));
    }
    if !path.is_dir() {
        return Err(format!("`cwd` {cwd} is not a directory"));
    }
    path.canonicalize().map_err(|e| format!("{cwd}: {e}"))
}

fn env_override() -> Option<PathBuf> {
    std::env::var_os("LOGLENSE_STATE_DIR")
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}
