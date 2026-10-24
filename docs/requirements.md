# loglense — Requirements

Status: approved design companion (2026-10-24) · Scope source: work plan §Scope · Companions: [architecture.md](architecture.md), [decisions.md](decisions.md)

loglense is a `time`-style command wrapper for AI agents: put it in front of any command, get an instant run ID back, then query short, structured, bounded results — summaries, errors, warnings, test totals, tails — via CLI or MCP instead of drowning in raw logs. This file states what loglense must do (FR) and how it must behave (NFR); each row names the plan todo whose acceptance tests verify it (F1 = final plan-compliance audit).

## 1. Functional requirements

| ID | Requirement (one line) | Verified by |
|---|---|---|
| FR-01 | Time-style wrap: `loglense <cmd...>` when the first argument is not a reserved subcommand starts the command detached and returns instantly — printing run id + child pid, exit 0; `loglense run -- <cmd...>` is the always-safe explicit form | T11, T12 |
| FR-02 | Detached survival: the run outlives the invoking terminal, agent tool timeouts, and CLI death (double-fork + setsid supervisor owning the child; no zombie intermediate) | T11 |
| FR-03 | Project-local store `.loglense/` with git-style walk-up discovery from CWD (never crossing the git root or `$HOME`), creation in CWD by writers with a self-ignore `.gitignore`, `LOGLENSE_STATE_DIR` env and `--dir` flag overriding discovery, refusal to create in `/` or `$HOME` without `--force` | T5, T12 |
| FR-04 | Parse-on-write: the detached supervisor captures stdout and stderr to pristine raw spools while a per-stream parser chain emits normalized events (stream tag + monotonic seq) appended to `events.jsonl`, flushed per event | T11 |
| FR-05 | Two-tier parser registry with `extends` inheritance — format bases generic/json/logfmt; lang bases java/python/rust/go; tool parsers maven (extends java), pytest (python), cargo (rust), gotest (go), standalone nix; TOML packs (schema=1) from project and user `formats.d/` with precedence project > user > built-in, and a bad pack warns to stderr and is skipped | T6, T7, T8, T9, T10 |
| FR-06 | Bounded reads everywhere: every read surface is capped by default with documented caps (see §1.2); flags may override upward; no unbounded read path exists | T12, T13, F1 |
| FR-07 | CLI subcommands run, init, ls, show, errors, warnings, tail, wait, kill, gc, parsers, feedback, notify (plus reserved mcp, help, version) honoring the agent exit-code contract (see §1.1) | T12, T14, T16 |
| FR-08 | MCP stdio server `loglense mcp` exposing exactly 8 tools — start_run, list_runs, get_run, get_events, get_log, wait_run, kill_run, leave_feedback — with schemars-typed args, stdout carrying only protocol frames (diagnostics to stderr), and a required explicit `cwd` on start_run/list_runs | T13 |
| FR-09 | Feedback loop: leave/list/show/promote/resolve records agent reports under `.loglense/feedback/` plus a pointer in the global index `~/.local/state/loglense/feedback-index.jsonl`; promote copies referenced raw lines into a golden fixture pair (empty expected fails until filled) and prints the test command; resolve marks fixed/wontfix and rewrites the index line | T5, T14 |
| FR-10 | Garbage collection bounded by age (default 14d) and total size (default 2048MB, oldest first), serialized via flock on `.loglense/gc.lock`, never touching runs with a live supervisor, deleting via rename-to-`.trash-<ts>` then remove | T5 |
| FR-11 | Completion callbacks: `--on-complete` flag (CLI) and `on_complete` param (MCP) stored in meta; the supervisor fires the callback after atomic finalization — detached, single attempt, 30s default timeout — with env context + summary JSON on stdin; output captured to bounded `callback.log` (≤64KB); result ok/failed/timeout recorded in summary; notify adapters tmux + exec built-in, webhook as a curl recipe; best-effort guarantee (see §1.3) | T16 |

### 1.1 Exit-code contract (FR-07)

