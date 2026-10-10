<p align="center">
  <img src="assets/logo.svg" width="96" height="96" alt="TicGit logo">
</p>

<h1 align="center">TicGit</h1>

<p align="center"><strong>Your issue tracker lives in your Git repo — no server, no SaaS, no lock-in.</strong></p>

<p align="center">
  Tickets are stored as <a href="https://crates.io/crates/git-meta-lib">git-meta</a> metadata and travel with your
  history. Every change is a signed, conflict-free operation, so two people can edit the
  same ticket offline and both edits survive the merge.
</p>

<p align="center">
  <a href="https://crates.io/crates/ticgit"><img src="https://img.shields.io/crates/v/ticgit.svg?logo=rust" alt="crates.io"></a>
  <img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license">
  <img src="https://img.shields.io/badge/rust-2021-orange.svg?logo=rust" alt="Rust 2021">
  <img src="https://img.shields.io/badge/output-json%20%7C%20markdown-8A2BE2.svg" alt="JSON + Markdown output">
</p>

<p align="center">
  <img width="900" alt="ti on the command line" src="https://github.com/user-attachments/assets/f5ff1a77-644d-47ba-80eb-77e7b7ee66cb">
</p>

---

## Why TicGit

Issue trackers usually live somewhere else — a web app, a database, a vendor. TicGit
puts them where the code is: **inside the Git repository itself**, as metadata under
`refs/meta/*`. That means:

- **Offline-first & distributed.** Clone, fork, and work on a plane. Sync over any Git remote.
- **No separate database in your tree.** Tickets don't clutter your working directory.
- **It merges.** Ticket state is an append-only **operation log** (CRDT) — concurrent edits to
  different fields never clobber each other under last-write-wins.
- **Auditable & signable.** Every operation has a content-derived id and can be signed with
  your existing SSH key, exactly like Git commit signing.
- **Built for humans _and_ agents.** Rich terminal UI, plus stable `--json` and `--markdown`
  output for scripting and AI workflows.

The binary is called **`ti`**.

## Highlights

