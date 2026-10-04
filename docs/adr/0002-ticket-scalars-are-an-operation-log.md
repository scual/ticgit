# Ticket scalar fields are projected from an append-only operation log

Tickets are shared through git-meta, so two clones routinely edit the same ticket offline. Storing scalars (title, state, priority, …) as plain keys makes the merge last-write-wins: editing the title in one clone and the state in another loses one of the edits. We instead record every scalar change as an immutable Operation with a content-derived SHA-256 id and a Lamport clock. Clones merge by unioning operations and replaying them in `(lamport, id)` order, so concurrent edits to different fields both survive and the result is identical on every clone. Operations may carry an SSH signature checked against the Identity chain, and `ti verify` replays the log to check ids, signatures and projection.

Only singleton scalars are op-managed. Sets (tags, dependencies, sub-issues) and comments already merge conflict-free as git-meta values, and `meta:*` is low-conflict, so they keep their own keys. A per-ticket `format-version` stays a plain key so older binaries refuse op-based tickets (`FormatTooNew`) instead of misreading them; `ti migrate` rolls v1 tickets forward.

This is hard to undo: once operations are synced, every clone depends on the log layout and id derivation, and a plain-key reader sees stale scalars.

## Considered Options

- Plain keys, last-write-wins: simplest and what v1 did, but silently drops concurrent edits to different fields.
- Per-field merge by timestamp: fixes different-field clobbering but depends on wall clocks and gives no tamper evidence or deterministic tiebreak.

## Consequences

- Reads project the log (plus the plain-key sets and comments), so a ticket is a hybrid of both stores.
- Unknown operation kinds are skipped on replay so newer writers don't break older readers.
- Unsigned operations are trusted by default and only warned about by `ti verify`.
