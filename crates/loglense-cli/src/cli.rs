//! Clap definitions for every subcommand (docs/surfaces.md §2).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Run any command in the background and query its logs in agent-sized
/// pieces. `loglense <cmd...>` is shorthand for `loglense run -- <cmd...>`.
#[derive(Debug, Parser)]
#[command(
    name = "loglense",
    version = env!("LOGLENSE_VERSION"),
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
        status: Option<StatusArg>,
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
    /// Parser authoring: list modules, dry-run or validate TOML packs.
    #[command(subcommand)]
    Parsers(ParsersCmd),
    /// Agent feedback: record parser problems, triage, promote to fixtures.
    #[command(subcommand)]
    Feedback(FeedbackCmd),
    /// Ping adapters to use as an --on-complete callback target.
    #[command(subcommand)]
    Notify(NotifyCmd),
    /// Serve the MCP tools over stdio (for agent hosts; see .mcp.json).
    Mcp,
    /// Print the version.
    #[command(hide = true)]
    Version,
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Parser module for this run (default: config, auto-detect).
    #[arg(long)]
    pub parser: Option<String>,
    /// Command the supervisor runs (via `sh -c`) when the run finishes:
    /// context in LOGLENSE_* env vars, summary JSON on stdin.
    #[arg(long, value_name = "CMD")]
    pub on_complete: Option<String>,
    /// Kill the callback after this many milliseconds.
    #[arg(
        long,
        value_name = "MS",
        default_value_t = 30_000,
        requires = "on_complete"
    )]
    pub on_complete_timeout: u64,
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

#[derive(Debug, Subcommand)]
pub(crate) enum ParsersCmd {
    /// Registered parsers: built-ins, user and project packs.
    List,
    /// Dry-run a pack (or a registered parser's chain) over a sample log.
    Test {
        /// Pack file to load on top of the registry.
        pack: Option<PathBuf>,
        /// Chain to run (default: the pack's name).
        #[arg(long)]
        parser: Option<String>,
        /// Sample log file.
        #[arg(long)]
        sample: PathBuf,
        /// Show at most this many events.
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u64).range(1..))]
        max: u64,
    },
    /// Load-only check of a pack; reports every error with file and field.
    Validate {
        /// Pack file.
        pack: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum FeedbackCmd {
    /// Record a report about a run's parsing.
    Leave {
        /// Run id the report is about.
        run_id: String,
        /// Report kind.
        #[arg(long, value_enum)]
        kind: FeedbackKindArg,
        /// What was observed.
        #[arg(long)]
        detail: String,
        /// Raw output line the report points at.
        #[arg(long)]
        line: Option<u64>,
        /// Severity the line should have had.
        #[arg(long, value_enum)]
        expected: Option<SeverityArg>,
        /// Quoted log fragment.
        #[arg(long)]
        snippet: Option<String>,
    },
    /// Feedback records of this project (or every project with --global).
    List {
        /// Only records in this state.
        #[arg(long, value_enum)]
        status: Option<FeedbackStatusArg>,
        /// Walk the cross-project index instead; dangling pointers show as
        /// orphans.
        #[arg(long)]
        global: bool,
        /// Show at most this many records.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..))]
        max: u64,
    },
    /// One record in full.
    Show {
        /// Feedback id (`f-<ts>-<seq>`).
        fid: String,
    },
    /// Copy the reported raw lines (±20) into a golden fixture whose
    /// expected events are empty — the test fails until filled in.
    Promote {
        /// Feedback id.
        fid: String,
        /// loglense source checkout to write fixtures into (default:
        /// LOGLENSE_REPO, else the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Spool the line refers to (default: stdout when it has that line).
        #[arg(long, value_enum, default_value_t = PromoteStream::Auto)]
        stream: PromoteStream,
        /// Fixture directory when the record names no parser.
        #[arg(long)]
        parser: Option<String>,
        /// Overwrite an existing fixture of the same name.
        #[arg(long)]
        force: bool,
    },
    /// Close a record as fixed or won't-fix.
    Resolve {
        /// Feedback id.
        fid: String,
        /// Final state.
        #[arg(long, value_enum)]
        status: ResolveStatus,
        /// Maintainer note.
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FeedbackKindArg {
    ParseMiss,
    FalsePositive,
    Bug,
    Feature,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SeverityArg {
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FeedbackStatusArg {
    Open,
    Fixed,
    Wontfix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ResolveStatus {
    Fixed,
    Wontfix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PromoteStream {
    Auto,
    Out,
    Err,
}

#[derive(Debug, Subcommand)]
pub(crate) enum NotifyCmd {
    /// Type a one-line completion message into a tmux pane (and press
    /// Enter), e.g. `--on-complete 'loglense notify tmux --pane %3'`.
    Tmux {
        /// Target pane (`%3`, `session:window.pane`, …).
        #[arg(long)]
        pane: String,
        /// Message layout; only {id} {status} {exit} {errors} {warnings}.
        #[arg(long, value_name = "FMT")]
        msg_fmt: Option<String>,
        /// Type the message without pressing Enter.
        #[arg(long)]
        no_enter: bool,
    },
    /// Run any command with the callback context (env vars + summary JSON
    /// on stdin); exits with the command's code.
    Exec {
        /// The command to run.
        #[arg(
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "CMD"
        )]
        cmd: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum StatusArg {
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
