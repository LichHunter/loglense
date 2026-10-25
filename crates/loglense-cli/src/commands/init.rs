//! `init`: create the store in the working directory and gitignore it.
//! Prints every action; idempotent; never edits anything but the store and
//! (append-only) the repository root `.gitignore`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::context::{self, Ctx};
use crate::error::CliError;
use crate::output;

/// The line appended to the repository `.gitignore`.
const IGNORE_LINE: &str = ".loglense/";
/// Lines that already ignore the store (any of them counts as present).
const IGNORE_EQUIVALENTS: [&str; 4] = [".loglense/", ".loglense", "/.loglense/", "/.loglense"];

#[derive(serde::Serialize)]
struct Action {
    /// `created`, `exists`, `appended`, `unchanged` or `skipped`.
    action: &'static str,
    path: PathBuf,
    detail: String,
}

pub(crate) fn init(ctx: &Ctx, force: bool) -> Result<u8, CliError> {
    let cwd = context::current_dir()?;
    let mut actions = Vec::new();
    let state = match ctx.override_dir() {
        Some(dir) => {
            let existed = dir.is_dir();
            let state = context::ensure_dir(&dir)?;
            actions.push(store_action(existed, &state));
            state
        }
        None => {
            let target = cwd.join(loglense_store::STATE_DIR_NAME);
            let existed = target.is_dir();
            let self_ignore_existed = target.join(".gitignore").is_file();
            let state = loglense_store::create(&cwd, &context::home(), force)?;
            actions.push(store_action(existed, &state));
            actions.push(Action {
                action: if self_ignore_existed {
                    "exists"
                } else {
                    "created"
                },
                path: state.join(".gitignore"),
                detail: "self-ignore (`*`): git never commits spool contents".to_string(),
            });
            state
        }
    };
    actions.push(repo_gitignore(&cwd)?);
    if ctx.json {
        output::json(&serde_json::json!({ "state_dir": state, "actions": actions }));
    } else {
        for action in &actions {
            println!(
                "{:<9} {}  ({})",
                action.action,
                action.path.display(),
                action.detail
            );
        }
    }
    Ok(0)
}

fn store_action(existed: bool, state: &Path) -> Action {
    Action {
        action: if existed { "exists" } else { "created" },
        path: state.to_path_buf(),
        detail: "loglense store".to_string(),
    }
}

/// Appends `.loglense/` to the git root's `.gitignore` when absent.
fn repo_gitignore(cwd: &Path) -> Result<Action, CliError> {
    let Some(root) = cwd
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
    else {
        return Ok(Action {
            action: "skipped",
            path: cwd.to_path_buf(),
            detail: "not inside a git repository; no repository .gitignore touched".to_string(),
        });
    };
    let path = root.join(".gitignore");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(CliError::io(&path, source)),
    };
    if existing
        .lines()
        .any(|line| IGNORE_EQUIVALENTS.contains(&line.trim()))
    {
        return Ok(Action {
            action: "unchanged",
            path,
            detail: "already ignores .loglense/".to_string(),
        });
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| CliError::io(&path, source))?;
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    file.write_all(format!("{separator}{IGNORE_LINE}\n").as_bytes())
        .map_err(|source| CliError::io(&path, source))?;
    Ok(Action {
        action: "appended",
        path,
        detail: format!("added `{IGNORE_LINE}`"),
    })
}
