# Triage labels: canonical states

The `triage` skill uses five canonical states to label incoming issues. Map them to your issue tracker's states or labels below.

| Canonical state | TicGit mapping | Meaning |
|---|---|---|
| `needs-triage` | (issue exists, comment from maintainer pending) | Maintainer needs to evaluate the issue |
| `needs-info` | (issue exists, awaiting reporter response) | Waiting on the reporter for more context |
| `ready-for-agent` | `open` + fully specified in description/comments | Fully specified; an agent can pick it up with no human context |
| `ready-for-human` | `open` + assigned or claimed | Needs human implementation |
| `wontfix` | `closed` + marked as declined/out-of-scope | Will not be actioned |

## Usage

When the `triage` skill processes an incoming issue, it:
1. Evaluates it (reads description, labels, comments)
2. Assigns a canonical state above
3. Records the state in the TicGit ticket (via `ti comment`, `ti state`, or other TicGit workflows)

If your triage workflow differs from the canonical pipeline, adjust the mapping above and document how issues flow through your states.
