# Domain docs: single-context layout

This repo uses a **single-context** domain doc layout. All domain documentation lives at the repo root.

## Files

- **`CONTEXT.md`** (at repo root) — Domain language, key concepts, and system overview. Skills like `diagnosing-bugs`, `tdd`, and `improve-codebase-architecture` read this to understand the project's domain.

- **`docs/adr/`** (at repo root) — Architecture Decision Records. One file per significant decision (e.g., `docs/adr/001-choice-of-language.md`). Skills read these to understand past trade-offs and avoid reopening decided questions.

## Consumer rules

Skills that read domain docs follow these rules:

1. **Required reading**: Before tackling a bug, architecture question, or test strategy, the skill reads `CONTEXT.md` to learn domain terms and system invariants.

2. **Optional reading**: If `docs/adr/` exists, the skill may scan it to understand prior decisions (e.g., "why was X chosen over Y?").

3. **Writing**: Skills do not write to these files. Domain knowledge is maintained by the team.

## Getting started

If `CONTEXT.md` does not exist yet, create it. Include:

- **System overview** — what does the system do? Who uses it?
- **Key concepts** — domain terminology (e.g., "a Ticket is a unit of work tracked in TicGit")
- **Invariants** — rules that must hold (e.g., "every Ticket must have a state")
- **Architecture sketch** — high-level components and their relationships

Example:

```markdown
# CONTEXT.md

## System overview

TicGit is a Git-native ticket tracking system. Users create, comment on, and close tickets stored in `.ticgit/`.

## Key concepts

- **Ticket** — a unit of work (bug, feature, task). Each ticket has an ID, title, description, state, and comments.
- **State** — one of `open`, `in-progress`, `closed`.
- **Comment** — user feedback on a ticket.

## Invariants

- Every ticket must have a unique ID.
- State transitions are: `open` → `in-progress` → `closed` (or directly to `closed`).
- Closed tickets are immutable (no state change, only read).

## Architecture sketch

- CLI (`ti`) — user interface for ticket operations
- Git storage (`.ticgit/`) — persists tickets as git objects
- State machine — enforces valid state transitions
```
