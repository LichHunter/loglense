//! `notify tmux | exec` — ping adapters designed to run AS a completion
//! callback (docs/surfaces.md §4.3, D-011). They read the callback context
//! the supervisor provides: `LOGLENSE_*` env vars and the summary JSON on
//! stdin.

use std::io::Read as _;
use std::process::Command;

use crate::cli::NotifyCmd;
use crate::error::{CliError, EXIT_USAGE};

/// Default tmux message layout.
const DEFAULT_FMT: &str =
    "loglense: run {id} {status} (exit {exit}, {errors} errors, {warnings} warnings)";
/// The only fields a message may contain (no arbitrary interpolation).
const FIELDS: [&str; 5] = ["id", "status", "exit", "errors", "warnings"];
/// Most summary bytes read from stdin.
const MAX_STDIN: u64 = 1024 * 1024;

pub(crate) fn notify(cmd: NotifyCmd) -> Result<u8, CliError> {
    match cmd {
        NotifyCmd::Tmux {
            pane,
            msg_fmt,
            no_enter,
        } => tmux(&pane, msg_fmt.as_deref().unwrap_or(DEFAULT_FMT), !no_enter),
        NotifyCmd::Exec { cmd } => exec(&cmd),
    }
}

/// The callback context as message fields.
struct Context {
    id: String,
    status: String,
    exit: String,
    errors: String,
    warnings: String,
}

impl Context {
    fn from_callback_env() -> Result<Self, CliError> {
        let id = std::env::var("LOGLENSE_RUN_ID").map_err(|_| {
            CliError::Usage(
                "`loglense notify` runs as a completion callback: LOGLENSE_RUN_ID is not set \
                 (use it via `--on-complete 'loglense notify …'`)"
                    .to_string(),
            )
        })?;
        let mut stdin = Vec::new();
        let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut stdin);
        let summary: serde_json::Value = serde_json::from_slice(&stdin).unwrap_or_default();
        // Zero counts are omitted from the summary: absent means 0; only a
        // missing summary leaves the counts unknown (`?`).
        let count = |key: &str| summary["severities"][key].as_u64().unwrap_or(0).to_string();
        Ok(Self {
            id,
            status: std::env::var("LOGLENSE_STATUS").unwrap_or_else(|_| "?".to_string()),
            exit: std::env::var("LOGLENSE_EXIT_CODE").unwrap_or_else(|_| "-".to_string()),
            errors: if summary.is_null() {
                "?".to_string()
            } else {
                count("error")
            },
            warnings: if summary.is_null() {
                "?".to_string()
            } else {
                count("warning")
            },
        })
    }

    fn field(&self, name: &str) -> &str {
        match name {
            "id" => &self.id,
            "status" => &self.status,
            "exit" => &self.exit,
            "errors" => &self.errors,
            _ => &self.warnings,
        }
    }
}

/// Renders `fmt`, allowing only the documented `{field}` placeholders; the
/// result is one line with control characters replaced by spaces.
fn render(fmt: &str, ctx: &Context) -> Result<String, CliError> {
    let mut out = String::new();
    let mut rest = fmt;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(CliError::Usage(format!(
                "unclosed `{{` in --msg-fmt `{fmt}`"
            )));
        };
        let name = &after[..close];
        if !FIELDS.contains(&name) {
            return Err(CliError::Usage(format!(
                "unknown field `{{{name}}}` in --msg-fmt; allowed: {}",
                FIELDS.map(|f| format!("{{{f}}}")).join(" ")
            )));
        }
        out.push_str(ctx.field(name));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect())
}

fn tmux(pane: &str, fmt: &str, enter: bool) -> Result<u8, CliError> {
    let ctx = Context::from_callback_env()?;
    let message = render(fmt, &ctx)?;
    // `-l`: the message is typed literally, never parsed as key names.
    let mut sends = vec![vec!["send-keys", "-t", pane, "-l", message.as_str()]];
    if enter {
        sends.push(vec!["send-keys", "-t", pane, "Enter"]);
    }
    for args in sends {
        let status = Command::new("tmux")
            .args(&args)
            .status()
            .map_err(|source| CliError::io("tmux", source))?;
        if !status.success() {
            eprintln!("loglense: tmux {} failed: {status}", args.join(" "));
            return Ok(EXIT_USAGE);
        }
    }
    Ok(0)
}

fn exec(cmd: &[String]) -> Result<u8, CliError> {
    let Some((program, args)) = cmd.split_first() else {
        return Err(CliError::Usage("notify exec needs a command".to_string()));
    };
    // Inherits the callback env and stdin (summary JSON) unchanged.
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|source| CliError::io(program, source))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            id: "20261025-A3F8C2".into(),
            status: "exited".into(),
            exit: "3".into(),
            errors: "2".into(),
            warnings: "0".into(),
        }
    }

    #[test]
    fn renders_only_documented_fields_on_one_line() {
        assert_eq!(
            render(DEFAULT_FMT, &ctx()).unwrap(),
            "loglense: run 20261025-A3F8C2 exited (exit 3, 2 errors, 0 warnings)"
        );
        assert_eq!(
            render("{id}:{exit}\n", &ctx()).unwrap(),
            "20261025-A3F8C2:3 "
        );
        assert!(render("{id} {cmd}", &ctx()).is_err(), "no arbitrary fields");
        assert!(render("{id", &ctx()).is_err());
    }
}
