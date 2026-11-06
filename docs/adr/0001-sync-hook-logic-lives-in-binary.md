# Sync hook logic lives in the `ti` binary behind a thin shim, and blocks a push only for a lost race window

A Sync hook is installed into a project as a ~10 line shell block, shared with the team when the hook file is committed (e.g. Husky). The block only finds `ti` and calls `ti hook run pre-push`; the recursion guard, pushed-ref filtering, remote matching and sync itself live in Rust. We chose this over copying a full self-contained script so that upgrading `ti` upgrades every project, and so `ti hook check` has nothing to drift against.

The hook almost never fails a push: a failed sync prints a warning and exits 0, and a missing `ti` is silent unless the repo has a `.git-meta` file. A ticket tracker should not hold code pushes hostage (offline, no meta remote), and teammates without TicGit must not be nagged.

The one exception (added 2026-11-06) is a sync that lost the race window: the remote was reachable but kept advancing between the pull and the push until the retries ran out (`PushRejected`). Letting the code through there recreates the exact failure the retry fixed — the code lands on the remote while its ticket state silently stays behind — and pushing again resolves it, which is not true of the offline or integrity failures. So the hook fails the push with a red line saying to push again; every other failure stays a warning.

## Considered Options

- Full script copied into the hook: self-contained, but stale copies in every project.
- Blocking on every sync failure: safer for ticket data, but breaks offline pushes. A `ticgit.hook.strict` opt-in can be added later.
- Warning on a lost race window too: keeps the "never blocks" rule absolute, but the warning scrolls past under git's own push output and the tickets stay invisible to the team until someone syncs again.
