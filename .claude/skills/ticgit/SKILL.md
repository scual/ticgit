---
name: ticgit
description: Use for durable task/ticket tracking in this repo — planning, progress notes, triage, resolving work. Tickets live in git as metadata via the `ti` CLI (TicGit). Trigger whenever creating, listing, updating, closing, or deleting tickets/tasks/issues; managing sub-issues, dependencies, milestones, priorities, tags, points, specs, writeups, saved views, code reviews, or stats; importing issues from GitHub/Linear; syncing ticket metadata; or when the user says "ticket", "TicGit", "ti", "track this", "backlog", or "next ticket". For the full command catalog with every flag, read reference.md.
---

# TicGit (`ti`) — git-native tickets for this repo

Durable tasks live in **git metadata** via the `ti` CLI (not a `TASKS.md`, not
GitHub/GitLab issues). The CLI is the source of truth. This repo is already
initialised (`ti init`; `origin` carries ticket metadata under `refs/meta/*`).

This file covers the day-to-day essentials. **For any command not shown here, or
for every flag of a command, read [`reference.md`](reference.md)** — it catalogs
all 30+ subcommands. For the always-current upstream guide run `ti agent`.

## Core conventions (apply to almost every command)

- **`-t, --ticket <id>`** targets a ticket by id or unique prefix. Omit it to act
  on the currently checked-out ticket (`ti checkout <id>` sets that). Short prefixes
  are fine as long as they're unique. The sentinel **`@`** means the checked-out
  ticket anywhere an id is accepted (e.g. `ti show @`, `ti list --blocks @`) —
  handy for the required flag values that can't otherwise default.
- **Read with `--markdown`** — richer than the default and it suggests useful
  next commands. `--json` is available on almost everything for scripting;
  `ti show --filter .field` extracts one field with a small jq-like path.
- **Multi-line bodies go through a file, not flags.** For `ti new`/`ti edit`,
  `-F <file>` reads title from line 1, a blank line, then the description. Same
  pattern for `ti spec -F`, `ti meta -F`, `ti comment` (opens `$EDITOR` when no body).

## Read

```sh
ti list --markdown           # open tickets (default filter: status open)
ti list --all --markdown     # everything, no open-only filter or limit
ti next --markdown           # pick + check out the best next ticket (skips blocked-by-deps)
ti show <id> --markdown      # one ticket + comments
ti mine --markdown           # assigned to me (git config user.email)
ti recent --markdown         # most recently touched
ti history -t <id>           # change history for a ticket
ti stats --markdown          # dashboard
```

Filter `list`/`mine` with `--tag`, `--state`, `--status`, `--search title:foo`,
`--order priority`, `--subissues`, `--depends-on <id>`, `--blocks <id>`. See reference.md.

`ti next` and `ti show` render the chosen ticket's **open sub-issues as a recursive
tree** (any depth; closed pruned). In `--json` this is an additive `subissues` array
of `{id, title, state, subissues:[…]}` nodes — the rest of the ticket object is unchanged.

## Create / edit

```sh
ti new -F /tmp/ticket.md --tags bug,parser --markdown   # line 1 = title, rest = description
ti new --title "fix parser" --tags bug --priority 2 --assigned me
ti new --title "child task" --subissue <parent-id>      # create as a sub-issue
ti edit <id>                 # opens $EDITOR; or -F /tmp/edit.md
```

## Work on a ticket

```sh
ti checkout <id>             # set "current"; later commands can omit -t
ti claim                     # assign to me + mark assigned
ti comment -t <id> "found the failing case"   # progress / blockers / verification
ti state in-progress -t <id> # lifecycle: status, state, or status:state
ti state blocked -t <id>
ti close <id>                # resolve (id positional or -t); records closed_by
ti delete <id> [<id>...] --yes          # permanently delete (positional ids); irreversible
ti delete <parent> --recursive --yes    # also delete the whole sub-issue subtree
```
`ti delete` requires explicit id(s) (no default-to-current) and prompts unless
`--yes`; `--json`/`--markdown` and non-interactive shells require `--yes`. Deleting
a parent **orphans** its sub-issues (kept top-level) unless `--recursive`. No undo.
Lifecycle — **open:** `new assigned in-progress blocked review` · **closed:**
`resolved wontfix duplicate invalid`.

## Planning fields

```sh
ti assign <email> -t <id>    # or --clear to unassign
ti priority 2 -t <id>        # lower = more important; --clear to remove
ti points 3 -t <id>          # estimate; --clear
ti milestone v1.0 -t <id>    # --clear
ti tag bug parser -t <id>    # add; -d/--remove <tag> to remove
ti subissue <parent> -t <id> # nest under parent; -c/--clear to detach
ti depends <blocker> -t <id> # this ticket depends on <blocker>; --remove / --clear
ti deps <id>                 # transitive dependency tree; --dependents / --both / --all
ti spec -F /tmp/spec.md -t <id>   # implementation notes; read back: ti show <id> --filter .spec
ti code https://host/path:branch -t <id>   # link code URI; --clear
ti meta <field> <value> -t <id>            # arbitrary metadata field
```
`ti dep` is an alias for `ti depends`. `ti next` skips tickets with unresolved deps.

## Sync (see workflow rule below)

```sh
ti sync                      # pull then push ticket metadata to origin
ti push                      # push only (seed a fresh remote)
ti pull <url|nickname>       # pull tickets from a fork / remote URL
```

## Beyond the basics — see reference.md

`ti views` (saved filters), `ti writeup` (rough notes → tickets),
`ti review` (branch code-review metadata), `ti import gh|linear`,
`ti users` (nick↔email mailmap), `ti tui`, `ti init`/`setup`/`update`/`agent`.

## Standing workflow rules (ALWAYS)

- **Sync with every remote code sync.** Any `git fetch`/`pull`/`push` → also run
  `ti sync` so ticket metadata (it rides `origin` under `refs/meta/*`) stays in
  lockstep with code. Fresh remote: `ti push` once to seed, then `ti sync`.
- **Reference the ticket in every commit.** Add a `Ticket: <short-id>` trailer
  (e.g. `Ticket: 21a835`) in the trailer block alongside `Changelog:` /
  `Claude-Session:`. Commit via `git commit -F <file>` so trailers parse. No
  ticket yet → `ti new` first.

## Practices

- Prefer `--markdown` when reading; use short unique id prefixes.
- Comment when you learn something or finish a step; **resolve only after verify**.
- Add a `ti spec` before implementing when the path is unclear.
- Link a branch review with `ti review new --ticket <id>`; refresh with
  `ti review update` after adding commits.
- Mark blockers with `ti state blocked` and ordering with `ti depends`.
