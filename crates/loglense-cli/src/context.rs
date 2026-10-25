//! Per-invocation context: store resolution (docs/surfaces.md §1.1), config
//! layering, and run-id lookup.

use std::path::{Path, PathBuf};

use loglense_core::{Config, RunId};
use loglense_store::StoreError;

use crate::error::CliError;

/// The env override for the store location (`--dir` wins over it).
const STATE_ENV: &str = "LOGLENSE_STATE_DIR";

pub(crate) struct Ctx {
    pub json: bool,
    dir: Option<PathBuf>,
}

impl Ctx {
    pub(crate) fn new(json: bool, dir: Option<PathBuf>) -> Self {
        Self { json, dir }
    }

    /// Explicit store override: `--dir`, else a non-empty
    /// `LOGLENSE_STATE_DIR`.
    fn explicit(&self) -> Option<PathBuf> {
        self.dir.clone().or_else(|| {
            std::env::var_os(STATE_ENV)
                .filter(|raw| !raw.is_empty())
                .map(PathBuf::from)
        })
    }

    /// Store for read commands: the override (must exist), else walk-up
    /// discovery. Readers never create a store.
    pub(crate) fn reader_state(&self) -> Result<PathBuf, CliError> {
        if let Some(dir) = self.explicit() {
            return dir
                .canonicalize()
                .map_err(|source| CliError::io(&dir, source));
        }
        let cwd = current_dir()?;
        Ok(loglense_store::discover(&cwd, &home())?)
    }

    /// Store for writers (`run`): the override (created when missing), else
    /// discovery, else a new `.loglense/` in the working directory.
    pub(crate) fn writer_state(&self) -> Result<PathBuf, CliError> {
        if let Some(dir) = self.explicit() {
            return ensure_dir(&dir);
        }
        let cwd = current_dir()?;
        match loglense_store::discover(&cwd, &home()) {
            Ok(state) => Ok(state),
            Err(StoreError::StateDirNotFound { .. }) => {
                loglense_store::create(&cwd, &home(), false).map_err(unwritable_hint)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// The explicit override, if any (used by `init`).
    pub(crate) fn override_dir(&self) -> Option<PathBuf> {
        self.explicit()
    }
}

/// Store creation failed for lack of write access: say how to fix it.
fn unwritable_hint(err: StoreError) -> CliError {
    match err {
        StoreError::Io { path, source }
            if matches!(
                source.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            CliError::Usage(format!(
                "cannot create {}: {source}; run from a writable project directory, or point \
                 --dir / LOGLENSE_STATE_DIR at a writable store",
                path.display()
            ))
        }
        other => other.into(),
    }
}

/// `<state>/config.toml` over `$HOME/.config/loglense/config.toml` over
/// defaults; invalid files are errors naming file and field.
pub(crate) fn config(state: &Path) -> Result<Config, CliError> {
    let user = home().join(".config/loglense/config.toml");
    Ok(Config::load(&state.join("config.toml"), &user)?)
}

/// Resolves `<state>/runs/<id>/`. The id must have run-id syntax, so no
/// argument can address a path outside the store.
pub(crate) fn run_dir(state: &Path, id: &str) -> Result<PathBuf, CliError> {
    let unknown = || CliError::UnknownRun {
        id: id.to_string(),
        state: state.to_path_buf(),
    };
    let run_id: RunId = id.parse().map_err(|_| unknown())?;
    let dir = state.join(loglense_store::RUNS_DIR).join(run_id.as_str());
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(unknown())
    }
}

/// Creates `dir` (and parents) when missing; returns it canonicalized.
pub(crate) fn ensure_dir(dir: &Path) -> Result<PathBuf, CliError> {
    std::fs::create_dir_all(dir).map_err(|source| CliError::io(dir, source))?;
    dir.canonicalize()
        .map_err(|source| CliError::io(dir, source))
}

pub(crate) fn current_dir() -> Result<PathBuf, CliError> {
    std::env::current_dir().map_err(|source| CliError::io(".", source))
}

pub(crate) fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Epoch milliseconds now.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
