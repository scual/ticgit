# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

TicGit is a Git-native issue tracker written in Rust (2021 edition). Tickets are
not stored in files or a database checked into the tree — they live as
[git-meta](https://crates.io/crates/git-meta-lib) metadata under the `ticgit:`
namespace on the git-meta `project` target. The binary is named `ti`.

## Build / test / run

```sh
cargo build                          # debug build of the whole workspace
cargo build --release                # optimized
cargo install --path crates/ticgit --locked   # install `ti` to ~/.cargo/bin
cargo test                           # run all tests
cargo test -p ticgit --test cli      # run only the CLI integration suite
cargo test -p ticgit --test cli <name>   # run tests whose name matches <name>
cargo clippy --all-targets           # lint
cargo fmt                            # format
```

Run the CLI from source: `cargo run -p ticgit -- <args>` (e.g. `cargo run -p ticgit -- list`).

## Workspace layout

Two crates (`Cargo.toml` is a virtual workspace, version is shared via
`workspace.package`):

- **`crates/ticgit-lib`** — the domain library. Knows nothing about clap or
  terminals. Public surface is re-exported from `lib.rs`.
- **`crates/ticgit`** — the `ti` CLI (and TUI) built on top of the library.

### Library architecture (`ticgit-lib`)

- `keys.rs` — **single source of truth for the on-the-wire key layout**
  (`ticgit:tickets:<uuid>:<field>`, `ticgit:writeups:...`, `ticgit:views:...`).
  Every read/write formats keys through these helpers; change the storage
  layout here, not inline.
- `store.rs` — `TicketStore`, the bridge between the `Ticket` domain model and a
  git-meta `Session`. **There is no index** — tickets are discovered by
  prefix-scanning `ticgit:tickets`. Open via `TicketStore::discover()` (from cwd)
  or `::open(repo)` / `::from_session(session)` (tests).
- `ticket.rs` — the `Ticket` model plus the lifecycle types. Lifecycle is split
  into a broad `status` (`open`/`closed`) and a specific `state` (`new`,
  `assigned`, `in-progress`, `blocked`, `review` when open; `resolved`,
  `wontfix`, `duplicate`, `invalid` when closed).
- `query.rs` — `Filter` / `SearchFilter` / sort keys for `list`.
- `writeup.rs` — "writeups": versioned markdown docs that can be promoted to tickets.
- `error.rs` — `Error` / `Result`.

### CLI architecture (`ticgit`)

- `main.rs` → `cli::run` — clap parse + dispatch. The command menu and help
  template live in `cli.rs`.
- `commands/` — one module per subcommand, each exposing a clap `Args` struct
  and `run(args) -> anyhow::Result<()>`. `commands/mod.rs` has shared helpers:
  `open_store()` (auto-runs setup after a fresh clone if a `.git-meta` file
  exists but no remote is configured) and `resolve_ticket()` (uses the explicit
  `--ticket` arg, else the currently checked-out ticket).
- `render.rs` — all output formatting: tables, single-ticket detail, `--json`,
  and `--markdown`.
- `session_state.rs` — **the only ticgit state that lives outside the repo.**
  Maps a git-dir path → currently checked-out ticket UUID (plus last-used
  filters / views / UI settings), stored under the OS state/cache dir. This is
  what lets `ti show` / `ti comment` work with no explicit id. Tests override the
  location with the `TICGIT_STATE_FILE` env var.

## Machine output contract (don't break this)

`--json` is a stable interface with a published schema at `docs/schema/v1.json`.
Every command that supports `--json` also supports `--markdown`. Rules enforced
by the CLI and integration tests:

- successful JSON goes to **stdout only**; diagnostics/errors go to **stderr**
- JSON output carries **no ANSI color**
- non-zero exit status means failure; ambiguous/missing id prefixes fail non-zero
- ticket ids may be full UUIDs or any unique prefix
- `ti show --json` / mutations emit a ticket object; `ti list --json` emits an array

## Testing approach

The heavy coverage is in `crates/ticgit/tests/cli.rs` (~48 integration tests):
each builds a throwaway git repo in a tempdir (`assert_cmd` + `tempfile`) and
drives the real `ti` binary end-to-end, asserting on stdout/stderr/exit and
parsing JSON with `serde_json`. Library-level tests use
`ticgit_lib::test_support::test_store()` (gated behind `cfg(test)` / the
`test-support` feature). Prefer a CLI integration test for anything
user-observable; the tempdir pattern keeps tests hermetic.

## Dependency pin to know about

`gix-actor` is pinned to `=0.40.0` in `Cargo.toml`. `0.40.1` bumped `winnow` to
1.0 but `gix-object` 0.58 still needs `winnow` 0.7. Don't unpin without checking
the `gix` stack.

## Release

`scripts/bump-version.sh <version>` updates the workspace version in `Cargo.toml`
and the version strings in `docs/index.html`. Pushing a `v*` tag triggers
`.github/workflows/release.yml`, which cross-builds binaries for
linux/macOS/windows and attaches them to a GitHub release.

## Conventions

- Terminology is defined in `CONTEXT.md` — notably **Dependency**: "A depends on
  B" and "B blocks A" are the same directed relation (the `ti depends` command and
  its `--depends-on` / `--blocks` filters follow this). Avoid "link", "blocked by",
  "requires".
- `docs/agents/` documents agent-facing workflows (issue tracker, triage labels,
  domain docs). `AGENTS.md` points agents at `ti agent` for the full workflow.
