# loglense — CLI & MCP surface contracts

Status: **spec (docs-first)** · Normative for `loglense-cli` and `loglense-mcp`. Companion: `architecture.md` §6 (surface summary) and §7 (completion notification) — this doc is the full contract; `parsers.md` covers the parser module system.

No code in this file: every contract is a table, prose or mermaid diagram. Inline backticked identifiers are names, not code.

---

## 1. CLI conventions

### 1.1 Global flags

| Flag / env | Applies to | Meaning |
|---|---|---|
| `--dir <path>` | all subcommands | use this `.loglense` store instead of walk-up discovery (short-circuits discovery entirely) |
| `LOGLENSE_STATE_DIR` (env) | all subcommands | same override as `--dir`; flag wins when both are set |
| `--json` | any subcommand that prints results | machine-readable output (shapes per subcommand below); read commands are the primary consumers |

### 1.2 Prefix dispatch and the reserved set

`loglense <cmd...>` is the time-style form: when the first argument is **not** a reserved subcommand, the CLI behaves as `run` with all arguments as the command. Reserved first arguments: `run`, `init`, `ls`, `show`, `errors`, `warnings`, `tail`, `wait`, `kill`, `gc`, `parsers`, `feedback`, `notify`, `mcp` (plus `help` and `version`).

**Ambiguity note:** to wrap a literal command whose own name starts with a reserved word (a binary actually called `run`, `show`, …), use the explicit form `run -- <cmd...>` — everything after `--` is the command, never a subcommand.

### 1.3 Bounded output (hard invariant)

Every read surface is bounded by default; caps protect agent context windows. `show` returns ≤ 20 error templates; `errors` / `warnings` ≤ 50 events; `tail` ≤ 100 lines; `ls` ≤ 50 runs. `--max` overrides the cap on `ls` / `show` / `errors` / `warnings`; `--lines` overrides it on `tail`. No read is ever unbounded.

### 1.4 Exit-code matrix (agent-facing contract)

Agents branch on exit codes — they are contract, not convention:

| Code | Meaning | Produced by |
|---|---|---|
| 0 | ok — requested operation succeeded | every subcommand on success; `wait` when the observed run ends with a successful outcome |
| 1 | run outcome failed — the wrapped command exited non-zero (or was signaled) | `wait` mirroring the run's outcome; never used for loglense's own failures |
| 2 | usage error, unknown id, or corrupt run — bad flags/arguments; unknown run or feedback id; unreadable store records (corrupt runs are warned and skipped, the rest still lists) | any subcommand on these conditions |
| 124 | wait timeout — the run was still live when `--timeout` elapsed | `wait` only |

MCP tools have no process exit codes; the same outcomes travel as result fields (`timed_out`, error text).

---

## 2. CLI subcommands

| Subcommand | Purpose | Arguments | Key flags | Default cap | `--json` shape |
|---|---|---|---|---|---|
| `run` | wrap a command: spawn detached supervisor, print id + pid, return instantly (exit 0) | `-- <cmd...>` | `--parser <name>` · `--on-complete <cmd>` · `--on-complete-timeout <ms>` | instant return | object with `run_id`, `pid`, run dir |
| `init` | create `.loglense/` in CWD, write its self-ignore file, append `.loglense/` to the repo `.gitignore` when in a git repo and the line is absent; prints every action; idempotent | — | — | — | list of actions taken |
| `ls` | list runs newest-first (derived liveness: orphaned/lost shown as interrupted family) | — | `--max` · `--status` | 50 runs | rows of `run_id`, `status`, `cmd_head`, `age`, severity `counts` |
| `show` | one run: meta + summary + top deduped error templates (+ callback result and `callback.log` tail when present) | `<run-id>` | `--max` | 20 templates | object with `meta`, `summary`, `templates` |
| `errors` | error events of a run, oldest first | `<run-id>` | `--max` | 50 events | object with `events`, `total`, `shown` |
| `warnings` | warning events of a run | `<run-id>` | `--max` | 50 events | same shape as `errors` |
| `tail` | last lines of a run's captured output (raw spool) | `<run-id>` | `--stream out\|err\|merged` · `--lines` | 100 lines | object with `lines`, `truncated` |
| `wait` | block until a run reaches a terminal state, a derived dead substate, or timeout (§3) | `<run-id>` | `--timeout <seconds>` (omitted = no timeout) | poll 250 ms | object with `status`, optional `exit_code`, `timed_out` |
| `kill` | terminate a run: signals the child process group after supervisor-liveness and pid-reuse checks; prints what was signaled; clean refusal on starttime mismatch or already-terminal run | `<run-id>` | — | — | object with `run_id`, `action`, `status` |
| `gc` | delete old runs (flock-serialized; never touches live-supervisor runs; rename-to-trash then delete) | — | `--max-age <days>` · `--max-total <MB>` (defaults from config: 14 days / 2048 MB) | — | object with `runs_removed`, `bytes_freed` |
| `parsers` | authoring suite — see §2.1 | — | — | — | — |
| `feedback` | agent-feedback suite — see §2.2 | — | — | — | — |
| `notify` | ping adapters, designed to run **as** an `--on-complete` callback target — see §4.3 | — | — | — | small adapter-specific object |
| `mcp` | start the stdio MCP server (§5) | — | — | — | no output — protocol on stdio |

