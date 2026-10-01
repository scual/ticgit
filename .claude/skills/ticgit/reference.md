# TicGit (`ti`) — full command reference

Complete catalog of every `ti` subcommand and its flags (verified against
`ti 0.3.x`). Read this when SKILL.md's essentials don't cover what you need, or
when you need a specific flag. Grouped as the CLI groups them.

Two conventions repeat almost everywhere, so they're stated once here:
- **`-t, --ticket <id>`** — target a ticket by id or unique prefix; omit to use
  the checked-out ticket. (A few commands take the id **positionally** instead —
  noted below where they differ.)
- **`--json` / `--markdown`** — output format; available on nearly every command.
  `--markdown` is preferred for reading (richer, suggests next commands).

## Table of contents

- [Create & browse](#create--browse) — `new` `list` `show` `recent` `mine` `history` `tui`
- [Work on tickets](#work-on-tickets) — `checkout` `next` `edit` `comment` `state` `close` `claim` `delete`
- [Ticket fields](#ticket-fields) — `tag` `assign` `priority` `points` `milestone` `subissue` `code` `depends`/`dep` `spec` `meta`
- [Views & import](#views--import) — `views` `writeup` `review` `stats` `import`
- [Team](#team) — `users` `mine`
- [Sync & setup](#sync--setup) — `sync` `pull` `push` `init` `setup` `update`
- [Agents](#agents) — `agent`

---

## Create & browse

### `ti new` — create a ticket
```
-t, --title <TITLE>        Title. If omitted, $EDITOR opens to write one.
-F, --file <FILE>          Read title (line 1) + description (rest) from a file.
-g, --tags <TAGS>          Comma/space-separated tags to apply on creation.
-a, --assigned <ASSIGNED>  Initial assignee.
    --priority <PRIORITY>  Initial priority (lower = more important).
-p, --subissue <SUBISSUE>  Create as a sub-issue of this ticket (id/prefix).
    --comment <COMMENT>    Initial comment body (--comment-edit to compose in $EDITOR).
    --comment-edit         Open $EDITOR for the initial comment.
-c, --checkout             Check out the new ticket after creating it.
    --id-only              Print only the new id.
    --json / --markdown    Output the created ticket.
```

### `ti list` (alias `ls`) — list tickets with filters
```
[VIEW]                     Load a saved view by name (see `ti views`).
-s, --state <STATE>        Filter by status/state. Default: status open.
    --status <STATUS>      Filter by broad status.
    --all                  All tickets — no open-only filter or limit.
    --open                 All open tickets, no terminal-height truncation.
-g, --tag <TAG>            Filter by tag (repeatable).
    --tag-mode <all|any>   How multiple --tag combine. Default: all.
-a, --assigned <ASSIGNED>  Filter by assignee.
-T, --only-tagged          Only tickets that have at least one tag.
    --search <SEARCH>      Search title/description/comments. Scope with
                           `title:term`, `description:term`, `comments:term`.
-o, --order <ORDER>        Sort: priority, state, title.desc, created, assigned, …
    --subissues            Include sub-issues (hidden by default).
    --depends-on <ID>      Tickets that depend on this ticket.
    --blocks <ID>          Tickets that block this ticket.
-n, --limit <LIMIT>        Max tickets. Default: terminal rows (0).
    --json / --markdown
```

### `ti show` — one ticket + comments
```
[TICKET]                   Id/prefix. Defaults to the checked-out ticket.
    --json / --markdown
    --filter [<FILTER>]    Output one JSON field via a small jq-like path,
                           e.g. `.title`, `.spec`, `.parent`, `.children`,
                           `.comments[0].body`.
```
Like `ti next`, the output includes the ticket's **open sub-issues as a recursive
tree** (closed pruned); `--json` adds the same additive `subissues` array
alongside the flat `children` id list (which is unchanged). `--filter` reads from
the plain stored fields (it does not expose `subissues`).

### `ti recent` — most recently touched
```
-n, --limit <LIMIT>        Number to show. Default 10.
    --json / --markdown
```

### `ti mine` — tickets assigned to you
Same filter flags as `ti list` (`--state`, `--status`, `--all`, `--open`,
`--tag`, `--tag-mode`, `--assigned`, `--only-tagged`, `--search`, `--order`,
`--subissues`, `--depends-on`, `--blocks`, `--limit`, `--json`, `--markdown`).
Uses `git config user.email` to decide who "you" is. Takes an optional `[VIEW]`.

### `ti history` — change history for a ticket
```
-t, --ticket <TICKET>      Id/prefix. Defaults to checked-out.
-n, --limit <LIMIT>        Max entries.
    --json / --markdown
```

### `ti tui` — interactive terminal UI
No options. Browses open tickets interactively. (Not for non-interactive use.)

---

## Work on tickets

### `ti checkout` (alias `co`) — set the "current" ticket
```
[TICKET]                   Id/prefix to mark current.
-c, --clear                Clear the checked-out ticket.
    --json / --markdown
```

### `ti next` — pick + check out the best next ticket
```
-g, --tag <TAG>            Only consider tickets with this tag.
-a, --assigned <ASSIGNED>  Only consider tickets assigned to this user.
    --json / --markdown
```
Skips tickets with unresolved dependencies. The output shows the chosen ticket's
**open sub-issues as a recursive tree** (any depth; closed sub-issues pruned). In
`--json` this is an additive `subissues` array on the ticket object — each node is
`{id, title, state, subissues:[…]}` — so the rest of the ticket object is unchanged.

### `ti edit` — edit title + description
```
[TICKET]                   Id/prefix. Defaults to checked-out.
-F, --file <FILE>          Read updated title + description from a file.
    --json / --markdown
```
With no `-F`, opens `$EDITOR`.

### `ti comment` — add a comment
```
[BODY]...                  Comment text. If omitted, $EDITOR opens.
-t, --ticket <TICKET>      Id/prefix. Defaults to checked-out.
-e, --edit                 Force $EDITOR, ignoring positional body.
    --json / --markdown
```

### `ti state` — change lifecycle status/state
```
[LIFECYCLE]                New value: `status`, `state`, or `status:state`.
                           Omit to choose interactively (needs a terminal).
-t, --ticket <TICKET>      Id/prefix. Defaults to checked-out.
    --json / --markdown
```
open: `new assigned in-progress blocked review` · closed: `resolved wontfix duplicate invalid`.

### `ti close` — resolve a ticket
```
[TICKET]                   Id/prefix (POSITIONAL, not -t). Defaults to checked-out.
    --json / --markdown
```
Shorthand for `ti state resolved`; records the current user as `closed_by`.

### `ti claim` — assign to you + mark assigned
```
-t, --ticket <TICKET>      Id/prefix. Defaults to checked-out.
    --json / --markdown
```

### `ti delete` — permanently delete ticket(s)
```
[IDS]...                   One or more ids/prefixes (REQUIRED, POSITIONAL).
                           No default-to-checked-out — a destructive op must be explicit.
-r, --recursive            Also delete the whole descendant subtree (incl. closed).
-y, --yes                  Skip the confirmation prompt.
    --json / --markdown    Output the deleted ticket(s). --json emits an ARRAY of
                           ticket objects (even for one id). Both require --yes.
```
**Irreversible.** Resolves every id first — if any is unknown/ambiguous the whole
batch aborts and nothing is deleted. Without `--recursive`, a deleted parent's
sub-issues are **orphaned** (kept as top-level tickets), not deleted; `--recursive`
removes the entire subtree. Prompts `[y/N]` unless `--yes`; machine mode
(`--json`/`--markdown`) and non-interactive shells **require `--yes`** (they error
instead of prompting). Clears the checked-out pointer if you delete the current ticket.

---

## Ticket fields

All of these accept `-t, --ticket <id>` (default: checked-out) and
`--json`/`--markdown`. Most take a value positionally, with `-c/--clear` to remove.

### `ti tag` — add/remove tags
```
[TAGS]...                  Tag(s) to add. Comma/space-separated.
-t, --ticket <TICKET>
-w, --writeup <WRITEUP>    Tag a writeup instead of a ticket.
-d, --remove <REMOVE>      Remove the given tag(s) instead of adding.
    --json / --markdown
```

### `ti assign` — set/clear assignee
```
[USER]                     User to assign (usually an email).
-t, --ticket <TICKET>
-c, --clear                Unassign.
```

### `ti priority` — set/clear priority (lower = more important)
```
[PRIORITY]                 Value. Omit with --clear to remove.
-t, --ticket <TICKET>
-c, --clear
```

### `ti points` — set/clear points estimate
```
[POINTS]                   Estimate. Omit with --clear to remove.
-t, --ticket <TICKET>
-c, --clear
```

### `ti milestone` — set/clear milestone
```
[MILESTONE]                Name. Omit with --clear to remove.
-t, --ticket <TICKET>
-c, --clear
```

### `ti subissue` — nest under a parent ticket
```
[PARENT]                   Parent id/prefix. Omit with --clear to detach.
-t, --ticket <TICKET>      The ticket to make a sub-issue.
-c, --clear                Remove the sub-issue relationship.
```

### `ti code` — set/clear an associated code URI
```
[CODE]                     URI as https://<host>/<path>:<branch>.
-t, --ticket <TICKET>
-c, --clear
```

### `ti depends` (alias `dep`) — dependency between tickets
```
[DEPENDENCY]               The blocker: current ticket depends on this ticket.
-t, --ticket <TICKET>      Ticket to modify (default: checked-out).
    --remove               Remove this dependency.
    --clear                Clear ALL dependencies from the ticket.
    --json / --markdown
```

### `ti spec` — implementation spec
```
[SPEC]                     Spec text. Omit to open $EDITOR, or --clear to remove.
-t, --ticket <TICKET>
-F, --file <FILE>          Read spec from a file.
-c, --clear
    --json / --markdown
```
Read it back with `ti show <id> --filter .spec`.

### `ti meta` — arbitrary string metadata field
```
<FIELD>                    Metadata field name (required).
[VALUE]                    Value (omit when using --file).
-t, --ticket <TICKET>
-F, --file <FILE>          Read the value from a file.
    --json / --markdown
```

---

## Views & import

### `ti views` — saved list filters
```
ti views save <NAME>       Save the last-used list filters as a named view.
ti views delete <NAME>     Delete a saved view.
```
Load with `ti list <NAME>` or `ti mine <NAME>`.

### `ti writeup` — rough notes promoted to tickets
```
ti writeup new [-t TITLE] [--body B | -F FILE] [-g TAGS] [--id-only]
ti writeup list [--all]                    List writeups (--all includes closed).
ti writeup show <ID> [--all]               Show latest (or every) version.
ti writeup edit <ID> [--body B | -F FILE]  Append a new version.
ti writeup promote <ID>                    Promote a writeup into a ticket.
ti writeup close <ID>                      Close a writeup.
ti writeup archive <ID>                    Alias for close.
ti writeup link <WRITEUP> <TICKET>         Link a writeup to a ticket.
ti writeup unlink <WRITEUP> <TICKET>       Remove a writeup↔ticket link.
```

### `ti review` — local code-review metadata for a branch
```
ti review new [--branch B] [--base REF] [--ticket ID] [--title T]
              [--description D] [--reviewer EMAIL]...   Create review metadata.
ti review list                                          List known reviews.
ti review show [REVIEW] [--json|--markdown]             Show metadata + messages.
ti review add-reviewer <email> | <branch> <email>      Request a reviewer.
ti review comment [--path P] [--line N | --lines A-B] [--commit SHA]
              <body...> | <branch> <body...>            Add a review comment.
ti review approve [TARGET] [--comment C]                Approve a review/commit.
ti review request-changes <body...> | <branch> <body...>
ti review update [REVIEW] [--head SHA]                  Refresh head + revision list.
ti review integrate <sha> | <branch> <sha>              Record an integration/merge.
```
`[REVIEW]`/`[branch]` default to the current branch; `--base` defaults to the
first main/master ref. Link to work with `--ticket`. Refresh with `ti review
update` after adding commits so the head + revision list stay correct.

### `ti stats` — ticket stats dashboard
```
    --json / --markdown
```

### `ti import` — import from external systems
```
ti import gh [-R OWNER/REPO] [--limit N] [--json|--markdown]
                           Import open issues via the GitHub CLI (`gh`). Default limit 1000.
ti import linear [-t TEAM] [--limit N] [--json|--markdown]
                           Import from Linear (GraphQL API). Omit -t to list teams. Default 1000.
```

---

## Team

### `ti users` — nick ↔ email mailmap (shared)
```
ti users add <NICK> <EMAIL>        Associate an email with a user nick.
ti users rm  <NICK> [EMAIL]        Remove a user (or just one email from it).
    --json / --markdown
```

### `ti mine`
Listed under [Create & browse](#create--browse) above (it's both a browse and a
team command). Lists tickets assigned to you.

---

## Sync & setup

Ticket metadata rides the git remote under `refs/meta/*`. Keep it in lockstep
with code: any `git fetch`/`pull`/`push` should be paired with a `ti sync`.

### `ti sync` — pull then push ticket metadata
```
-r, --remote <REMOTE>      Remote to sync. Default: git-meta's first meta remote.
```

### `ti pull` — pull tickets from a fork / URL
```
<SOURCE>                   URL or saved nickname to pull from.
[NICKNAME]                 Save the URL under this nickname for reuse.
    --json / --markdown
```

### `ti push` — push ticket metadata
```
-r, --remote <REMOTE>      Remote to push to. Default: git-meta's first meta remote.
```
Use once to seed a fresh remote, then `ti sync` afterward.

### `ti init` — initialise ticgit metadata (idempotent)
No options. Sets up ticgit on the current repo.

### `ti setup` — configure git-meta remote from `.git-meta` (idempotent)
No options.

### `ti update` — update `ti` to the latest release
```
    --check                Check for updates without installing.
```

---

## Agents

### `ti agent` — AI-agent integration guidance
```
ti agent                   Print the always-current Markdown agent guide.
ti agent skill [--target T] [--check]
                           Install TicGit instructions for an AI agent.
                           --check verifies whether the target is installed + current.
```
Run `ti agent` any time to get the upstream guide straight from the installed
CLI version — useful when a new `ti` release adds subcommands not listed here.

---

## Notes & gotchas

- **`ti close` takes the id positionally**, unlike most field/state commands
  which use `-t`. `ti close <id>`, not `ti close -t <id>`.
- **`ti delete` is irreversible and non-interactive-safe only with `--yes`.**
  It takes ids positionally (like `close`), accepts several at once, and refuses
  to run in `--json`/`--markdown` or a non-TTY without `--yes`. Deleting a parent
  orphans its sub-issues unless you pass `--recursive`. There is no undo.
- **`ti dep`** is an exact alias of **`ti depends`** — same flags.
- **Dependency direction:** `ti depends <blocker> -t <id>` means *`<id>` depends
  on `<blocker>`* (i.e. `<blocker>` must be resolved first). `ti next` and
  `list --blocks/--depends-on` respect this.
- **Multi-line content** (descriptions, specs, meta values, comments) is best
  passed via `-F <file>` (or `$EDITOR`), not shell-quoted flags — this repo's
  git-commit heredoc/quoting caveats apply to any multi-line CLI input.
- **`--filter`** on `ti show` is the cheap way to read one field
  programmatically (`.title`, `.state`, `.spec`, `.assigned`, `.comments[0].body`).
