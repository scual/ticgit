# TicGit

Git-native ticket tracking: tickets, their relationships, and their lifecycle live inside the repository.

## Language

### Ticket relationships

**Dependency**:
A directed relation between two tickets where one cannot proceed until the other is closed, in any State. Read in one of two directions: "A depends on B" and "B blocks A" state the same Dependency.
_Avoid_: Link, relation (too vague)

**Depends on**:
The downstream reading of a Dependency: the ticket that waits. "A depends on B" means A waits for B.
_Avoid_: Blocked by, needs, requires

**Blocks**:
The upstream reading of a Dependency: the ticket being waited for. "B blocks A" means A waits for B.
_Avoid_: Blocker of, prerequisite of, depended on by

**Sub-issue**:
A ticket that is part of another ticket's work. It has exactly one Parent and may itself have Sub-issues, to any depth. A Parent is closed only after its Sub-issues are.
_Avoid_: Child, subtask

**Parent**:
The ticket that owns a Sub-issue. Work is offered at the Parent level; its open Sub-issues are shown beneath it.
_Avoid_: Epic, container

### Ticket lifecycle

**Status**:
The broad lifecycle position of a ticket: open or closed.

**Closed**:
The Status of a ticket that is finished, however it ended: resolved, wontfix, duplicate, or invalid. A Dependency is satisfied once its target is Closed.
_Avoid_: Resolved or solved for this meaning (Resolved is one specific State)

**State**:
The specific lifecycle position of a ticket within its Status.
_Avoid_: Stage, phase

**Blocked**:
An open State set by hand, meaning work is stalled for any reason, including ones outside the tracker. Unrelated to Dependency: a Dependency never sets it, and a ticket with an unfinished Dependency need not be Blocked.
_Avoid_: Using "blocked" for a ticket that merely has an unfinished Dependency

### Writeups

**Writeup**:
A versioned markdown draft that precedes work. It is either promoted into a Ticket or closed without one.
_Avoid_: Proposal, RFC, spec (a spec belongs to a Ticket, not a Writeup)

**Promote**:
Turn a Writeup into a Ticket, carrying over its title, tags, priority, and latest body. The Writeup is then closed and stays linked to the Ticket it became.
_Avoid_: Convert, publish

### Working context

**Checked-out ticket**:
The one ticket a person is currently focused on, so commands can act on it when no ticket is named. It is personal to each user and each clone, and is never shared through the repository. It may be a Closed ticket.
_Avoid_: Current ticket, active ticket, selected ticket

### Prioritising work

**Priority**:
An optional integer on a ticket where lower is more important. Unprioritised (`none`) is the least-important band: a numeric priority — even a large one — always ranks above unprioritised tickets, so a number can never sink a ticket below the unprioritised pile. To push work down, leave it unprioritised, mark it Deferred, or set it Blocked.
_Avoid_: Treating a high number as a demotion below unprioritised tickets

**Deferred**:
Work parked out of the active queue, marked by the `deferred` or `backlog` tag. `ti next` hides Deferred tickets by default; `ti next --include-deferred` brings them back. This is the non-abusive way to shelve work, distinct from Blocked (stalled) and from a low Priority.
_Avoid_: Using Blocked to mean deferred, or a large Priority number to shelve work

**Next queue**:
The ordering `ti next` uses to pick one ticket to work on. It excludes Closed tickets, Sub-issues, tickets with an unfinished Dependency, and Deferred tickets, then orders the rest by Priority (unprioritised last), then State (Blocked last), then oldest-created first.

### Storage and integrity

**Operation log**:
The append-only record of operations that produce a ticket's scalar fields (title, state, priority, …). It is the conflict-free source of truth: clones merge by unioning operations and replaying them in order, so concurrent edits to different fields both survive rather than one clobbering the other.
_Avoid_: Mutable field, last-write-wins, overwrite

**Operation (op)**:
One immutable change in the log — create, set a field, or clear a field — with a content-derived id (SHA-256), a Lamport clock, and an optional SSH signature. Older clients skip operation kinds they don't understand.
_Avoid_: Edit record, patch, event (reserve "event" for unrelated uses)

**Signing**:
Attaching an SSH signature (git-signing style, via `ssh-keygen -Y`) to an operation so authorship is verifiable across clones. Unsigned operations are trusted by default and flagged by `ti verify`.
_Avoid_: GPG (TicGit signs with SSH keys), authentication

**Verify**:
`ti verify` — the consistency oracle. Replays each ticket's log, recomputes operation ids, checks signatures, and confirms the log projects to a valid ticket. A failure is a hard error (non-zero exit); an unsigned op is a warning.
_Avoid_: Validate, lint

**Migrate**:
`ti migrate` — rolls tickets forward to the current on-disk format version (a per-ticket `format-version`). Dry-run by default; `--write` applies and is idempotent. A ticket whose format is newer than the running `ti` is refused rather than misread.
_Avoid_: Upgrade (reserve for the `ti` binary), convert
