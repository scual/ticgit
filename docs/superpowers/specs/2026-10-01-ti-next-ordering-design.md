# `ti next` ordering fix — design

Date: 2026-10-01

## Problem

With every open ticket at priority `none`, `ti next` tie-breaks to oldest-created
and surfaces deferred work (e.g. a ticket tagged `future`). The ordering had two
defects:

1. **`none` is the floor.** In the old `score()` the priority term was
   `(max_priority + 1 - p) * 50`; `none` contributed `0` while *any* numeric
   priority contributed a positive amount. So any number — even `100` — ranked a
   ticket *above* every unprioritized ticket. There was no number that could sink
   a ticket below the unprioritized pile. Setting `ti priority 100` *promoted* the
   ticket instead of demoting it.
2. **No documented sort key.** `ti next --help` listed only `--tag`/`--assigned`;
   the actual ordering (a weighted blend of state, priority, points, assigned,
   age) was undocumented and hard to reason about.
3. **No non-abusive park.** The only lever that removed a ticket from `ti next`
   was setting `open:blocked`, a semantic stretch for "deferred".
4. **Blocked exclusion undocumented.** Blocked was a `-200` score penalty, not a
   documented behavior.

## Decisions (confirmed with user)

1. **`none` stays the least-important band** (the floor). A numeric priority —
   even a large one — always ranks *above* unprioritized tickets. Numbers cannot
   cross below `none`. This is now documented, and users are pointed at the
   deferred mechanism to park work.
2. **Tag-based deferred.** `ti next` excludes tickets tagged `deferred` or
   `backlog` by default; `ti next --include-deferred` restores them. Reuses tag
   infrastructure; no new lifecycle state, no new storage.
3. **Lexicographic sort key**, replacing the weighted blend. Priority is the
   dominant lever.
4. **Blocked is kept as a demotion, not an exclusion.** It sorts last among
   states rather than being filtered out.

## Design

### Ordering function (library)

Extract filtering + ordering out of `crates/ticgit/src/commands/next.rs` into a
pure, unit-testable function in `crates/ticgit-lib/src/query.rs`:

```rust
pub const DEFERRED_TAGS: &[&str] = &["deferred", "backlog"];

pub struct NextOptions {
    pub tag: Option<String>,
    pub assigned: Option<String>,
    pub include_deferred: bool,
}

/// Open, actionable tickets ordered best-first for `ti next`.
pub fn next_queue<'a>(tickets: &'a [Ticket], opts: &NextOptions) -> Vec<&'a Ticket>;
```

`next.rs` calls `next_queue`, takes the first result, checks it out, and renders.

### Exclusions

A ticket never appears in `ti next` if any hold:

- it is closed (`status == Closed`)
- it is a sub-issue (`parent.is_some()`)
- it has an unresolved dependency (a `depends_on` id whose ticket is not closed)
- it is tagged `deferred` or `backlog` — unless `include_deferred`

Plus the existing `--tag` / `--assigned` narrowing filters.

Blocked tickets are **not** excluded; they sort last (see key).

### Sort key

Ascending tuple, first element most significant; the first ticket is the one to
work on next:

1. **priority** — numeric priorities first, ascending (lower = more important);
   `none` sorts last. Encoded as `(priority.is_none(), priority.unwrap_or(0))`.
2. **state** — `in-progress` < `assigned` < `review` < `new` < `blocked`
   (blocked last).
3. **created_at** — oldest first.

The old weighted factors (points bonus, assigned bonus, age cap) are dropped so
the key is precise and documentable.

### Documentation

State the semantics in: `priority.rs` help, `next.rs` help / `cli.rs` menu,
`CONTEXT.md`, and the agent guide (`crates/ticgit/agents.md` + any synced copy):

> `none` is the least-important priority band. A numeric priority — even a large
> one — always ranks above unprioritized tickets; numbers cannot sink a ticket
> below the unprioritized pile. To park deferred work so it drops out of
> `ti next`, tag it `deferred` or `backlog` (restore with `--include-deferred`),
> or set it `blocked` (sorts last). `ti next` excludes closed tickets,
> sub-issues, tickets with unresolved dependencies, and deferred-tagged tickets.

## Tests

Library unit tests on `next_queue` plus CLI integration tests in
`crates/ticgit/tests/cli.rs`:

- numeric priority ranks above `none`
- numeric priorities order ascending (1 before 5)
- blocked sorts after non-blocked at equal priority
- `deferred` / `backlog` tagged tickets are excluded; `--include-deferred`
  restores them
- closed, sub-issue, and unresolved-dependency tickets are excluded
- `created_at` breaks ties (oldest first)

## Out of scope

- No new lifecycle state.
- No change to the `priority` storage format or to `ti priority` beyond help text.
- No configurable deferred-tag set (the constant `["deferred", "backlog"]` is
  fixed for now).