`tail` has no follow/watch mode in v1 — a `--follow` flag is rejected with a clear error rather than approximated.

### 2.1 `parsers` subcommands

| Command | Purpose | Output |
|---|---|---|
| `parsers list` | registry contents | table: name, source (built-in/project/user), extends, loaded-or-warning |
| `parsers test <pack.toml>` | dry-run a pack (or `--parser <name>` chain) against `--sample <file>` | events it would emit + `N lines unclaimed` info; exit 0 |
| `parsers validate <pack.toml>` | load-only, all errors file:field-precise | clean/dirty verdict + error list; exit 0 when clean |

### 2.2 `feedback` subcommands

| Command | Purpose | Output |
|---|---|---|
| `feedback leave <run-id>` | record an agent report (`--kind` parse-miss/false-positive/bug/feature/other · `--detail` text · optional `--line N`, `--expected <severity>`, `--snippet`) | prints feedback id `f-<ts>-<seq>` |
| `feedback list` | open records; `--global` walks the cross-project index and marks dangling pointers as orphans | record rows |
| `feedback show <fid>` | one record | full record |
| `feedback promote <fid>` | copy the referenced raw lines (around the reported line ± 20) into `crates/loglense-parsers/tests/fixtures/<parser>/f-<fid>.log` + sibling `.expected.json` containing an **empty array** (fails-until-filled) | prints the exact `cargo test` command to run and reminds to fill expected |
| `feedback resolve <fid>` | `--status fixed\|wontfix`, optional `--note` | updated record |

---

## 3. The `wait` contract

`wait` (and its MCP twin `wait_run`) never make an agent burn a timeout on a dead run.

| Aspect | Contract |
|---|---|
| Result | object with `status` (terminal: exited / signaled / failed-to-start — or a derived dead substate — or `running` only on timeout), `exit_code` present exactly when the child exited, and `timed_out` boolean |
| Polling | CLI polls run meta every **250 ms**; MCP `wait_run` polls every 100 ms |
| Fast-fail | before waiting, derived liveness is computed: if the supervisor is dead and the child is alive (`orphaned`) or both are dead (`lost`), `wait` returns **immediately** with the interrupted substate instead of polling to timeout |
| Timeout | `--timeout <seconds>`; when it elapses with the run still live → exit **124**, `timed_out: true` (MCP: `timed_out: true`, no exit code) |
| Exit mirror | on terminal states, `wait` mirrors the run outcome: child exit 0 → 0; child non-zero / signaled → 1 (matrix §1.4) |
| MCP bounds | `wait_run` `timeout_ms` defaults to **10000** and is hard-clamped server-side to **120000** ms (larger values are clamped, not rejected) |

---

## 4. Completion callback contract

Per `architecture.md` §7 (D-011): push beats poll — the supervisor itself pings the agent when results are ready. Polling stays the always-available fallback.

### 4.1 Requesting

