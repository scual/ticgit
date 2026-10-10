# `@` sentinel for the checked-out ticket — design

Ticket: `2ee5e0` — CLI-wide sentinel for "the checked-out ticket".

## Problem

Many commands default to the checked-out ticket when `-t` is omitted, but flags
that take a *required* ticket value (e.g. `ti list --blocks X`, `--depends-on X`
from 2870b8) cannot default — there is no way to say "the current ticket" as a
flag value. Users want to write `ti list --blocks @`.

## Constraint that shapes the design

The notion of "the checked-out ticket" lives in the CLI's `session_state`, not
in `ticgit-lib`. The library is deliberately git/session-agnostic:
`TicketStore::resolve_id()` takes no "current" context, and the
`SessionGitDir` trait keeps that knowledge in the commands layer. Therefore the
sentinel must be resolved in the **CLI layer**, wrapping `store.resolve_id()`.
The library is not changed.

## Sentinel

`@` — the "self/current" convention (git uses `@` for HEAD). UUIDs are hex, so
`@` cannot collide with an id prefix; it does not clash with the `.` used for
shell/paths or with the `ti list <view>` positional slot. No `.` alias.

## Scope

`@` is accepted **anywhere a ticket reference is accepted**, resolved once in a
shared CLI helper so every command gets it for free: `ti show @`, `ti close @`,
`ti depends @ -t X`, `ti list --blocks @`, `ti list --depends-on @`, `-t @`, etc.

## Design

New helper in `crates/ticgit/src/commands/mod.rs`:

```rust
/// Resolve a ticket reference, expanding the `@` sentinel to the
/// checked-out ticket. All other input defers to the store's id resolver.
pub fn resolve_ref(store: &TicketStore, reference: &str) -> Result<Uuid> {
    if reference.trim() == "@" {
        let state = State::load().unwrap_or_default();
        let git_dir = store.session().repo_git_dir();
        return state.current_for(&git_dir).ok_or_else(|| {
            anyhow!("`@` means the checked-out ticket, but none is checked out - run `ti checkout <id>` first")
        });
    }
    Ok(store.resolve_id(reference)?)
}
```

- `resolve_ticket()`'s explicit branch routes through `resolve_ref` so `-t @`
  (and any positional id) works everywhere with no per-command change.
- The `--blocks` / `--depends-on` value resolution in `commands/list.rs`
  routes through `resolve_ref` too.
- `ticgit_lib::TicketStore::resolve_id()` is untouched.

## Saved views

Already resolve the flag value to a full UUID before recording the view
(decided in 2870b8). Because `@` resolves to a concrete UUID at `ti list` time,
a saved view pins that id — the sentinel is never persisted. No code change;
covered by a regression test.

## Empty state

`@` used with nothing checked out exits non-zero with a clear message naming
`ti checkout`.

## Testing (CLI integration, TDD)

1. `ti checkout X` then `ti show @ --json` → ticket X.
2. `ti list --blocks @` resolves to the checked-out ticket (returns the
   expected tickets).
3. Save a view built from `--blocks @`, re-run it → still pins X's full id
   (`ti views list` shows the full id, not `@`).
4. `ti show @` (or `--blocks @`) with nothing checked out → non-zero exit,
   message mentions `ti checkout`.

## Out of scope

- A `.` alias or any sentinel other than `@`.
- Defaulting a flag *value* to the current ticket without the sentinel.
- Exposing the sentinel in the TUI.
