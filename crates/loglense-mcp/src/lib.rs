//! MCP stdio server exposing loglense runs and events to agents
//! (docs/surfaces.md §5, D-008). Eight tools, poll-based, no notifications.
//!
//! Diagnostics discipline: stdout carries only JSON-RPC frames (clippy
//! denies printing to stdout in this crate); every tool body runs on a
//! blocking thread whose panic becomes a tool error, so no handler can take
//! the server down.

mod params;
mod stores;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use loglense_runner::{kill_run, spawn_run, wait_run, KillAction, SpawnRequest};
use loglense_store::query::{self, EventFilter, LogStream};
use loglense_store::{read_meta, FeedbackRecord, FeedbackStatus};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde_json::{json, Value};

pub use params::*;
use stores::KnownStores;

/// Default / max `list_runs` rows.
const LIST_DEFAULT: u32 = 50;
const LIST_MAX: u32 = 200;
/// Default / max `get_events` page.
const EVENTS_DEFAULT: u32 = 100;
const EVENTS_MAX: u32 = 1000;
/// Default / max `get_log` lines.
const LOG_DEFAULT: u32 = 100;
const LOG_MAX: u32 = 10_000;
/// Default / hard max `wait_run` timeout.
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MAX_MS: u64 = 120_000;
/// `wait_run` meta poll cadence.
const WAIT_POLL: Duration = Duration::from_millis(100);
/// Templates returned by `get_run`.
const RUN_TEMPLATES: usize = 20;

const INSTRUCTIONS: &str = "loglense runs commands detached and answers bounded questions about \
their output. start_run(cmd, cwd) returns a run_id at once; then wait_run (≤120 s per call), \
get_run (summary + top error templates), get_events (filter by severity/kind), get_log (raw \
lines). Always pass `cwd` as the absolute project directory — this server never uses its own \
working directory. leave_feedback reports parser misses.";

/// The loglense MCP server.
#[derive(Clone)]
pub struct LoglenseServer {
    known: Arc<KnownStores>,
    supervisor_exe: Option<PathBuf>,
    #[allow(dead_code)] // read by the #[tool_handler] expansion
    tool_router: ToolRouter<LoglenseServer>,
}

impl Default for LoglenseServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LoglenseServer {
    /// A server whose runs re-exec the current binary as their supervisor.
    pub fn new() -> Self {
        Self {
            known: Arc::new(KnownStores::default()),
            supervisor_exe: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Overrides the supervisor binary (tests embedding the server).
    pub fn with_supervisor_exe(mut self, exe: PathBuf) -> Self {
        self.supervisor_exe = Some(exe);
        self
    }
}

#[tool_router]
impl LoglenseServer {
    #[tool(
        description = "Start a command detached in the project dir `cwd`; returns run_id and pid immediately."
    )]
    async fn start_run(
        &self,
        Parameters(args): Parameters<StartRunArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        let supervisor_exe = self.supervisor_exe.clone();
        blocking(move || {
            if args.cmd.is_empty() {
                return Err("`cmd` must name a command".to_string());
            }
            let state = known.writer_state(&args.cwd)?;
            let mut request = SpawnRequest::new(args.cmd, PathBuf::from(&args.cwd));
            request.parser = args.parser;
            request.supervisor_exe = supervisor_exe;
            let started = spawn_run(&state, request).map_err(|e| e.to_string())?;
            known.remember_run(started.run_id.as_str(), &state);
            Ok(json!({
                "run_id": started.run_id.as_str(),
                "pid": started.child_pid,
                "run_dir": started.run_dir,
            }))
        })
        .await
    }

