---
name: ticgit
description: Use TicGit (`ti`) to track Git-native tickets in this repository.
---

# TicGit Agent Guide

TicGit stores tickets as Git metadata. Use `ti` for planning, progress notes,
triage, and resolving work. Prefer commands with `--markdown` when reading
ticket data because Markdown output includes useful context and next commands;
every read and mutation also supports `--json` for machine parsing. Ids may be a
full UUID or any unique prefix.

## Basic Workflow

Find work:

```sh
ti list --markdown
ti next --markdown
ti show <id> --markdown
```

Select a current ticket when you will run several commands against it:

```sh
ti checkout <id>
ti show --markdown
ti comment "progress update"
```

Claim work before starting:

```sh
ti claim
```

## Finding And Browsing

`ti list` filters and sorts the backlog; `ti mine` is the same list scoped to
the tickets assigned to you. Both default to open tickets only:

```sh
ti list --tag bug --order priority --markdown
ti list --search "parser" --markdown
ti list --search "title:timeout" --markdown
ti list --status closed --markdown
ti list --all --markdown
ti mine --markdown
```

Filter by relationship:

```sh
ti list --parent <id>        # direct sub-issues of a ticket
ti list --depends-on <id>    # tickets that depend on this one
ti list --blocks <id>        # tickets that block this one
ti list --subissues          # include sub-issues (hidden by default)
```

Review recent activity and one ticket's change history:

```sh
ti recent --markdown
ti history -t <id> --markdown
```

`ti tui` opens an interactive terminal browser; it is for humans, not scripting.

## Create And Edit Tickets

Create a ticket from a file. The first line is the title; the rest is the
description:

```sh
ti new -F /tmp/ticket.md --tags bug,parser --markdown
```

Set relationships, assignee, or checkout at creation. `--id-only` prints just
the new id, which is convenient for scripting:

```sh
ti new -t "fix timeout" --subissue <parent> --depends-on <blocker> --checkout
ti new -t "quick task" --id-only
```

Edit title and description:

```sh
ti edit <id>
ti edit <id> -F /tmp/ticket.md
```

## Progress Notes

Add comments for useful observations, plans, blockers, and verification:

```sh
ti comment -t <id> "found the failing case"
ti comment "implemented fix; running cargo test -p ticgit"
ti comment -t <id> --edit
```

## State And Triage

Tickets have a broad status and a specific state:

```text
open: new, assigned, in-progress, blocked, review
closed: resolved, wontfix, duplicate, invalid
```

Useful updates:

```sh
ti state blocked -t <id>
ti state review -t <id>
ti close -t <id>
```

`ti close` resolves the ticket and records the current user as `closed_by`.

## Planning Fields

Use priority, tags, estimates, milestones, and assignee to keep work easy to
sort:

```sh
ti priority -t <id> 2
ti tag -t <id> bug parser
ti points -t <id> 3
ti milestone -t <id> v1.0
ti assign -t <id> alice@example.com
```

Record the associated code location, or set a custom metadata field:

```sh
ti code -t <id> https://github.com/org/repo:feature-branch
ti meta -t <id> external-id PROJ-123
```

Use `spec` for implementation notes before coding:

```sh
ti spec -t <id> -F /tmp/spec.md
ti show <id> --filter .spec
```

## Dependencies

Track ordering constraints explicitly. "A depends on B" and "B blocks A" are the
same directed relation; the depended-on ticket is the blocker:

```sh
ti depends <blocker-id> -t <id>
ti depends <blocker-id> -t <id> --remove
```

`ti next` skips tickets with open dependencies.

## Sub-Issues

Nest a ticket under a parent to break large work into a hierarchy:

```sh
ti subissue <parent-id> -t <id>
ti subissue -t <id> --clear
```

`ti next` and `ti show --markdown` render a parent's open sub-issues as a
recursive tree, so reading the parent shows the whole remaining subtree.

## Deleting

`ti delete` removes tickets permanently. It takes explicit ids (it never falls
back to the checked-out ticket) and prompts for confirmation unless `--yes` is
passed. Deleting a parent orphans its sub-issues; pass `--recursive` to remove
the whole subtree instead:

```sh
ti delete <id> --yes
ti delete <parent-id> --recursive --yes
```

Prefer closing (`ti close`) over deleting; delete only to discard tickets that
should leave no history.

## Writeups

Capture rough notes before they are ticket-shaped, then promote the good ones
into tickets:

```sh
ti writeup new -F /tmp/notes.md
ti writeup list
ti writeup show <writeup-id>
ti writeup promote <writeup-id>
ti writeup link <writeup-id> <ticket-id>
```

## Saved Views

Save common filters and recall them by name:

```sh
ti list --tag bug
ti views save bugs
ti list bugs --markdown
```

## Importing

Bring in issues from external trackers:

```sh
ti import gh --repo owner/repo
ti import linear
```

## Stats

```sh
ti stats --markdown
```

## Sync And Setup

Share ticket metadata with collaborators:

```sh
ti sync                       # pull then push against the meta remote
ti push                       # push only
ti pull <url-or-nickname>     # pull from a fork or remote URL
```

First-time setup on a repo (both idempotent):

```sh
ti init                       # initialise ticgit metadata
ti setup                      # configure the meta remote from a .git-meta file
```

Map contributor nicks to emails so assignees resolve consistently:

```sh
ti users add scott scott@example.com
```

## Code Reviews

Open a review when a branch is ready for review. Link it to the ticket so the
review shows up with that work:

```sh
ti review new --branch <branch-name> --ticket <id>
ti review show <branch-name>
```

After adding commits to the branch, update the review metadata so the current
head and revision list are correct:

```sh
ti review update <branch-name>
```

Use the current branch by omitting the branch argument:

```sh
ti review new --ticket <id>
ti review update
```

## Agent Practices

- Use ticket IDs or unique prefixes.
- Prefer `--markdown` for reading tickets and `--json` when parsing output.
- Check for a spec before implementing; add one if the path is unclear.
- Comment when you learn something important or finish a meaningful step.
- Open or update reviews with `ti review new` and `ti review update` when branch commits change.
- Mark blockers with `ti state blocked` and dependencies with `ti depends`.
- Break large tickets into sub-issues with `ti subissue`.
- Resolve tickets only after implementation and verification are complete.
- Close tickets with `ti close`; reach for `ti delete` only to discard a ticket
  that should leave no trace, and use `--recursive` so sub-issues are not orphaned.
