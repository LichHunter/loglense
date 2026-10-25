# loglense — Decision records

ADR-lite ledger: every cross-cutting choice with its why. Titles and section cross-references match the register in [architecture.md](architecture.md) §11. Date = decision date. Rule (binding for all future work): a new cross-cutting choice gets a new D-number here before it enters the code.

| # | Decision | Architecture | Date |
|---|---|---|---|
| D-001 | daemonless spool | §1, §4, §5 | 2026-10-24 |
| D-002 | project-local `.loglense/` + walk-up discovery | §5 | 2026-10-24 |
| D-003 | two-tier lang-first parsers (`extends`) | §3 | 2026-10-24 |
| D-004 | TOML packs as the only external extension seam | §3.4, §10 | 2026-10-24 |
| D-005 | stateful per-block parsing | §3.2 | 2026-10-24 |
| D-006 | no index file — directory scan | §5 | 2026-10-24 |
| D-007 | pipes, not pty | §4.3 | 2026-10-24 |
| D-008 | single binary = CLI + MCP (rmcp, server features) | §6, §9 | 2026-10-24 |
| D-009 | static musl distribution | §9 | 2026-10-24 |
| D-010 | feedback → fixture promote bridge | §8 | 2026-10-24 |
| D-011 | supervisor-fired completion callbacks + ping adapters | §7 | 2026-10-24 |
| D-012 | minimal packages-only flake + devenv dev shell | §9 | 2026-10-24 |

## D-001 — daemonless spool

| | |
|---|---|
| Context | Agents need to inspect long-running builds after the fact, from multiple consumers (CLI, MCP) and after crashes. A daemon adds a lifecycle to manage, a socket to break, locks to contend on, and a second crash domain. |
| Decision | The filesystem is the state: a detached per-run supervisor writes runs under `.loglense/`; every consumer is a pure file reader. No daemon, no sockets; the only locks are gc serialization and the feedback index append. Atomic writes (temp + rename) make readers race-free. |
| Alternatives considered | Daemon + unix socket (systemd-style); embedded database (sqlite) as state; client-server with a query API. |
| Consequences | Crash-tolerant by construction; consumers are trivially read-only and parallel-safe. Liveness is derived, not tracked (orphaned/lost substates from `/proc`). No cross-machine use in v1 — accepted, and the on-disk format is already daemon-compatible if one is ever added. Serves FR-01..FR-04, FR-10; NFR-04. |

## D-002 — project-local `.loglense/` + walk-up discovery

| | |
|---|---|
| Context | Runs belong to a project (a repo, a workspace). A single global store mixes projects, complicates retention, and makes "which runs are mine?" ambiguous for agents working in different checkouts. |
| Decision | Git-style walk-up from CWD to the nearest `.loglense/`, never crossing the git root or `$HOME`; writers create `./.loglense/` when none is found (also writing a self-ignore `.loglense/.gitignore` so spool contents never land in git); `LOGLENSE_STATE_DIR` and `--dir` override discovery entirely; refuse to create in `/` or `$HOME` without `--force`. |
| Alternatives considered | One global store under `~/.local/state`; XDG cache dir per project hash; env-var-only configuration (no discovery). |
| Consequences | Per-project gc and retention; `formats.d/` packs are committable per project. Nested-repo ambiguity is resolved by the git-root boundary; a symlinked store is canonicalized once at spawn. Serves FR-03, FR-10. |

## D-003 — two-tier lang-first parsers (`extends`)

| | |
|---|---|
| Context | Tool log formats inherit their ecosystem's grammar: every JVM tool can print Java stack traces; every Rust tool surfaces rustc diagnostics. Per-tool monoliths would duplicate that grammar N times and drift. |
| Decision | Language bases (java, python, rust, go) own the ecosystem's multi-line grammar; tool parsers `extend` them and add only tool-specific rules: maven→java, pytest→python, cargo→rust, gotest→go, nix standalone. `extends` is name-based, resolved by DFS at registry build time; cycles and unknown parents are load-time errors. The same mechanism serves compiled parsers and TOML packs. |
| Alternatives considered | Self-contained per-tool parsers; pure config (regex packs only, no shared bases); a filter-pipeline model without inheritance. |
| Consequences | Maven logs with embedded NPE traces parse correctly with zero stack-trace code in the maven parser. Bases need curation (fixtures, tests); a new ecosystem base is rare (post-v1 `gradle extends java` is the canonical curated example). Serves FR-05. |