| Surface | How |
|---|---|
| CLI | `--on-complete <cmd>` on `run` (and the prefix form), optional `--on-complete-timeout <ms>` (default 30000) |
| MCP | `on_complete` and `on_complete_timeout_ms` params on `start_run` |

The callback spec is stored in run meta at spawn time.

### 4.2 Execution semantics

| Aspect | Contract |
|---|---|
| Trigger | any terminal state: exited, signaled, or failed-to-start |
| Ordering | run finalization **always precedes** the callback: `meta.json` and `summary.json` are atomically written first — a failing or hanging callback can never corrupt the run record; the callback *result* is recorded via a second atomic summary update after the callback completes |
| Context | environment variables (table below) + the full summary JSON on the callback's standard input |
| Execution | detached, **single attempt**, bounded by the timeout (default 30 s) |
| Observability | captured output lands in `runs/<id>/callback.log`, bounded **≤ 64 KB**; the result — `ok` / `failed` / `timeout` — is recorded in the summary and displayed by `show` |
| Guarantee | best-effort by construction: the callback fires only when the supervisor survives to the terminal state; a lost run fires **nothing** — `wait` / `wait_run` polling is the fallback path |
| Trust | same trust level as the wrapped command itself — the caller already controls what runs; callbacks add no new privilege |

| Env var | Value |
|---|---|
| `LOGLENSE_RUN_ID` | the run id |
| `LOGLENSE_STATUS` | terminal status (exited / signaled / failed-to-start) |
| `LOGLENSE_EXIT_CODE` | child exit code — present only when the child exited |
| `LOGLENSE_STORE_PATH` | absolute path of the `.loglense` store root |

### 4.3 `notify` adapters

`loglense notify` subcommands exist to be used **as** the callback target:

