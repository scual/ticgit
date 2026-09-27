# Issue tracker: TicGit

Issues are tracked in **TicGit**, a git-native ticket system. Tickets live in the `.ticgit/` folder (committed to the repo) and are managed via the `ti` CLI.

## Reading issues

```bash
ti list --markdown          # List all open tickets
ti show <id> --markdown     # Show a single ticket
ti list --state closed      # List closed tickets
```

## Modifying issues

```bash
ti comment <id> "message"   # Add a comment
ti state <id> <state>       # Change ticket state
ti close <id> "resolution"  # Close a ticket
```

## States

TicGit uses custom states. Tickets flow through:
- `open` — ticket exists, initial state
- `in-progress` — being worked on
- `closed` — resolved

The `triage` skill maps these to its canonical states (see `docs/agents/triage-labels.md`).

## External PRs

TicGit is git-native and does not have an external PR surface. Feature requests are filed as TicGit tickets, not pull requests.

## References

- `ti agent` — learn TicGit workflow and practices
- `ti help` — command reference
- `.ticgit/` folder — ticket storage (committed to repo)