## D-004 — TOML packs as the only external extension seam

| | |
|---|---|
| Context | New log sources appear faster than loglense releases. Recompiling for every internal tool is a non-starter; dynamic plugin loading (WASM/lua/dlopen) is a security and maintenance burden that would also break the static-binary guarantee. |
| Decision | Declarative TOML packs (schema=1, `deny_unknown_fields`, `regex`-crate dialect only — linear-time, no catastrophic backtracking) dropped into `.loglense/formats.d/` (project) or `~/.config/loglense/formats.d/` (user). Precedence: project > user > built-in (a project pack may shadow a built-in by name). A bad pack warns to stderr naming file and field, and is skipped — it can never kill a run. |
| Alternatives considered | WASM plugins; embedded Lua; dlopen native modules; shipping a full backtracking regex engine in packs. |
| Consequences | New sources in minutes with no recompilation; packs are safe to load from project repos. Expressiveness is bounded (accepted — curated parsers remain PATH B/C for complex grammar). Authoring loop: `parsers test` dry-run + `parsers validate` field-precise errors. Serves FR-05; NFR-06. |

## D-005 — stateful per-block parsing

| | |
|---|---|
| Context | Line-oriented classification fragments stack traces: one Java exception becomes forty line-events, flooding agent context and destroying the "short, structured summary" promise. |
| Decision | Each module holds a `ParseState` per run — and separately per stream — and may buffer while a multi-line block (stack trace, rustc diagnostic) is open, emitting one Event per logical block with frames attached. At end of stream, `finish()` flushes any still-open partial block as a best-effort event. One state per stream means stdout and stderr blocks never interleave. |
| Alternatives considered | Stateless line classification with severity keywords only; post-hoc reassembly of line-events by consumers; global (cross-stream) parse state. |
| Consequences | Summaries stay agent-sized even with hundred-line traces. EOF-flush semantics must be tested explicitly (truncated-trace fixtures are first-class golden cases). Consumers remain parser-agnostic (P3). Serves FR-04, FR-05; NFR-03, NFR-05. |

## D-006 — no index file — directory scan

| | |
|---|---|
| Context | Listing runs could use an index file or database for speed — but indexes are a second source of truth: they corrupt, race writers, drift from reality, and add a recovery path the daemonless design does not want. |
| Decision | `ls` (and MCP `list_runs`) scans `runs/*/meta.json` and sorts by start time, descending. No index file, no cache. Single-writer-per-run-dir plus atomic meta writes remove lock contention entirely. |
| Alternatives considered | sqlite catalog; a JSON index rebuilt on demand; an append-only journal of run creations. |
| Consequences | Listing is O(number of runs) — fine at project scale with gc bounds (14d/2GB defaults). One entire corruption class (index drift) cannot exist. `meta.json` is the only listing source, so tolerant readers there cover listing too. Serves FR-03, FR-10; NFR-04. |

## D-007 — pipes, not pty

| | |
|---|---|
| Context | A pty would capture interactive/color/cursor behavior — but agents wrap non-interactive builds and tests; pty emulation drags in terminal state machinery, window sizing, echo semantics, and a dependency the static binary does not want. |
| Decision | The child is spawned with pipes; stdin is `/dev/null`. Capture is line-oriented: lossy UTF-8 decode, ANSI stripped before parsing, raw spool bytes kept pristine and unstripped. Binary detection (NUL byte in the first 4KB) disables parsing for that stream — raw-only. |
| Alternatives considered | pty emulation (script(1)-style wrapper); a hybrid pty mode behind a flag. |
| Consequences | Deterministic, parseable line streams; no interactive programs in v1 (guardrail); no TUI/watch mode; colored output still parses (ANSI stripped upstream) while raw spools keep exact bytes for fixtures and tail. Serves FR-04; plan Must-NOT (no pty). |

