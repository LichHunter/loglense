//! Clap definitions for every subcommand (docs/surfaces.md §2).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Run any command in the background and query its logs in agent-sized
/// pieces. `loglense <cmd...>` is shorthand for `loglense run -- <cmd...>`.
#[derive(Debug, Parser)]
#[command(
    name = "loglense",
    version,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    /// Use this `.loglense` store instead of walk-up discovery (also:
    /// LOGLENSE_STATE_DIR; the flag wins).
    #[arg(long, global = true, value_name = "PATH")]
    pub dir: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Wrap a command: start it detached, print its run id, return at once.
    Run(RunArgs),
    /// Create `.loglense/` here and gitignore it.
    Init {
        /// Allow creating the store directly in `$HOME` or `/`.
        #[arg(long)]
        force: bool,
    },
    /// List runs, newest first.
    Ls {
        /// Show at most this many runs.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..))]
        max: u64,
        /// Only runs in this state.
        #[arg(long, value_enum)]
        status: Option<StatusFilter>,
    },
    /// One run: metadata, summary and top error templates.
    Show {
        /// Run id.
        run_id: String,
        /// Show at most this many templates (default: config, 20).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max: Option<u64>,
    },
    /// Error events of a run, oldest first.
    Errors(EventsArgs),
    /// Warning events of a run, oldest first.
    Warnings(EventsArgs),
    /// Last lines of a run's captured output.
    Tail {
        /// Run id.
        run_id: String,
        /// Which output to read.
        #[arg(long, value_enum, default_value_t = StreamArg::Merged)]
        stream: StreamArg,
        /// Show at most this many lines (default: config, 100).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        lines: Option<u64>,
        /// Not supported in v1 (rejected with an error).
        #[arg(long, short = 'f', hide = true)]
        follow: bool,
    },
    /// Block until a run finishes, dies, or the timeout elapses.
    Wait {
        /// Run id.
        run_id: String,
        /// Give up after this many seconds (exit 124); default: no limit.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<f64>,
    },
    /// Terminate a run (TERM, then KILL after 5 s).
    Kill {
        /// Run id.
        run_id: String,
    },
    /// Delete old runs past retention.
    Gc {
        /// Remove runs older than this many days (default: config, 14).
        #[arg(long, value_name = "DAYS")]
        max_age: Option<u64>,
        /// Keep the store under this many MB, oldest first (default:
        /// config, 2048).
        #[arg(long, value_name = "MB")]
        max_total: Option<u64>,
    },
    /// Parser authoring tools (not implemented yet).
    #[command(hide = true)]
    Parsers(Pending),
    /// Agent feedback tools (not implemented yet).
    #[command(hide = true)]
    Feedback(Pending),
    /// Completion ping adapters (not implemented yet).
    #[command(hide = true)]
    Notify(Pending),
    /// Stdio MCP server (not implemented yet).
    #[command(hide = true)]
    Mcp(Pending),
    /// Print the version.
    #[command(hide = true)]
    Version,
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Parser module for this run (default: config, auto-detect).
    #[arg(long)]
    pub parser: Option<String>,
    /// The command to run.
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "CMD"
    )]
    pub cmd: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct EventsArgs {
    /// Run id.
    pub run_id: String,
    /// Show at most this many events (default: config, 50).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub max: Option<u64>,
}

/// Placeholder argument sink for subcommands later tasks implement.
#[derive(Debug, Args)]
pub(crate) struct Pending {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum StatusFilter {
    Starting,
    Running,
    Exited,
    Signaled,
    FailedToStart,
    /// Orphaned or lost.
    Interrupted,
    Orphaned,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum StreamArg {
    Out,
    Err,
    Merged,
}
