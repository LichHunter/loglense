# loglense

Put `loglense` in front of any command, like `time`. The command runs
detached in the background and you get a run id back at once. Then ask small
questions about it (a summary, the errors, the test totals, the last lines)
instead of reading the whole log. Built for AI agents, usable by humans.

```sh
$ loglense cargo test
run 20261025-A3F8C2 started (pid 41827)
$ loglense wait 20261025-A3F8C2 --timeout 600; loglense show 20261025-A3F8C2
run 20261025-A3F8C2  exited 101  (48.2s)
events: 1312 (error 3, warning 7, info 1302)
tests:  failed 2, passed 214
top error/warning templates (3 of 3):
      2x error   L812-907  thread 'store::gc::keeps_live' panicked at crates/…
```

- **No daemon, no database.** Each project keeps a small `.loglense/` folder
  of runs (gitignored automatically). Every command just reads files.
- **Survives everything but a reboot.** The run lives in its own session:
  closing the terminal, agent tool timeouts, or killing the caller don't stop
  it.
- **Understands logs.** Language parsers (Java, Python, Rust, Go) with
  build-tool parsers on top (Maven, pytest, cargo, go test, nix), plus
  generic/JSON/logfmt. A stack trace is one event, not 40 lines. New formats
  are a TOML file, no recompile.
- **Bounded output by default.** Every read is capped, so an agent's context
  never floods.
- **Pings you when done.** `--on-complete` runs a callback (tmux line, exec,
  webhook) when the run finishes.

Linux only. One static binary with no runtime dependencies.

## Install

The flake builds one self-contained static binary per architecture. Pick
whichever install style fits; all of them put `loglense` on `PATH`. In the
URLs below, replace `/path/to/loglense` with your checkout, or use
`github:<owner>/loglense` once the repo is hosted.

| Output | What |
|---|---|
| `loglense-static` | x86_64 musl, statically linked (recommended) |
| `loglense-static-aarch64` | aarch64 musl, cross-built from x86_64 |
| `loglense` | native (dynamically linked) build |
| `tarball` · `tarball-aarch64` | `loglense-<ver>-<target>.tar.gz`: the binary + this README |

The flake exposes `x86_64-linux` packages only. Copy a static binary anywhere
and it runs. `cargo install --path crates/loglense-cli` works from a checkout
too.

### Nix profile (per user, no system rebuild)

Install it once, tracking a branch:

```sh
nix profile add 'git+file:///path/to/loglense?ref=main#loglense-static'
```

**Update, one command:**

```sh
nix profile upgrade loglense-static
```

This re-resolves `ref=main` to its newest commit, rebuilds, and swaps the
binary in place. Nix only sees **committed** changes (a `git+file` flake
ignores uncommitted edits), so commit first. `loglense --version` shows the
commit you are on (`0.1.0+<rev>`). To undo an update, run
`nix profile rollback`.

To pin a release instead, install with `?ref=refs/tags/v0.1.0`. Tags never
move, so `upgrade` leaves a pinned install alone. Moving to another tag (or
switching a pinned install back to tracking `main`) is one line:

```sh
nix profile remove loglense-static && nix profile add 'git+file:///path/to/loglense?ref=refs/tags/v0.2.0#loglense-static'
```

### NixOS (flake-based system config)