Agents branch on these codes; they are contract, not convention.

| Exit code | Meaning |
|---|---|
| 0 | ok |
| 1 | run outcome failed (non-zero child) |
| 2 | usage error, unknown run id, corrupt run |
| 124 | wait timeout |

`wait` fast-fails with a derived interrupted substate when supervisor and child are both dead, instead of burning the timeout.

### 1.2 Default read caps (FR-06, NFR-05)

| Surface | Default cap |
|---|---|
| `show` | 20 error templates |
| `errors` / `warnings` | 50 events |
| `tail` | 100 lines |
| MCP `list_runs` | 50 runs |
| MCP `get_events` | 100 events (offset paging) |
| MCP `get_log` | 100 lines |
| MCP `wait_run` | 10s default, hard server-side clamp 120s |
| callback execution | 30s default timeout |
| `callback.log` | 64KB |

### 1.3 Completion callback contract (FR-11)

- Trigger: any terminal state — exited, signaled, failed-to-start.
- Ordering: meta + summary are finalized atomically BEFORE the callback fires; a failing or hanging callback can never corrupt the run record. The callback result lands via a second atomic summary update afterwards.
- Context: run id, status, exit code, and store path as environment variables; full summary JSON on the callback's stdin.
- Execution: detached, single attempt, bounded timeout (default 30s, configurable).
- Observability: captured output goes to `runs/<id>/callback.log` (bounded); the result is recorded as callback ok/failed/timeout and visible via `show`.
- Adapters: `loglense notify tmux` (one-line completion into a pane) and `loglense notify exec` (arbitrary command with callback context) ship built-in; webhook ships as a documented curl recipe so the binary's dependency list stays frozen.
- Guarantee: best-effort by construction — a lost supervisor fires nothing; `wait`/`wait_run` polling remains the universal fallback.

## 2. Non-functional requirements

| ID | Requirement (one line) | Verified by |
|---|---|---|
| NFR-01 | Zero-dynamic-dependency static musl binaries for x86_64 and aarch64: copy one file to any Linux machine and run it; `ldd` reports "not a dynamic executable"; `--version` embeds the short git rev | T15 |
| NFR-02 | CLI startup latency < 10ms typical (single static binary, no runtime lookups); the executable proxy is the instant-return acceptance (CLI returns < 500ms including the detach handshake) | T11, T15 |
| NFR-03 | GB-scale stdout handled with bounded memory: streaming capture and parse everywhere, 10k-line block cap, LRU template state (10k entries); the GB-scale smoke keeps resident RSS < 150MB | T11 |
| NFR-04 | Corrupt-store tolerance: a partial trailing `events.jsonl` line is skipped with a counted warning; truncated or unparseable `meta.json` classifies the run corrupt; no command ever panics on malformed store content | T5, T12 |
| NFR-05 | Agent-context safety as a hard invariant: every read path is capped by default, and summaries expose at most 20 deduped error templates | T12, T13, F1 |
| NFR-06 | Pure-Rust frozen dependency list: serde, serde_json, thiserror, clap, regex, time, schemars, rmcp, tokio (dev-only: tempfile, proptest) — nothing else enters the build without a new decision record | T1, T15, F1 |

## 3. Non-goals (v1 guardrails)

Mirror of the plan's Must-NOT list; the final audit (F1) verifies each absence.

- No daemon process, unix socket server, background service manager, or remote/multi-host operation.
- No pty mode, TUI, web UI, or watch mode.
- No dynamic plugin loading (WASM/lua/dlopen) — TOML packs are the only external extension seam.
- No log shipping, alerting, cron, or aggregation into remote sinks.
- No Windows support; no macOS claims beyond "it compiles" (target: Linux).
- No separate index database or cache file — run listing derives from a directory scan.
- No dependency on openssl, glibc-only crates, or any network client in the binary.
- No silent modification of user files; nothing writes outside `.loglense/`, the feedback index, and the configured state dir.
- No unbounded output on any read surface.
