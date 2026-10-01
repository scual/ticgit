# `ti delete` command

**Date:** 2026-10-01
**Status:** Approved

## Goal

Expose ticket deletion on the CLI. `store.delete_ticket` already exists but is
only wired into the interactive TUI; there is no scriptable `ti delete`.

## Command

```
ti delete <id>... [--recursive/-r] [--yes/-y] [--json | --markdown]
```

## Decisions

- **Target:** one or more positional id/prefix args, **required**. No
  default-to-checked-out ticket (a destructive op must be explicit; `ti next`
  auto-checks-out its pick, so a bare default would be a footgun).
- **Validate-then-act:** resolve every id first. If any is unknown or an
  ambiguous prefix, fail non-zero and delete **nothing**.
- **Children (default):** orphan. `store.delete_ticket` already un-parents a
  deleted ticket's children (they survive as top-level tickets) and cleans
  reverse dependency refs.
- **`--recursive/-r`:** also delete the full descendant subtree. Collect
  descendants by walking `children` over the full ticket set, **including
  closed** descendants, with a visited-set cycle guard. Effective delete set =
  resolved ids ∪ their subtrees, deduped.
- **Confirmation:** prompt `Delete N ticket(s)? [y/N]` listing each
  `shortid — title (state)`; proceed only on `y`/`yes`. `--yes/-y` skips the
  prompt. `--json` is machine mode and **requires `--yes`** (error cleanly if
  absent). If stdin is not a TTY and `--yes` is absent, error clearly rather
  than hang.
- **Session state:** if any deleted id is the currently checked-out ticket for
  this git-dir, clear it from `session_state`.
- **Irreversible:** no trash/undo (git-meta has no undo here); the confirmation
  is the only guard.

## Output

Snapshot each ticket object **before** deleting (so output reflects what was
removed).

- **plain text:** `Deleted N ticket(s):` followed by one `shortid — title` line
  per deleted ticket.
- **`--json`:** a JSON **array of the deleted ticket objects** — reuses the
  existing `$defs.ticketList` schema shape (plain ticket objects; no
  `subissues`). Emitted even for a single id. **No schema change required.**
- **`--markdown`:** a `# Deleted tickets` heading + a bullet list.

JSON/markdown rules from the machine-output contract hold: JSON → stdout only,
no ANSI; errors → stderr; non-zero exit on failure.

## Architecture

- **New file:** `crates/ticgit/src/commands/delete.rs` — clap `Args` struct and
  `pub fn run(args: Args) -> anyhow::Result<()>`.
- **Wiring:** register the subcommand in `crates/ticgit/src/commands/mod.rs`
  (module decl), and in `crates/ticgit/src/cli.rs` (the clap `Command` enum
  variant, the dispatch arm, and the help/menu template).
- **Reuse:** `open_store()` (commands/mod.rs), `store.resolve_id(&str)` for id
  resolution, `store.delete_ticket(&Uuid)` for the deletion + ref cleanup,
  `store.load(&Uuid)` for pre-delete snapshots, `render` helpers for
  text/json/markdown, `session_state::State` for clearing a checked-out id.
- **Recursive collection:** a helper that, given the resolved root ids and the
  full ticket list, returns the deduped set of all ids to delete (roots +
  descendants), visited-guarded. Closed descendants included.

## Testing (`crates/ticgit/tests/cli.rs`)

- Delete a single ticket with `--yes` → gone from `ti list`; exit 0.
- `--json` returns an array containing the deleted ticket's object; `--json`
  without `--yes` fails non-zero with a clear message.
- Unknown/ambiguous id → non-zero, nothing deleted (a sibling valid id in the
  same invocation survives).
- Default (non-recursive) delete of a parent → children survive as top-level
  (verify via `ti list` / `ti show` on a former child: `parent` is null).
- `--recursive` delete of a parent → all descendants (including a closed one)
  are gone.
- Deleting the currently checked-out ticket clears session state (subsequent
  `ti show` with no id reports no current ticket rather than erroring on a
  dangling id).
- Multiple ids in one invocation all deleted.

## Out of scope

- Undo / trash / soft-delete.
- Wildcard, tag-based, or filter-based bulk delete.
- TUI changes (it already has delete).
