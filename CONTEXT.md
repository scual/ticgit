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