| Adapter | Invocation | What it pings |
|---|---|---|
| tmux (built-in) | `notify tmux --pane <target>` (optional `--msg-fmt`) | injects a one-line completion message into a tmux pane — TUI agents living in tmux literally receive the line as input. Message fields are fixed: run id, status, exit code, error count (E), warning count (W); `--msg-fmt` may rearrange exactly these documented fields, never arbitrary interpolation |
| exec (built-in) | `notify exec -- <cmd>` | runs any command with the full callback context (env vars + summary JSON on stdin) — harness CLIs, custom integrations |
| webhook (recipe, not built-in) | an exec callback invoking `curl` | POSTs the summary JSON (read from the callback's stdin) to a URL with a ~10 s limit. Kept a documented recipe so the static binary's dependency list stays frozen — no HTTP client ships in the binary |

---

## 5. MCP server (`loglense mcp`)

### 5.1 Diagnostics discipline

- **stdout carries only protocol frames** (JSON-RPC). A single stray print corrupts the stream — all warnings, tracing and diagnostics go to **stderr**.
- A panic in a tool handler must never abort the server: failures return as tool errors; the server keeps serving.
- Transport: stdio; eight tools, exactly the set below — no notifications, no streaming (wait_run is poll-based by design).

### 5.2 Tools

#### `start_run`

| Param | Type | Required | Default | Bounds / notes |
|---|---|---|---|---|
| `cmd` | array of strings | yes | — | the command argv to wrap |
| `cwd` | string | yes | — | **absolute project dir** — store discovery runs from here, never from the server's own CWD (§5.3) |
| `parser` | string | no | auto | module name; omit for config default / autodetect |
| `on_complete` | string | no | — | callback command (§4) |
| `on_complete_timeout_ms` | integer | no | 30000 | |

Result: object with `run_id` (text) and `pid` (integer).

#### `list_runs`

| Param | Type | Required | Default | Bounds |
|---|---|---|---|---|
| `status` | enum | no | all | filter by status family (incl. derived interrupted substates) |
| `limit` | integer | no | 50 | server clamps to ≤ 200 |
| `cwd` | string | yes | — | explicit project dir — same trap as `start_run` |

Result: rows of `run_id`, `status`, `cmd_head` (first token of the command), `age`, severity `counts`.

#### `get_run`

| Param | Type | Required | Default |
|---|---|---|---|
| `run_id` | string | yes | — |

Result: object with `meta` (full run meta), `summary` (terminal summary, incl. callback result when present), `templates` (top deduped error templates — ≤ 20, with count, first/last seen, example).

#### `get_events`

| Param | Type | Required | Default | Bounds |
|---|---|---|---|---|
| `run_id` | string | yes | — | |
| `severity` | enum | no | all | error / warning / notice / info / debug |
| `kind` | enum | no | all | line / phase / summary / test |
| `limit` | integer | no | 100 | server clamps to ≤ 1000 |
| `offset` | integer | no | 0 | paginate past the newest |

Result: object with `events` (event array, §1 of `parsers.md`) and `total` (matching count, unpaginated).

#### `get_log`

| Param | Type | Required | Default | Bounds |
|---|---|---|---|---|
| `run_id` | string | yes | — | |
| `stream` | enum | no | out | out / err / both |
| `tail_n` | integer | no | 100 | server clamps to ≤ 10000 |
| `from_line` | integer | no | — | 1-based raw line; when set, return up to `tail_n` lines starting there instead of the last `tail_n` |

Result: object with `lines` (numbered text lines) and `truncated` (boolean).

#### `wait_run`

| Param | Type | Required | Default | Bounds |
|---|---|---|---|---|
| `run_id` | string | yes | — | |
| `timeout_ms` | integer | no | 10000 | **hard server-side clamp 120000** |

Result: object with `status`, `exit_code` (when exited), `timed_out`. Polls meta every 100 ms; fast-fails on orphaned/lost exactly like CLI `wait` (§3).

#### `kill_run`

| Param | Type | Required | Default |
|---|---|---|---|
| `run_id` | string | yes | — |

Result: object with `run_id`, `action` (signaled / already-terminal / refused), `status`, optional `detail`. Same liveness and pid-reuse guards as CLI `kill`.

#### `leave_feedback`

| Param | Type | Required | Default | Notes |
|---|---|---|---|---|
| `run_id` | string | yes | — | |
| `kind` | enum | yes | — | parse-miss / false-positive / bug / feature / other |
| `detail` | string | yes | — | free text |
| `line` | integer | no | — | raw line the feedback refers to |
| `expected_severity` | enum | no | — | what the agent expected (e.g. a parse-miss that should have been error) |
| `snippet` | string | no | — | quoted log fragment |

Result: object with `feedback_id`. The record lands in the project store and is mirrored to the global cross-project index (see `feedback leave`, §2.2).

### 5.3 `.mcp.json` wiring

| Key | Value | Notes |
|---|---|---|
| `command` | `loglense` | or the absolute path to the binary if it is not on PATH |
| `args` | array containing exactly `mcp` | boots the stdio server |

**The cwd trap:** MCP servers are commonly launched from `$HOME`, not from the project the agent is working in. Store discovery walks up from the *process* CWD — run from `$HOME` it would find (or worse, create) the wrong store. This is exactly why `start_run` and `list_runs` take an **explicit required `cwd`**: the agent names the project directory, and discovery runs from there. Never rely on the server's own CWD.

---

## 6. Notification sequence (MCP path)

```mermaid
sequenceDiagram
    participant A as agent (MCP client)
    participant M as loglense mcp server
    participant R as supervisor
    participant K as child command
    participant T as tmux pane (agent's terminal)

    A->>M: start_run(cmd, cwd, on_complete = notify tmux --pane …)
    M->>R: spawn + ack (setsid handshake)
    M-->>A: run_id + pid (instant)
    R->>K: exec — pipes spool + parse-on-write
    K-->>R: terminal state
    R->>R: finalize: meta + summary.json (atomic) — before anything else
    R->>T: fire callback (env vars + summary JSON on stdin, single attempt, ≤ 30 s)
    T-->>A: pane line: run … done — failed, 3 errors, 1 warning
    Note over R,T: best-effort: a lost supervisor fires nothing —<br/>wait / wait_run polling stays as the fallback
```

---

## 7. Cross-references

- `architecture.md` §6 (surface summary table), §7 (completion notification) — this doc is their full contract.
- `parsers.md` — event model (consumed by `get_events` / `show` / fixtures), `parsers` suite, fixture convention.
- Decision records: D-008 (single binary = CLI + MCP), D-011 (supervisor-fired callbacks + adapters).
