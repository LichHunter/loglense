# AGENTS.md: working on loglense

## Fast lane (must pass before every commit)

```sh
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

The dev shell is devenv + direnv (`.envrc`: `use devenv`). Packaging lives in
`flake.nix`: `nix build .#loglense`, `.#loglense-static`,
`.#loglense-static-aarch64`, `.#tarball`, `.#tarball-aarch64`.

## Crate map

| Crate | Owns |
|---|---|
| `loglense-core` | run ids, the `Event` model, config layering, template normalization/dedup. Sync, no I/O beyond config files |
| `loglense-parsers` | `Parser` / `ParseState` traits, registry + `extends`, chains, autodetect, TOML packs, built-in bases (`src/bases/`) and tools (`src/tools/`), `standard_registry` |
| `loglense-store` | `.loglense/` discovery, run dirs, atomic meta, events/interleave/feedback files, gc, and `query`: the bounded reads shared by CLI and MCP |
| `loglense-runner` | `spawn_run` (setsid re-exec of `loglense __supervisor <run-dir>`), the supervisor (capture, parse-on-write, summary, callbacks), `kill_run` / `wait_run`. All libc calls are in `src/sys.rs` |
| `loglense-mcp` | rmcp stdio server, 8 tools. stdout is protocol-only: `print_stdout` / `unwrap` / `expect` are denied in this crate |
| `loglense-cli` | the `loglense` binary: prefix dispatch, subcommands, rendering |

## Conventions

- **Bounded reads are an invariant.** Every surface caps item counts and item
  size (`loglense_store::query::{MAX_TEXT, MAX_FRAMES}`). Never add a read
  path without a cap.
- **Tests run real processes.** Runner/CLI/MCP tests spawn real commands
  through the real binaries. Clean up what you spawn: use process groups and
  null stdio so a leaked child can't hold the harness pipes.
- **Golden fixtures:** `crates/loglense-parsers/tests/fixtures/<parser>/<case>.log`
  plus `<case>.expected.json`, discovered at test runtime. Write the fixture
  first (RED), then the parser. A new parser directory needs one arm in
  `parser_for` in `tests/golden.rs`.
- **Promoted feedback** lands as `<fid>.log` with an empty
  `<fid>.expected.json`. It fails until someone fills in the expected events.
  Never generate expected events from the current parser output: that would
  lock in the bug.
- **Decision records:** a new cross-cutting choice (a dependency, a storage
  file, a contract change) needs a new `D-0xx` in `docs/decisions.md`, and the
  affected docs updated in the same commit. `docs/` holds no code blocks
  except mermaid diagrams.
- **Commits:** conventional commits, one per task.
