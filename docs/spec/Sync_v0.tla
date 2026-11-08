--------------------------- MODULE Sync_v0 ---------------------------
(***************************************************************************)
(* TicGit Sync protocol as shipped through ti 0.4.2 ("v0").                *)
(*                                                                         *)
(* Deletion is key absence: `ti delete` removes every key of the ticket.   *)
(* Reindex is destructive: the local index is replaced by the remote tree. *)
(* Pull self-heals a "stranded index" by running Reindex whenever a ticket *)
(* in the remote tree has no keys locally -- which is exactly what a local *)
(* deletion looks like.                                                    *)
(*                                                                         *)
(* TLC reproduces incident 215ee3 as a violation of NoLostLocalOp: a       *)
(* comment added locally, a local delete, then a Sync whose pull           *)
(* resurrects the deleted ticket and drops the local-only member.          *)
(* NoResurrection fails too (shorter trace: delete, pull).                 *)
(*                                                                         *)
(* Abstractions (docs/spec/README.md): two clones, one remote; a ticket is *)
(* an `exists` flag plus a set of members (comments, Dependencies,         *)
(* Sub-issues, tags: git-meta sets and lists) with                          *)
(* member tombstones; git-meta three-way merge rules as implemented; a     *)
(* Sync is Pull then Push with a race window between them. No git objects, *)
(* Lamport clocks or hashes.                                               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Clones, Tickets, Items

TicketState == [exists: BOOLEAN, items: SUBSET Items, tombs: SUBSET Items]
Store       == [Tickets -> TicketState]

Blank  == [exists |-> TRUE,  items |-> {}, tombs |-> {}]
Absent == [exists |-> FALSE, items |-> {}, tombs |-> {}]

VARIABLES
  local,    \* local[c]      : clone c's index (git-meta.sqlite)
  remote,   \* the remote tree, refs/{ns}/main
  base,     \* base[c]       : remote tip c last merged or pushed; merge base of its
            \*                 next pull. Push is fast-forward iff base[c] = remote.
  pending,  \* pending[c][t] : members c added on t and has not pushed yet
  deleted,  \* deleted[c]    : tickets c deleted (v0 has no undelete)
  upToDate  \* upToDate[c]   : c pushed and nothing has moved since

vars == <<local, remote, base, pending, deleted, upToDate>>

TypeOK ==
  /\ local \in [Clones -> Store]
  /\ remote \in Store
  /\ base \in [Clones -> Store]
  /\ pending \in [Clones -> [Tickets -> SUBSET Items]]
  /\ deleted \in [Clones -> SUBSET Tickets]
  /\ upToDate \in [Clones -> BOOLEAN]

\* Every ticket exists on the remote and every clone is in sync with it.
Init ==
  /\ remote = [t \in Tickets |-> Blank]
  /\ local = [c \in Clones |-> remote]
  /\ base = [c \in Clones |-> remote]
  /\ pending = [c \in Clones |-> [t \in Tickets |-> {}]]
  /\ deleted = [c \in Clones |-> {}]
  /\ upToDate = [c \in Clones |-> TRUE]

(* ---- git-meta three-way merge (tree/merge.rs), per ticket ------------- *)

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

MergeMembers(b, l, r) ==
  LET m(i) == MergeMember(MState(b, i), MState(l, i), MState(r, i))
  IN [exists |-> TRUE,
      items  |-> {i \in Items : m(i) = "present"},
      tombs  |-> {i \in Items : m(i) = "tomb"}]

\* A ticket removed on one side stays removed unless the other side changed
\* it since the base, in which case the modified side wins (resurrection).
MergeTicket(b, l, r) ==
  CASE l.exists /\ r.exists    -> MergeMembers(b, l, r)
    [] ~l.exists /\ ~r.exists  -> Absent
    [] ~l.exists /\ r.exists   -> IF r # b THEN r ELSE Absent
    [] l.exists /\ ~r.exists   -> IF l # b THEN l ELSE Absent

Merge(b, l, r) == [t \in Tickets |-> MergeTicket(b[t], l[t], r[t])]

(* ---- local edits ------------------------------------------------------ *)

Touch(c) == upToDate' = [upToDate EXCEPT ![c] = FALSE]

\* `ti comment`, `ti tag`, `ti depends`, `ti subissue`: add a member.
AddItem(c, t, i) ==
  /\ local[c][t].exists
  /\ i \notin local[c][t].items
  /\ local' = [local EXCEPT ![c][t].items = @ \cup {i}, ![c][t].tombs = @ \ {i}]
  /\ pending' = [pending EXCEPT ![c][t] = @ \cup {i}]
  /\ Touch(c)
  /\ UNCHANGED <<remote, base, deleted>>

\* Remove a member: git-meta writes a member tombstone.
RemoveItem(c, t, i) ==
  /\ i \in local[c][t].items
  /\ local' = [local EXCEPT ![c][t].items = @ \ {i}, ![c][t].tombs = @ \cup {i}]
  /\ pending' = [pending EXCEPT ![c][t] = @ \ {i}]
  /\ Touch(c)
  /\ UNCHANGED <<remote, base, deleted>>

\* `ti delete` (v0): remove every key of the ticket.
Delete(c, t) ==
  /\ local[c][t].exists
  /\ local' = [local EXCEPT ![c][t] = Absent]
  /\ deleted' = [deleted EXCEPT ![c] = @ \cup {t}]
  /\ pending' = [pending EXCEPT ![c][t] = {}]
  /\ Touch(c)
  /\ UNCHANGED <<remote, base>>

(* ---- sync -------------------------------------------------------------- *)

\* TicketStore::pull's honesty check: a ticket in the fetched remote tree
\* with no keys in the index is taken for a stranded index ...
Stranded(merged) == \E t \in Tickets : remote[t].exists /\ ~merged[t].exists

\* ... and healed by TicketStore::reindex, which drops refs/{ns}/local/main
\* and fast-forward-applies the remote tree over the index: local := remote.
\* The merge result and every local-only member are discarded.
Pull(c) ==
  LET merged == Merge(base[c], local[c], remote)
      healed == IF Stranded(merged) THEN remote ELSE merged
  IN /\ local' = [local EXCEPT ![c] = healed]
     /\ base' = [base EXCEPT ![c] = remote]
     /\ UNCHANGED <<remote, pending, deleted, upToDate>>

\* ADR 0002 push guard: refuse to drop a remote ticket absent locally.
NoImplicitDeletion(c) == \A t \in Tickets : remote[t].exists => local[c][t].exists

\* git accepts the push only if the remote has not moved since the pull
\* (fast-forward); a clone that lost the race window pulls again.
DoPush(c) ==
  /\ base[c] = remote
  /\ remote' = local[c]
  /\ base' = [base EXCEPT ![c] = local[c]]
  /\ pending' = [pending EXCEPT ![c] = [t \in Tickets |-> {}]]
  /\ upToDate' = [d \in Clones |-> IF d = c THEN TRUE ELSE upToDate[d] /\ remote' = remote]
  /\ UNCHANGED <<local, deleted>>

Push(c) == NoImplicitDeletion(c) /\ DoPush(c)

\* `ti push --allow-deletes`.
PushAllowDeletes(c) == DoPush(c)

Next ==
  \E c \in Clones :
    \/ \E t \in Tickets : \/ Delete(c, t)
                          \/ \E i \in Items : AddItem(c, t, i) \/ RemoveItem(c, t, i)
    \/ Pull(c)
    \/ Push(c)
    \/ PushAllowDeletes(c)

Spec == Init /\ [][Next]_vars

(* ---- properties ------------------------------------------------------- *)

\* A member added locally and not yet pushed is still in the local index.
NoLostLocalOp ==
  \A c \in Clones, t \in Tickets : pending[c][t] \subseteq local[c][t].items

\* A ticket a clone deleted never reappears in that clone.
NoResurrection ==
  \A c \in Clones : \A t \in deleted[c] : ~local[c][t].exists

\* When every clone has pushed and nothing moved since, all indexes equal the remote.
Convergence ==
  (\A c \in Clones : upToDate[c]) => \A c \in Clones : local[c] = remote

=============================================================================