|                          |                                                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------------- |
| 🧩 **Git-native**        | Tickets ride `refs/meta/*`; nothing extra in your working tree.                                         |
| 🔀 **Conflict-free**     | Scalar fields are an op-log; merges union operations and replay deterministically.                      |
| 🔐 **Signed history**    | SSH-signed operations (`ssh-keygen -Y`), verified by `ti verify`.                                       |
| 🔎 **`ti verify`**       | Consistency oracle: recomputes every op's content id and checks signatures.                             |
| 🪜 **Versioned storage** | Per-ticket format versioning + `ti migrate` for safe, forward-only upgrades.                            |
| 🤖 **Agent-ready**       | `--json` (stable schema) and `--markdown` with next-step hints on every command.                        |
| 🖥️ **TUI**               | `ti tui` for an interactive terminal browser.                                                           |
| 🧠 **Specs & writeups**  | Attach implementation specs; promote rough notes into tickets.                                          |
| 🧰 **IDE plugin**        | A [JetBrains UI plugin](https://plugins.jetbrains.com/plugin/34640-ticgit-ui) drives the same `ti` CLI. |

<p align="center">
  <img width="900" alt="ti tui" src="https://github.com/user-attachments/assets/8c648b6a-0c13-4234-a11b-963fff4a7a2f">
</p>

## Install

Pre-built binary:

```sh
curl -fsSL https://ticgit.dev/install | sh
```

From crates.io:

```sh
cargo install ticgit
```

From source:

```sh
cargo install --path crates/ticgit --locked
```

## Quick start

```sh
git init
git config user.email you@example.com
git config user.name "Your Name"

ti init
ti new --title "fix the parser" --tags bug,parser --comment "fails on empty input"
ti list
ti show <id>          # full UUID or any unique prefix
```

Pick something to work on, make it current, and leave a note:

```sh
ti next               # best next ticket (skips blocked / dependency-gated)
ti checkout <id>
ti comment "on it"
ti state in-progress
ti close <id>
```

## Command reference

| Area                | Commands                                                                                                   |
| ------------------- | ---------------------------------------------------------------------------------------------------------- |
| **Create & browse** | `new` · `list`/`ls` · `show` · `recent` · `mine` · `history` · `tui`                                       |
| **Work on tickets** | `checkout`/`co` · `next` · `edit` · `comment` · `state`/`status` · `close` · `delete`                      |
| **Ticket fields**   | `tag` · `assign` · `priority` · `points` · `milestone` · `subissue` · `code` · `depends` · `spec` · `meta` |
| **Views & import**  | `views` · `writeup` · `review` · `stats` · `import gh\|linear`                                             |
| **Team**            | `users`                                                                                                    |
| **Sync & setup**    | `sync` · `pull` · `push` · `init` · `setup` · `update`                                                     |
| **Maintenance**     | `migrate` · `verify` · `reindex`                                                                           |
| **Agents**          | `agent` · any command with `--markdown`                                                                    |

Run `ti` with no arguments for the grouped help menu, or `ti <command> --help` for flags.
Every command that supports `--json` also supports `--markdown`.

Lifecycle is a broad **status** (`open` / `closed`) plus a specific **state** — open:
`new`, `assigned`, `in-progress`, `blocked`, `review`; closed: `resolved`, `wontfix`,
`duplicate`, `invalid`. Closing is blocked while a ticket still has an open sub-issue or an
unresolved dependency, so "done" never hides unfinished work (`--force` overrides).

## The operation log

TicGit's storage is an **operation-based CRDT**, inspired by
[git-bug](https://github.com/git-bug/git-bug). A ticket's scalar fields (title, state,
priority, assignee, …) are not stored as mutable values — they're the result of replaying an
append-only log of operations:

```text
ticgit:tickets:<uuid>:ops:<lamport>:<hash>   # one immutable operation
```

- **Content-derived ids.** Each operation's id is the SHA-256 of its content, so identical
  operations dedupe and equal logical clocks tie-break deterministically.
- **Lamport ordering.** Operations replay in `(lamport, id)` order — identically on every clone.
- **Conflict-free merge.** Divergent clones union their operation sets; no three-way field
  merge, no last-write-wins clobber. Edits to _different_ fields both survive; edits to the
  _same_ field resolve deterministically and the loser stays recoverable in history.
- **Signed authorship.** Set `TICGIT_SIGNING_KEY` to an SSH private key and operations are
  signed with `ssh-keygen -Y` (git-signing style); the public key is published to a synced
  identity chain. Unsigned operations are trusted by default and flagged by `ti verify`.

Two maintenance commands back this up:

```sh
ti verify            # replay + integrity check: content ids, signatures, projectability
ti migrate           # dry-run plan to roll tickets to the current on-disk format
ti migrate --write   # apply it (idempotent)
```

Sets (`tags`, sub-issues, dependencies), comments, and arbitrary `meta:*` fields keep their
own git-meta keys — they already merge cleanly — so the op-log carries exactly what needs it.

## Machine output (agents & scripts)

`--json` is a **stable interface** with a published schema at
[`docs/schema/v1.json`](docs/schema/v1.json) (also at
[`https://ticgit.dev/schema/v1.json`](https://ticgit.dev/schema/v1.json)):

- successful JSON goes to **stdout only**; diagnostics and errors go to **stderr**
- JSON output carries **no ANSI color**
- non-zero exit status means failure; ambiguous/missing id prefixes fail non-zero
- ids may be full UUIDs or any unique prefix
- `ti show --json` / mutations emit a ticket object; `ti list --json` emits an array
- `ti next --json` emits a ticket object, or `{ "next": null }` when nothing is workable

Point an agent at the full workflow guide with `ti agent`.

## IDE integration

A JetBrains IDE plugin — **[TicGit UI](https://plugins.jetbrains.com/plugin/34640-ticgit-ui)** —
browses, creates, and manages tickets in a tool window and drives this same `ti` CLI, so the
command line and the IDE share one source of truth.

## What it stores

All data lives on the git-meta `project` target under the `ticgit:` namespace:

```text
ticgit:schema-version                        string
ticgit:owners                                set of emails
ticgit:identities:<email>                    SSH public key (op signing)
ticgit:views:<name>                          set of ticket UUIDs
ticgit:tickets:<uuid>:ops:<lamport>:<hash>   operation (scalar fields, source of truth)
ticgit:tickets:<uuid>:format-version         string (on-disk format)
ticgit:tickets:<uuid>:tags                   set
ticgit:tickets:<uuid>:children               set of child UUIDs
ticgit:tickets:<uuid>:depends_on             set of UUIDs
ticgit:tickets:<uuid>:blocks                 set of UUIDs
ticgit:tickets:<uuid>:comments               list of JSON {author, body}
ticgit:tickets:<uuid>:meta:<key>             string (custom fields)
```

There is no separate ticket index — a ticket exists because its keys do. Exchange with other
clones happens through `refs/meta/*` over normal Git transfer; the local query database is
`.git/git-meta.sqlite`.

## Rust API

The workspace has two crates:

- **`ticgit-lib`** — the domain model and the git-meta-backed `TicketStore`.
- **`ticgit`** — the `ti` command-line application and TUI.

```rust
use ticgit_lib::{NewTicketOpts, TicketStore};

let store = TicketStore::discover()?;
let ticket = store.create("fix parser", NewTicketOpts::default())?;
println!("{}", ticket.id);
Ok::<(), ticgit_lib::Error>(())
```

## Development

```sh
cargo build                          # debug build of the workspace
cargo test                           # all tests
cargo test -p ticgit --test cli      # CLI integration suite
cargo clippy --all-targets           # lint
cargo fmt                            # format
cargo install --path crates/ticgit --locked   # install `ti`
```

## License

MIT © the TicGit authors. See [`LICENSE`](LICENSE).