Add loglense as an input of your system flake and put the package in
`environment.systemPackages` (or `home.packages` with home-manager):

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # A local checkout; or "github:<owner>/loglense", optionally pinned
    # with ?ref=refs/tags/v0.1.0.
    loglense.url = "git+file:///path/to/loglense?ref=main";
  };

  outputs = { nixpkgs, loglense, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        {
          environment.systemPackages = [
            loglense.packages.x86_64-linux.loglense-static
          ];
        }
      ];
    };
  };
}
```

Leave out `inputs.loglense.inputs.nixpkgs.follows`. loglense's own nixpkgs pin
is what keeps the static musl toolchain coming from the binary cache, and
following your system nixpkgs can mean building that toolchain from source.

**Update, one command** (run where your system flake lives, e.g. `/etc/nixos`):

```sh
nix flake update loglense && sudo nixos-rebuild switch --flake .#myhost
```

`nix flake update loglense` re-locks only that input to the newest commit of
its ref. The rebuild then installs it, and the previous generation stays
bootable as the rollback. For a pinned tag, change the `?ref=` in `flake.nix`
first; the same command applies it.

## Use

| Command | What it does |
|---|---|
| `loglense <cmd…>` / `loglense run -- <cmd…>` | start a run, print its id, return immediately |
| `loglense ls` | runs, newest first, with status and error/warning counts |
| `loglense show <id>` | summary, test totals, top error templates |
| `loglense errors <id>` · `warnings <id>` | the events themselves (≤ 50) |
| `loglense tail <id>` | last 100 lines, stdout and stderr in arrival order |
| `loglense wait <id> [--timeout s]` | block until done; exit code 0 = ok, 1 = run failed, 124 = timeout |
| `loglense kill <id>` | TERM the run's process group, KILL after 5 s |
| `loglense gc` | drop runs older than 14 days / beyond 2 GB |
| `loglense init` | create `.loglense/` here and add it to `.gitignore` |

Every read command takes `--json`. Every cap can be raised with `--max` or
`--lines`. If the wrapped command itself starts with a loglense word (`run`,
`show`, …), use `loglense run -- <cmd…>`. The store is found by walking up
from the current directory, stopping at the git root. Override it with
`--dir` or `LOGLENSE_STATE_DIR`.

Full contracts: [`docs/surfaces.md`](docs/surfaces.md). Design:
[`docs/architecture.md`](docs/architecture.md),
[`docs/decisions.md`](docs/decisions.md).

## Agents: MCP

`loglense mcp` is a stdio MCP server with eight tools: `start_run`,
`list_runs`, `get_run`, `get_events`, `get_log`, `wait_run`, `kill_run` and
`leave_feedback`. Wire it into `.mcp.json`:

```json
{ "mcpServers": { "loglense": { "command": "loglense", "args": ["mcp"] } } }
```

Use an absolute `command` path if the binary isn't on the agent's PATH. MCP
servers usually start in `$HOME`, so **always pass `cwd`** (the project
directory) to `start_run` and `list_runs`. loglense never uses the server's
own working directory.

A 60-second loop: `start_run(["cargo","test"], cwd)` → `wait_run(id)` (≤ 120 s
per call, call again while `timed_out`) → `get_run(id)` → `get_events(id,
severity="error")` → `get_log(id, stream="both", tail_n=50)`. If a line was
misclassified, `leave_feedback(id, kind="parse-miss", line=…, expected_severity="error")`.

## Completion callbacks

`--on-complete '<cmd>'` (MCP: `on_complete`) runs `<cmd>` via `sh -c` once the
run is final. It gets `LOGLENSE_RUN_ID`, `LOGLENSE_STATUS`,
`LOGLENSE_EXIT_CODE` and `LOGLENSE_STORE_PATH` in its environment and the
summary JSON on stdin. It runs once, is killed after 30 s
(`--on-complete-timeout`), and its output goes to `callback.log`. The result
(`ok` / `failed` / `timeout`) is shown by `show`. Polling with `wait` still
works if the supervisor was killed.

Recipes:

- **TUI agent in tmux** (types a line into the agent's pane and presses Enter):
  `--on-complete 'loglense notify tmux --pane %3'` (find the pane with
  `tmux display -p '#{pane_id}'`). Change the layout with
  `--msg-fmt '{id} {status} E={errors}'`; only `{id} {status} {exit} {errors}
  {warnings}` are allowed.
- **Headless harness / any CLI**: `--on-complete 'loglense notify exec -- my-harness resume'`
  passes the env vars and the summary on stdin through unchanged.
- **Webhook** (there's no HTTP client in the binary on purpose):
  `--on-complete 'curl -fsS -m 10 -H "content-type: application/json" --data-binary @- https://example.invalid/hook'`

## New log formats

Drop a TOML pack into `.loglense/formats.d/` (project) or
`~/.config/loglense/formats.d/` (user). It can `extends = "java"` (or
python/rust/go) to inherit stack-trace handling. Check it with
`loglense parsers validate pack.toml` and try it with
`loglense parsers test pack.toml --sample build.log`. The grammar and a worked
example are in [`docs/parsers.md`](docs/parsers.md).

When a parser gets something wrong, `loglense feedback leave <id> --kind
parse-miss --line N --detail …` records it. In a loglense checkout,
`loglense feedback promote <fid>` turns it into a failing golden fixture.

## License

MIT
