<!-- ticgit-agent-start -->
## TicGit

This project uses TicGit (`ti`) for Git-native ticket tracking.

- Run `ti agent` to learn the TicGit workflow, command examples, and agent practices.
- Prefer `ti list --markdown` and `ti show <id> --markdown` when reading ticket data.
- Use `ti comment`, `ti state`, and `ti close` to record progress and resolution.
- When generating code to implement an issue, open a review for that branch on that issue (`ti review`)
<!-- ticgit-agent-end -->

## Agent skills

### Issue tracker

Issues tracked in TicGit (git-native tickets). See `docs/agents/issue-tracker.md`.

### Triage labels

Five canonical states: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout with `CONTEXT.md` and `docs/adr/` at repo root. See `docs/agents/domain.md`.