## D-008 — single binary = CLI + MCP (rmcp, server features)

| | |
|---|---|
| Context | Agents reach loglense through two surfaces: shells (CLI) and harnesses (MCP servers configured via `.mcp.json`). Two artifacts would double packaging, versioning, and QA; hand-rolled JSON-RPC is a protocol-maintenance tax. |
| Decision | One binary serves both: reserved subcommand `loglense mcp` boots a stdio MCP server built on rmcp 3.5.1 (features: server, transport-io, schemars; client feature enabled only for the in-process dogfood test). Tool args are schemars-typed structs. Stdout carries only protocol frames — all diagnostics to stderr; a panic never aborts the server. |
| Alternatives considered | Separate `loglense-mcp` binary; hand-rolled JSON-RPC over stdio; a Python/Node sidecar server. |
| Consequences | One static artifact serves both surfaces; `.mcp.json` wiring is `{"command": "loglense", "args": ["mcp"]}`. Stdout purity is a hard invariant (one stray byte corrupts JSON-RPC). rmcp is pinned (MSRV 1.88 — satisfied by the devenv toolchain). Serves FR-08; NFR-01, NFR-06. |

## D-009 — static musl distribution

| | |
|---|---|
| Context | The primary install path is an agent copying one binary onto an arbitrary Linux machine (containers, scratch images, old distros). Dynamic linking — glibc versioning, shared-library paths — breaks copy-and-run. |
| Decision | Static musl builds for x86_64 plus cross-compiled aarch64, with a pure-Rust dependency tree (no openssl, no glibc-only crates, no network client) so static linking is actually achievable. Artifacts ship as `loglense-<ver>-<target>.tar.gz`; `--version` embeds the short git rev (fallback `dev`). |
| Alternatives considered | Dynamically linked glibc releases; container images as the only distribution; distro packages only. |
| Consequences | `ldd` reports "not a dynamic executable"; zero runtime deps (NFR-01). Slightly larger binary and no dlopen — both accepted. Static artifacts are also buildable via cargo from the dev shell, independent of nix. Serves NFR-01, NFR-06. |

## D-010 — feedback → fixture promote bridge

| | |
|---|---|
| Context | Parser gaps are discovered by agents in the field, mid-task, with the failing log right there. Without a bridge, that observation dies in chat; with only a bug tracker, the reproduction (the raw lines) is lost by the time a human looks. |
| Decision | `feedback leave` writes a record (run ref, parser name, tool version, kind, detail, optional line/snippet/expected) under `.loglense/feedback/` and appends a pointer to the global cross-project index `~/.local/state/loglense/feedback-index.jsonl`. `promote` copies the referenced raw lines into `crates/loglense-parsers/tests/fixtures/<parser>/<case>.log` plus a sibling `.expected.json` containing an empty array — the golden test fails until a human fills it — and prints the exact test command. `resolve --status fixed|wontfix` updates the record and rewrites its index line. Promote never guesses expected events and never auto-runs tests. |
| Alternatives considered | Issue-tracker integration; manual fixture authoring only; auto-generating expected events from current parser output (rejected: that would lock in the bug). |
| Consequences | Feedback and TDD are the same loop: every report can become a RED fixture before the fix. The golden runner discovers promoted fixtures automatically at test runtime. Serves FR-09. |

## D-011 — supervisor-fired completion callbacks + ping adapters

| | |
|---|---|
| Context | LLM agents are turn-based: a run that finishes while the agent is idle is invisible until the agent asks. Polling `wait` wastes turns; a notification broker would reintroduce the daemon D-001 excluded. |
| Decision | The supervisor — already alive until the run reaches a terminal state — fires an optional completion callback (`--on-complete` flag / `on_complete` MCP param, stored in meta) AFTER atomic finalization. Detached, single attempt, 30s default timeout; context via env vars (run id, status, exit code, store path) plus summary JSON on stdin; output captured to bounded `callback.log` (≤64KB); result recorded in the summary as ok/failed/timeout via a second atomic update. Common targets ship as `loglense notify` adapters: tmux and exec built-in; webhook as a documented curl recipe so the dependency list stays frozen (D-009, NFR-06). |
| Alternatives considered | A daemon broker or watcher; inotify-based file watchers; polling only (rejected as sole path — wastes agent turns); ureq/rustls for a built-in webhook (rejected — would unfreeze the dep list). |
| Consequences | Push beats poll for the common case while `wait`/`wait_run` polling remains the universal fallback — the guarantee is best-effort by construction: a lost supervisor fires nothing, and callback failure can never corrupt the run record. Serves FR-11. |

