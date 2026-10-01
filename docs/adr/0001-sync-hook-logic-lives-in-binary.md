# Sync hook logic lives in the `ti` binary behind a thin shim, and never blocks a push

A Sync hook is installed into a project as a ~10 line shell block, shared with the team when the hook file is committed (e.g. Husky). The block only finds `ti` and calls `ti hook run pre-push`; the recursion guard, pushed-ref filtering, remote matching and sync itself live in Rust. We chose this over copying a full self-contained script so that upgrading `ti` upgrades every project, and so `ti hook check` has nothing to drift against.

The hook never fails a push: a failed sync prints a warning and exits 0, and a missing `ti` is silent unless the repo has a `.git-meta` file. A ticket tracker should not hold code pushes hostage (offline, no meta remote), and teammates without TicGit must not be nagged.

## Considered Options

- Full script copied into the hook: self-contained, but stale copies in every project.
- Blocking on sync failure: safer for ticket data, but breaks offline pushes. A `ticgit.hook.strict` opt-in can be added later.