    #[tool(
        description = "List runs of the project at `cwd`, newest first, with status and error/warning counts."
    )]
    async fn list_runs(
        &self,
        Parameters(args): Parameters<ListRunsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let state = known.reader_state(&args.cwd)?;
            let limit = clamp(args.limit, LIST_DEFAULT, LIST_MAX);
            let list = query::list(
                &state,
                args.status.map(StatusParam::filter),
                limit,
                now_ms(),
            );
            for row in &list.runs {
                known.remember_run(&row.run_id, &state);
            }
            to_value(&list)
        })
        .await
    }

    #[tool(
        description = "One run: meta, terminal summary (counts, test totals) and top error/warning templates."
    )]
    async fn get_run(
        &self,
        Parameters(args): Parameters<RunRef>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let detail = query::run_detail(&run_dir, RUN_TEMPLATES)
                .map_err(|e| format!("run {} is corrupt: {e}", args.run_id))?;
            to_value(&detail)
        })
        .await
    }

    #[tool(
        description = "A page of a run's parsed events, oldest first, optionally filtered by severity and kind."
    )]
    async fn get_events(
        &self,
        Parameters(args): Parameters<GetEventsArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let filter = EventFilter {
                severity: args.severity.map(SeverityParam::severity),
                kind: args.kind.map(KindParam::kind),
            };
            let offset = usize::try_from(args.offset.unwrap_or(0)).unwrap_or(usize::MAX);
            let page = query::events(
                &run_dir,
                filter,
                offset,
                clamp(args.limit, EVENTS_DEFAULT, EVENTS_MAX),
            )
            .map_err(|e| e.to_string())?;
            to_value(&page)
        })
        .await
    }

    #[tool(
        description = "Raw output lines of a run: the last tail_n lines of out/err/both (both = arrival order), or tail_n lines from from_line."
    )]
    async fn get_log(
        &self,
        Parameters(args): Parameters<GetLogArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let n = clamp(args.tail_n, LOG_DEFAULT, LOG_MAX);
            let stream = args.stream.unwrap_or(LogStreamParam::Out);
            let page =
                match (args.from_line, stream) {
                    (Some(_), LogStreamParam::Both) => return Err(
                        "`from_line` needs stream \"out\" or \"err\" (line numbers are per stream)"
                            .to_string(),
                    ),
                    (Some(from), LogStreamParam::Out) => {
                        query::log_from(&run_dir, loglense_core::Stream::Out, from, n)
                    }
                    (Some(from), LogStreamParam::Err) => {
                        query::log_from(&run_dir, loglense_core::Stream::Err, from, n)
                    }
                    (None, LogStreamParam::Out) => query::tail(&run_dir, LogStream::Out, n),
                    (None, LogStreamParam::Err) => query::tail(&run_dir, LogStream::Err, n),
                    (None, LogStreamParam::Both) => query::tail(&run_dir, LogStream::Merged, n),
                }
                .map_err(|e| e.to_string())?;
            to_value(&page)
        })
        .await
    }

    #[tool(
        description = "Wait for a run to finish (default 10 s, max 120 s per call); returns status, exit_code, timed_out. Returns at once for dead (orphaned/lost) runs."
    )]
    async fn wait_run(
        &self,
        Parameters(args): Parameters<WaitRunArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let timeout = args.timeout_ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MAX_MS);
            let outcome = wait_run(&run_dir, Some(Duration::from_millis(timeout)), WAIT_POLL);
            let mut body = json!({
                "run_id": args.run_id,
                "status": outcome.lifecycle.as_str(),
                "timed_out": outcome.timed_out,
            });
            if let Some(code) = outcome.exit_code {
                body["exit_code"] = code.into();
            }
            Ok(body)
        })
        .await
    }

    #[tool(
        description = "Terminate a run (TERM to its process group, KILL after 5 s) behind liveness and pid-reuse checks."
    )]
    async fn kill_run(
        &self,
        Parameters(args): Parameters<RunRef>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let outcome = kill_run(&run_dir);
            let action = match outcome.action {
                KillAction::Signaled => "signaled",
                KillAction::AlreadyTerminal => "already-terminal",
                KillAction::Refused => "refused",
            };
            Ok(json!({
                "run_id": args.run_id,
                "action": action,
                "status": outcome.lifecycle.as_str(),
                "detail": outcome.detail,
            }))
        })
        .await
    }

    #[tool(
        description = "Report a parser problem on a run (e.g. a missed error); stored in the project and the global feedback index."
    )]
    async fn leave_feedback(
        &self,
        Parameters(args): Parameters<LeaveFeedbackArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let known = Arc::clone(&self.known);
        blocking(move || {
            let run_dir = known.locate(&args.run_id, args.cwd.as_deref())?;
            let state = run_dir
                .parent()
                .and_then(|runs| runs.parent())
                .ok_or_else(|| format!("run dir {} has no store", run_dir.display()))?;
            let meta = read_meta(&run_dir).map_err(|e| e.to_string())?;
            let record = FeedbackRecord {
                kind: args.kind.as_str().to_string(),
                detail: query::clip(&args.detail),
                run_id: args.run_id,
                parser: meta.parser,
                tool_version: meta.tool_version,
                line: args.line,
                expected: args.expected_severity.map(|s| s.as_str().to_string()),
                snippet: args.snippet.map(|s| query::clip(&s)),
                status: FeedbackStatus::Open,
                note: None,
            };
            let fid = loglense_store::write_feedback(state, &stores::home(), now_ms(), record)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "feedback_id": fid }))
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for LoglenseServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
            .with_server_info(Implementation::from_build_env())
    }
}

/// Runs `body` on a blocking thread: `Ok` → structured success, `Err` →
/// structured tool error, panic → tool error (the server keeps serving).
async fn blocking<F>(body: F) -> Result<CallToolResult, rmcp::ErrorData>
where
    F: FnOnce() -> Result<Value, String> + Send + 'static,
{
    let outcome = tokio::task::spawn_blocking(body).await;
    Ok(match outcome {
        Ok(Ok(value)) => success(value),
        Ok(Err(message)) => failure(message),
        Err(join) => failure(format!("internal error: tool handler failed: {join}")),
    })
}

/// Structured content plus the same JSON pretty-printed as text.
fn success(value: Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let mut result = CallToolResult::structured(value);
    result.content = vec![ContentBlock::text(text)];
    result
}

fn failure(message: String) -> CallToolResult {
    CallToolResult::structured_error(json!({ "error": message }))
}

fn to_value(value: &impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| format!("cannot serialize result: {e}"))
}

fn clamp(requested: Option<u32>, default: u32, max: u32) -> usize {
    usize::try_from(requested.unwrap_or(default).clamp(1, max)).unwrap_or(1)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Serves MCP over stdio until the client disconnects. A client that closes
/// stdin before initializing (e.g. `loglense mcp < /dev/null`) is a clean
/// exit, not an error.
pub fn serve_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let running = match LoglenseServer::new().serve(rmcp::transport::stdio()).await {
            Ok(running) => running,
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        running.waiting().await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_applies_default_floor_and_ceiling() {
        assert_eq!(clamp(None, 50, 200), 50);
        assert_eq!(clamp(Some(0), 50, 200), 1);
        assert_eq!(clamp(Some(10_000), 50, 200), 200);
    }

    #[test]
    fn eight_tools_exactly() {
        let mut names: Vec<String> = LoglenseServer::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "get_events",
                "get_log",
                "get_run",
                "kill_run",
                "leave_feedback",
                "list_runs",
                "start_run",
                "wait_run"
            ]
        );
    }
}