## D-012 — minimal packages-only flake + devenv dev shell

| | |
|---|---|
| Context | Two audiences with different needs: developers want a fast, batteries-included shell; NixOS machines want a consumable package. The user rejected devShells-in-flake (gate feedback #4, 2026-10-24) — pure devenv alone cannot produce a NixOS-consumable package, and full devenv-flake wiring pulls a heavy input graph for every evaluator. ephor set the precedent of keeping the two concerns apart. |
| Decision | Dev environment stays devenv + direnv (`devenv.nix`, `.envrc` with `use devenv` — no flake devShells). A minimal packages-only `flake.nix` sits alongside it: `packages.<system>.loglense` (native) now, static outputs later; the nixpkgs pin matches the ephor workspace so both projects share one closure cache. Static artifacts are also buildable via plain cargo from the dev shell, so nix is never a hard dependency for producing binaries. |
| Alternatives considered | The devenv flake template with devShells (rejected by user); no flake at all (no NixOS story); one flake carrying both devShell and packages (revisit of the rejection). |
| Consequences | Dev loop stays direnv-fast and nix-evaluation-light; NixOS consumption works via `nix build`/flake inputs from day one. Two packaging descriptions (devenv toolchain vs flake package) must stay consistent — accepted, small surface. Serves NFR-01 distribution path. |

## D-013 — `toml` for config + pack parsing

| | |
|---|---|
| Context | Config (`config.toml`) and parser packs (`formats.d/*.toml`, D-004) are TOML by contract; both need `deny_unknown_fields` errors that name the file and field. |
| Decision | Depend on `toml` 0.8 (serde-based, pure Rust) in `loglense-core` and `loglense-parsers`. |
| Alternatives considered | A hand-written TOML subset parser (rejected: error quality and spec drift); JSON packs (rejected: hostile to hand authoring, contradicts D-004). |
| Consequences | One small pure-Rust dependency; static linking unaffected (D-009). |

## D-014 — `sha2` for template ids

| | |
|---|---|
| Context | Template ids must be stable across runs, machines and releases so `show`, feedback and golden fixtures can refer to them; std's hasher is randomly keyed per process. |
| Decision | `template_id` = short hex of SHA-256 over the normalized template, via `sha2` 0.10 (pure Rust). |
| Alternatives considered | std `DefaultHasher` (rejected: not stable across processes/releases); hand-rolled FNV (rejected: collision-prone at short widths). |
| Consequences | Deterministic ids everywhere; pulls `cpufeatures` (and through it `libc`) transitively — see D-016. |

## D-015 — `fs4` for the two advisory locks

| | |
|---|---|
| Context | Exactly two places need cross-process mutual exclusion: concurrent `gc` runs (`gc.lock`) and appends to the global feedback index. std's file locking was not stable at the pinned toolchain baseline. |
| Decision | Use `fs4` 0.12 `flock` wrappers, as ephor does; no other locks exist (single writer per run dir). |
| Alternatives considered | Lock-free index appends only (rejected: index line rewrites in `resolve` need exclusion); lock files with pid content (rejected: stale-lock recovery is its own bug class). |
| Consequences | One small dependency (pulls `rustix`); static linking unaffected. |

## D-016 — `libc` for setsid detach, group signals, TERM/INT trap

| | |
|---|---|
| Context | FR-02 requires the supervisor to survive the invoking terminal, agent tool timeouts and CLI death, and `kill` must signal the child's whole process group with a TERM → grace → KILL escalation. std offers `process_group` but no `setsid`, no `kill(2)` for arbitrary pids or groups, and no signal handlers. An interim std-only design (own process group, external `kill` binary, marker file only) left the supervisor in the caller's session with the controlling terminal attached and depended on a `kill` executable being present. |
| Decision | Depend directly on `libc` 0.2 in `loglense-runner` (already in the build graph via D-014/D-015 — no new crate). All unsafe calls live in one module, each a single async-signal-safe call: `setsid` between fork and exec of `loglense __supervisor <run-dir>` (single fork: the forked child is never a group leader, so the EPERM the double-fork guards against cannot occur), `kill(-pgid, sig)` for group signals (ESRCH = already gone), and a TERM/INT `sigaction` handler that only stores the signal number for the capture loop to act on. `loglense kill` keeps the `control.kill` marker for the supervisor-owned escalation (pid-reuse-proof), plus an immediate guarded group TERM. |
| Alternatives considered | Std-only detach (rejected: weaker FR-02 guarantee, external binary dependency); classic double-fork (rejected: same guarantee as fork + setsid-before-exec with an extra process); `nix` or `rustix` safe wrappers (rejected: larger API surface for four calls); `signal-hook` (rejected: one atomic store does not need it). |
| Consequences | Runs survive SIGHUP/terminal close and a kill of the caller's entire process group (tested). External signals to the supervisor are forwarded rather than orphaning the child. Linux/Unix-only, consistent with the v1 platform scope. Serves FR-02, FR-07, NFR-04. |

## D-017 — per-read arrival journal for merged tail

| | |
|---|---|
| Context | `tail` must show the newest output across stdout and stderr in the order it appeared — a build's final error on stderr is exactly what an agent needs. The raw spools keep each stream's bytes but not their interleaving; event `seq` is emission order (autodetect buffering and multi-line blocks emit late, so a late stderr line can get the smallest seq); and the reader-to-parser channel reorders whole bursts between the two reader threads. |
| Decision | Each pipe read in the supervisor takes a process-wide sequence number. The capture loop appends one record per read chunk to `runs/<id>/interleave.idx` (stream, first raw line the chunk completed, read sequence number). `tail --stream merged` reads the journal's newest records, sorts by read sequence, and rebuilds the last N lines from one bounded pass per spool (the newest N lines overall are, per stream, a suffix of at most N lines). Runs without a journal fall back to stdout's tail then stderr's. |
| Alternatives considered | Ordering by event `seq` (rejected: emission order, shown wrong in tests); recording stream switches in channel order (rejected: channel reorders bursts); a merged spool copy (rejected: doubles disk); a pty (rejected by D-007). |
| Consequences | Merged order is exact up to pipe-read granularity — lines one read returned stay together; writes on the two streams that land between the same pair of reads cannot be ordered by any reader of two pipes. Cost: one ~12-byte record per read chunk. Serves FR-06, NFR-05. |

## D-018 — test totals travel on the event

| | |
|---|---|
| Context | The run summary's test counts came only from `kind=test` events. Quiet runners print no per-test line for passes (`pytest -q`, a passing `cargo test`), so a real `2 failed, 1 passed` run summarized as `failed 2`, wrong exactly where agents look first. The only place the full counts exist is the tool's own totals line, and the event model had no way to carry numbers. |
| Decision | `Event` gains an optional `totals` map (outcome → count, zeros omitted). Tool parsers fill it only on *aggregate* lines: pytest's final totals line, cargo's per-binary `test result:` line, and maven's bare module-level `Tests run:` line (per-class lines with `-- in <class>` are excluded, so summing never double-counts). The supervisor sums `totals` across the run when any event carries them; otherwise it falls back to counting `kind=test` events. |
| Alternatives considered | Parsing totals out of summary messages in the runner (rejected: puts tool grammar in a consumer, breaking P3, the Event model as the only contract); emitting synthetic per-test events (rejected: invents tests that never printed). |
| Consequences | Accurate totals for quiet runs. Additive schema change: older events and fixtures without `totals` still parse (§1.3 forwards compatibility). Eight golden fixtures gained the field. Serves FR-04, FR-06. |
