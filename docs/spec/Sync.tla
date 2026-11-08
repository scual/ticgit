----------------------------- MODULE Sync -----------------------------
(***************************************************************************)
(* TicGit Sync protocol after ADR 0003: deletion is an operation, reindex   *)
(* is additive.                                                            *)
(*                                                                         *)
(* `ti delete` appends a `del` Operation to the ticket's log and           *)
(* `ti undelete` appends an `undel` Operation covering the `del` ids it    *)
(* saw. A ticket is visible iff it has keys and every `del` is covered.    *)
(* Operations are never removed, so merging is union. Reindex only adds    *)
(* what the fetched remote tree has and the index lacks, so every pull     *)
(* runs it after the merge instead of guessing whether the index is        *)
(* stranded (a guess that fails when the remote appended an op to a        *)
(* ticket the index had lost). Since a deleted ticket keeps its keys, the  *)
(* push guard of ADR 0002 still keeps a stranded store off the remote.     *)
(*                                                                         *)
(* TLC checks NoLostLocalOp, NoResurrection, Convergence and RemoteKeepsOps *)
(* over every interleaving of two clones, including the push race window  *)
(* and an index that becomes stranded at any time.                         *)
(*                                                                         *)
(* Same abstraction as Sync_v0 (docs/spec/README.md). Operation ids are    *)
(* drawn from a counter in place of content hashes; `covers` stands for   *)
(* the (lamport, id) order in which an undelete follows the deletes it     *)
(* reverts.                                                                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Clones, Tickets, Items,
          MaxOps        \* total delete/undelete operations in a behaviour

OpIds       == 0..MaxOps
Op          == [kind: {"create", "del", "undel"}, id: OpIds, covers: SUBSET OpIds]
TicketState == [ops: SUBSET Op, items: SUBSET Items, tombs: SUBSET Items]
Store       == [Tickets -> TicketState]
\* What a clone has done since its last push and the remote does not hold.
Pending     == [ops: SUBSET OpIds, adds: SUBSET Items, removes: SUBSET Items]

CreateOp  == [kind |-> "create", id |-> 0, covers |-> {}]
Blank     == [ops |-> {CreateOp}, items |-> {}, tombs |-> {}]
NoKeys    == [ops |-> {}, items |-> {}, tombs |-> {}]
NoPending == [ops |-> {}, adds |-> {}, removes |-> {}]

HasKeys(ts)    == ts.ops # {}
DelOps(ts)     == {o \in ts.ops : o.kind = "del"}
UndelOps(ts)   == {o \in ts.ops : o.kind = "undel"}
Covered(d, ts) == \E u \in UndelOps(ts) : d.id \in u.covers
\* Projection of the log: a ticket shows unless an uncovered delete exists.
Visible(ts)    == HasKeys(ts) /\ \A d \in DelOps(ts) : Covered(d, ts)

VARIABLES
  local,        \* local[c]       : clone c's index
  remote,       \* the remote tree, refs/{ns}/main
  base,         \* base[c]        : remote tip c last merged or pushed; the merge
                \*                  base of its next pull, and the fetched refs
                \*                  `ti reindex` replays. Push is fast-forward
                \*                  iff base[c] = remote.
  pending,      \* pending[c][t]  : c's unpushed work on t (see Pending)
  everDeleted,  \* everDeleted[c][t] : c's index has held a `del` op for t
  upToDate,     \* upToDate[c]    : c pushed and nothing has moved since
  nextId        \* next operation id

vars == <<local, remote, base, pending, everDeleted, upToDate, nextId>>

TypeOK ==
  /\ local \in [Clones -> Store]
  /\ remote \in Store
  /\ base \in [Clones -> Store]
  /\ pending \in [Clones -> [Tickets -> Pending]]
  /\ everDeleted \in [Clones -> [Tickets -> BOOLEAN]]
  /\ upToDate \in [Clones -> BOOLEAN]
  /\ nextId \in 1..(MaxOps + 1)

Init ==
  /\ remote = [t \in Tickets |-> Blank]
  /\ local = [c \in Clones |-> remote]
  /\ base = [c \in Clones |-> remote]
  /\ pending = [c \in Clones |-> [t \in Tickets |-> NoPending]]
  /\ everDeleted = [c \in Clones |-> [t \in Tickets |-> FALSE]]
  /\ upToDate = [c \in Clones |-> TRUE]
  /\ nextId = 1

\* History: once a `del` op is in c's index for t, remember it.
History ==
  everDeleted' = [c \in Clones |-> [t \in Tickets |->
                    everDeleted[c][t] \/ DelOps(local'[c][t]) # {}]]

Touch(c) == upToDate' = [upToDate EXCEPT ![c] = FALSE]

(* ---- git-meta three-way merge, per ticket ----------------------------- *)

\* Operations are one key each: kept if on either side unless one side
\* removed it and the other left it unchanged (only a stranded index removes).
MergeOps(b, l, r) == (l \cap r) \cup (l \ b) \cup (r \ b)

MState(ts, i) ==
  IF i \in ts.items THEN "present" ELSE IF i \in ts.tombs THEN "tomb" ELSE "none"

\* Unchanged on one side: take the other. Changed on both: a live value wins
\* over a tombstone (merge_tombstones drops tombstones for live members).
MergeMember(b, l, r) ==
  IF l = r THEN l
  ELSE IF l = b THEN r
  ELSE IF r = b THEN l
  ELSE IF "present" \in {l, r} THEN "present"
  ELSE "tomb"

MergeTicket(b, l, r) ==
  LET m(i) == MergeMember(MState(b, i), MState(l, i), MState(r, i))
  IN [ops   |-> MergeOps(b.ops, l.ops, r.ops),
      items |-> {i \in Items : m(i) = "present"},
      tombs |-> {i \in Items : m(i) = "tomb"}]

Merge(b, l, r) == [t \in Tickets |-> MergeTicket(b[t], l[t], r[t])]

(* ---- additive reindex -------------------------------------------------- *)

\* The index gains what the fetched remote tree has and it lacks; nothing is
\* removed or rewritten. A member the index tombstoned stays tombstoned, a
\* member the index holds live is not tombstoned by a remote tombstone.
Gain(ts, rs) ==
  [ops   |-> ts.ops \cup rs.ops,
   items |-> ts.items \cup (rs.items \ ts.tombs),
   tombs |-> ts.tombs \cup (rs.tombs \ ts.items)]

Reindexed(st, tree) == [t \in Tickets |-> Gain(st[t], tree[t])]

(* ---- local edits ------------------------------------------------------- *)

\* Members merge by state, not by edit, so a change is local-only (pending)
\* only relative to the last remote tip the clone merged: re-adding a member
\* the remote already holds, or re-removing one it already tombstoned, is a
\* no-op for the merge and is tracked as such.

AddItem(c, t, i) ==
  /\ Visible(local[c][t])
  /\ i \notin local[c][t].items
  /\ local' = [local EXCEPT ![c][t].items = @ \cup {i}, ![c][t].tombs = @ \ {i}]
  /\ pending' = [pending EXCEPT ![c][t].adds = IF i \in base[c][t].items THEN @ ELSE @ \cup {i},
                                ![c][t].removes = @ \ {i}]
  /\ Touch(c) /\ History
  /\ UNCHANGED <<remote, base, nextId>>

RemoveItem(c, t, i) ==
  /\ i \in local[c][t].items
  /\ local' = [local EXCEPT ![c][t].items = @ \ {i}, ![c][t].tombs = @ \cup {i}]
  /\ pending' = [pending EXCEPT ![c][t].adds = @ \ {i},
                                ![c][t].removes = IF i \in base[c][t].tombs THEN @ ELSE @ \cup {i}]
  /\ Touch(c) /\ History
  /\ UNCHANGED <<remote, base, nextId>>

Append(c, t, op) ==
  /\ local' = [local EXCEPT ![c][t].ops = @ \cup {op}]
  /\ pending' = [pending EXCEPT ![c][t].ops = @ \cup {op.id}]
  /\ nextId' = nextId + 1
  /\ Touch(c) /\ History
  /\ UNCHANGED <<remote, base>>

\* `ti delete`: append a `del` op. Members and history stay.
Delete(c, t) ==
  /\ nextId <= MaxOps
  /\ Visible(local[c][t])
  /\ Append(c, t, [kind |-> "del", id |-> nextId, covers |-> {}])

\* `ti undelete`: append an `undel` op covering every `del` the clone sees.
Undelete(c, t) ==
  /\ nextId <= MaxOps
  /\ HasKeys(local[c][t]) /\ ~Visible(local[c][t])
  /\ Append(c, t, [kind |-> "undel", id |-> nextId,
                   covers |-> {d.id : d \in DelOps(local[c][t])}])

\* The index loses a ticket it has no unpushed work on (a worktree whose
\* store never materialized it, a lost store file): a Stranded index.
Strand(c, t) ==
  /\ HasKeys(local[c][t])
  /\ pending[c][t] = NoPending
  /\ local' = [local EXCEPT ![c][t] = NoKeys]
  /\ Touch(c) /\ History
  /\ UNCHANGED <<remote, base, pending, nextId>>

(* ---- sync -------------------------------------------------------------- *)

\* A member the clone removed but another clone concurrently added: the
\* merge rule lets the live value win (add-wins). The removal is superseded,
\* not dropped; git-meta reports it as a conflict decision (its word).
Superseded(c, t) ==
  {i \in pending[c][t].removes :
     MState(base[c][t], i) # "present" /\ MState(remote[t], i) = "present"}

\* Pending means local-only: a change the fetched remote already holds (the
\* same member added or tombstoned by another clone) is synced.
Rebased(c, t) ==
  [ops     |-> pending[c][t].ops,
   adds    |-> {i \in pending[c][t].adds : i \notin remote[t].items},
   removes |-> {i \in pending[c][t].removes : i \notin remote[t].tombs} \ Superseded(c, t)]

\* Merge the fetched tip, then Reindex from it: the three-way merge treats a
\* key only the index lacks as "removed locally", so the additive Reindex
\* puts back what a Stranded index lost. Always running it is safe because
\* it is additive, and needs no stranded-index guess.
Pull(c) ==
  LET merged == Merge(base[c], local[c], remote)
  IN /\ local' = [local EXCEPT ![c] = Reindexed(merged, remote)]
     /\ base' = [base EXCEPT ![c] = remote]
     /\ pending' = [pending EXCEPT ![c] = [t \in Tickets |-> Rebased(c, t)]]
     /\ History
     /\ UNCHANGED <<remote, upToDate, nextId>>

\* `ti reindex`, runnable at any time: replays the refs already fetched.
Reindex(c) ==
  /\ local' = [local EXCEPT ![c] = Reindexed(local[c], base[c])]
  /\ History
  /\ UNCHANGED <<remote, base, pending, upToDate, nextId>>

\* ADR 0002 push guard, unchanged: never drop a remote ticket's keys.
NoImplicitDeletion(c) == \A t \in Tickets : HasKeys(remote[t]) => HasKeys(local[c][t])

\* git accepts the push only if the remote has not moved since the pull
\* (fast-forward); a clone that lost the race window pulls again.
Push(c) ==
  /\ base[c] = remote
  /\ NoImplicitDeletion(c)
  /\ remote' = local[c]
  /\ base' = [base EXCEPT ![c] = local[c]]
  /\ pending' = [pending EXCEPT ![c] = [t \in Tickets |-> NoPending]]
  /\ upToDate' = [d \in Clones |-> IF d = c THEN TRUE ELSE upToDate[d] /\ remote' = remote]
  /\ UNCHANGED <<local, everDeleted, nextId>>

Next ==
  \E c \in Clones :
    \/ \E t \in Tickets : \/ Delete(c, t) \/ Undelete(c, t) \/ Strand(c, t)
                          \/ \E i \in Items : AddItem(c, t, i) \/ RemoveItem(c, t, i)
    \/ Pull(c)
    \/ Reindex(c)
    \/ Push(c)

Spec == Init /\ [][Next]_vars

(* ---- properties -------------------------------------------------------- *)

\* Nothing a clone did since its last push leaves its index before the push,
\* except a removal superseded by a concurrent add (see Superseded).
NoLostLocalOp ==
  \A c \in Clones, t \in Tickets :
    /\ pending[c][t].ops \subseteq {o.id : o \in local[c][t].ops}
    /\ pending[c][t].adds \subseteq local[c][t].items
    /\ pending[c][t].removes \subseteq local[c][t].tombs

\* Once a clone has seen a ticket deleted, the ticket shows again only
\* through an Undeletion operation.
NoResurrection ==
  \A c \in Clones, t \in Tickets :
    everDeleted[c][t] /\ Visible(local[c][t]) => UndelOps(local[c][t]) # {}

\* When every clone has pushed and nothing moved since, all indexes equal
\* the remote (and so each other).
Convergence ==
  (\A c \in Clones : upToDate[c]) => \A c \in Clones : local[c] = remote

\* An operation on the remote stays on the remote: no push ever drops one,
\* whatever state the pushing index is in.
RemoteKeepsOps == [][\A t \in Tickets : remote[t].ops \subseteq remote'[t].ops]_vars

=============================================================================
