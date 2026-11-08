# Deletion is an operation; reindex is additive

ADR 0002 left ticket deletion as the one scalar change that is not an Operation: `ti delete` removes the ticket's keys, so a deleted ticket and a ticket the local index never materialized look the same. `TicketStore::pull` tells a stranded index apart from a complete one by looking for remote tickets with no local keys, and heals it with `reindex`, which drops `refs/{ns}/local/main` and replays the remote tree over the index. A local deletion therefore triggers the heal, and the heal replaces the index with the remote tree: the deleted ticket returns and every unsynced local comment, tag, Dependency and Sub-issue on every ticket is dropped. That is the incident of ticket 215ee3, and it reproduces in the model with one clone and no remote activity.

We make Deletion and Undeletion Operations in the ticket's log (`del`, `undel`), projected like any other kind: a ticket is visible iff every `del` is covered by a later `undel`. Deleted tickets keep their keys, history, Dependencies and Sub-issues and are hidden by default. Reindex becomes additive: the index gains what the fetched refs hold and it lacks, and never removes or rewrites a key, a member or a tombstone. Because it is additive, pull runs it after every merge instead of guessing whether the index is stranded: the model showed the guess ("a remote ticket with no local keys") misses a stranded ticket the remote has since appended an operation to, and the merge then pushes that ticket without its earlier operations. The ADR 0002 push guard against implicit deletion stays as is, and a deleted ticket, keeping its keys, never trips it.

Two TLA+ specs under `docs/spec/` carry the decision. `Sync_v0.tla` models the shipped protocol and TLC violates `NoLostLocalOp` in three steps (`Delete`, `AddItem`, `Pull`): the pull resurrects the deleted ticket and drops the local-only member. `Sync.tla` models this ADR and TLC passes `NoLostLocalOp`, `NoResurrection`, `Convergence` and `RemoteKeepsOps` (no push drops an operation from the remote) over every interleaving of two clones, the push race window, an index stranded at any moment and `ti reindex` run at any moment (3,036,047 distinct states, depth 21, two tickets, one member, three delete/undelete operations). The glossary terms Deletion, Undeletion, Reindex and Stranded index in `CONTEXT.md` are the vocabulary of this decision.

## Considered Options

- Keep key-absence deletion, make the pull self-heal skip tickets `ti delete` removed: needs a local list of deleted ids that is itself unsynced state, and still lets a concurrent remote edit resurrect the ticket through the "modified wins over removed" merge rule.
- Tombstone the ticket as a plain git-meta key instead of an Operation: merges last-write-wins against a concurrent undelete and gives no signed, ordered history; the op log already solves both.
- Additive reindex only, deletion unchanged: fixes the data drop but a deleted ticket still returns on every pull, and the stranded-index check stays ambiguous.
- Hard-delete that also propagates: would need tombstones that outlive the ticket's keys in every clone forever, and loses the history the Operation log exists to keep.

## Consequences

- `ti delete` is reversible with `ti undelete`; a deleted ticket is hidden from list, next, stats, search, Dependencies, Sub-issues and the TUI unless asked for.
- Edits that arrive for a deleted ticket are kept and reported, not dropped or silently applied.
- `ti reindex` is safe to run at any time, and every Sync runs one; replacing the index wholesale is a separate, guarded `--reset` that refuses while unsynced work exists.
- Nothing leaves the store any more; a purge of deleted tickets, if ever wanted, is a new decision.
- The specs are the reference for the sync invariants; a change to merge or reindex semantics updates them first and re-runs TLC by hand (not CI).
