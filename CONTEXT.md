# TicGit

Git-native ticket tracking: tickets, their relationships, and their lifecycle live inside the repository.

## Language

### Ticket relationships

**Dependency**:
A directed relation between two tickets where one cannot proceed until the other is done. Read in one of two directions: "A depends on B" and "B blocks A" state the same Dependency.
_Avoid_: Link, relation (too vague)

**Depends on**:
The downstream reading of a Dependency: the ticket that waits. "A depends on B" means A waits for B.
_Avoid_: Blocked by, needs, requires

**Blocks**:
The upstream reading of a Dependency: the ticket being waited for. "B blocks A" means A waits for B.
_Avoid_: Blocker of, prerequisite of, depended on by
