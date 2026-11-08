# Sync protocol specs

TLA+ models of the `ti sync` protocol, checked with TLC. They are not run in
CI; a maintainer runs them by hand when the sync design changes.

| File | Models | Expected result |
|------|--------|-----------------|
| `Sync_v0.tla` + `Sync_v0.cfg` | ti <= 0.4.2: Deletion is key absence, Reindex replaces the index with the remote tree | `NoLostLocalOp` **violated**: the trace is incident 215ee3 |
| `Sync.tla` + `Sync.cfg` | ADR 0003: Deletion and Undeletion are Operations, Reindex is additive and runs on every pull | `NoLostLocalOp`, `NoResurrection`, `Convergence`, `RemoteKeepsOps` **hold** |

## Running

Download `tla2tools.jar` from <https://github.com/tlaplus/tlaplus/releases>
(the v1.8.0 nightly or newer) into this directory (gitignored) and run:

```sh
java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC -workers auto -config Sync_v0.cfg Sync_v0.tla
java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC -workers auto -config Sync.cfg Sync.tla
```

`Sync_v0` finishes in a second. `Sync` with the committed `Sync.cfg` takes
about two minutes on 16 cores. TLC
writes a `states/` directory and `*_TTrace_*.tla` files next to the spec;
both are gitignored.

## Abstraction

Both specs share one model, so the fix is the diff between them.

- Two clones (`a`, `b`) and one remote. `local[c]` is a clone's index
  (`git-meta.sqlite`), `remote` the tree at `refs/{ns}/main`.
- A ticket is a set of members (comments, tags, Dependencies and
  Sub-issues, all "sets and lists" in git-meta) with member tombstones. In
  `Sync_v0` it also has an `exists` flag (its keys are present); in `Sync`
  it has a set of Operations (`create`, `del`, `undel`) and is visible iff
  it has keys and every `del` is covered by a later `undel`.
- git-meta's three-way merge (`tree/merge.rs`) per key: unchanged on one
  side takes the other side; changed on both sides unions, with a live
  value winning over a tombstone; removed on one side and modified on the
  other, the modified side wins.
- A Sync is `Pull` then `Push`. `Push` succeeds only if the remote has not
  moved since the clone's last `Pull` (fast-forward); otherwise the clone
  must `Pull` again, which is the race window retry.
- `Sync_v0`'s `Pull` runs the stranded-index self-heal of
  `TicketStore::pull`: if a ticket in the remote tree has no keys in the
  merged index, Reindex, and Reindex is `local := remote`. `Sync`'s `Pull`
  always follows the merge with Reindex, which is "gain what the fetched
  tree has and the index lacks". The conditional form was tried first; TLC
  showed a stranded ticket the remote has appended an op to slips past the
  no-keys test and is pushed without its earlier operations, which is why
  `Sync` also checks `RemoteKeepsOps`.
- `Push` keeps the ADR 0002 guard: it is refused when it would drop a remote
  ticket's keys (`Sync_v0` also models `--allow-deletes`).
- `Sync` adds `Strand(c, t)`: the index loses a ticket it has no unpushed
  work on, at any time. `Reindex(c)` (`ti reindex`, replaying the refs the
  clone last fetched) is likewise enabled at any time.
- Operation ids come from a counter bounded by `MaxOps`; `covers` on an
  `undel` stands for the `(lamport, id)` order. No git objects, hashes,
  Lamport values, signatures or verify.

## Properties

- `NoLostLocalOp`: everything a clone did since its last push (operations
  appended, members added, members removed) is still in its index. Members
  merge by state, not by edit, so "did" means relative to the last remote
  tip the clone merged: re-adding a member the remote already holds is not
  a local-only change, and an unpushed removal that another clone's
  concurrent add revives (git-meta's add-wins rule) is superseded, not lost.
  Both refinements were forced by TLC counterexamples that were legitimate
  merge outcomes, and are recorded as such in `Sync.tla`. `Sync_v0` tracks
  only pending adds, which is all the incident needs.
- `NoResurrection`: once a clone's index has held a deletion of a ticket,
  the ticket is visible there only if an Undeletion operation is in its log.
  In `Sync_v0`, where no undelete exists: a deleted ticket never exists
  again in that clone.
- `Convergence`: when every clone has pushed and the remote has not moved
  since, every index equals the remote.
- `RemoteKeepsOps` (`Sync` only, a temporal property): an operation on the
  remote is never removed by a push.

## Results

TLC2 Version 2026.11.06 (tla2tools from the v1.8.0 release page), 16 workers,
2026-11-08.

### `Sync_v0`

`Error: Invariant NoLostLocalOp is violated.` Trace:

1. `Delete(a, t1)`: clone a deletes t1 (keys removed).
2. `AddItem(a, t2, i1)`: clone a comments on t2 (unpushed).
3. `Pull(a)`: the merge keeps both changes, but t1 is in the remote tree with
   no local keys, so the stranded-index self-heal runs Reindex and the index
   becomes the remote tree: t1 is back, and the comment on t2 is gone.

699 states generated, 325 distinct, depth 6, under a second. Checking
`NoResurrection` instead gives the two-step trace `Delete`, `Pull`.

### `Sync`

`Model checking completed. No error has been found.`

25,574,505 states generated, 3,036,047 distinct states, depth 21,
1 min 35 s. `TypeOK`, `NoLostLocalOp`, `NoResurrection`, `Convergence` and
`RemoteKeepsOps` hold with `Clones = {a, b}`, `Tickets = {t1, t2}`, `Items = {i1}`,
`MaxOps = 3`.
