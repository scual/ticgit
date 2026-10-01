# `ti delete` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a scriptable `ti delete <id>... [--recursive] [--yes] [--json|--markdown]` command that permanently deletes tickets, exposing the existing `store.delete_ticket`.

**Architecture:** A new `commands/delete.rs` (clap `Args` + `run`) modeled on `commands/close.rs`. It resolves all ids up front (validate-then-act), optionally expands subtrees, gates on a confirmation prompt (`--yes` / interactive TTY / machine-mode requires `--yes`), snapshots ticket objects before deleting, deletes via `store.delete_ticket` (which cleans parent/child/dependency refs), clears session state if a checked-out ticket was deleted, and renders text/JSON/Markdown. Wired into `commands/mod.rs` and `cli.rs`.

**Tech Stack:** Rust 2021, clap derive, serde_json, `assert_cmd` + `tempfile` CLI tests.

## Global Constraints

- Binary is `ti`; run from source with `cargo run -p ticgit -- delete ...`.
- `store.delete_ticket(&Uuid)` already un-parents children (they survive as top-level) and cleans reverse dependency refs; it does NOT recurse.
- Machine-output contract: `--json` → stdout only, no ANSI; errors → stderr; non-zero exit on failure. `--json` and `--markdown` conflict.
- `--json` output is a JSON **array** of the deleted ticket objects — reuse `render::tickets_json` (shape = existing `$defs.ticketList`). **No schema change.**
- Deletion is irreversible; the confirmation is the only guard. Default-to-checked-out is NOT allowed — id(s) are required.
- Lint clean (`cargo clippy --all-targets`), formatted (`cargo fmt`). When `cargo fmt` touches files outside your task, revert them and stage only your files.
- Commit messages: write to a file, `git commit -F <file>` (never inline `-m`).

---

### Task 1: `delete.rs` command + wiring + text-mode behavior

**Files:**
- Create: `crates/ticgit/src/commands/delete.rs`
- Modify: `crates/ticgit/src/commands/mod.rs` (add `pub mod delete;`, alphabetically between `comment` and `depends`)
- Modify: `crates/ticgit/src/cli.rs` (menu line, `Command` enum variant, dispatch arm)
- Test: `crates/ticgit/tests/cli.rs`

**Interfaces:**
- Consumes: `commands::open_store() -> Result<TicketStore>`; `store.resolve_id(&str) -> Result<Uuid>`; `store.list() -> Result<Vec<Ticket>>`; `store.load(&Uuid) -> Result<Ticket>`; `store.delete_ticket(&Uuid) -> Result<()>`; `store.session().repo_git_dir()`; `session_state::State` with `load()`, `current_for(&git_dir) -> Option<Uuid>`, `clear_current(&git_dir)`, `save()`; `Ticket::short_id()`, `Ticket::state.as_str()`, fields `id/title/children`.
- Produces: `commands::delete::{Args, run}`; private `collect_targets(roots: &[Uuid], all: &[Ticket], recursive: bool) -> Vec<Uuid>`.

- [ ] **Step 1: Write failing CLI tests (text behaviors)**

Add to `crates/ticgit/tests/cli.rs` near the other command tests. Reuse existing helpers `create_ticket`, `create_subissue`. Assume `assert_cmd` runs with a non-TTY stdin (so no-`--yes` delete must error).

```rust
#[test]
fn delete_removes_a_ticket_with_yes() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "doomed");
    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    // Gone from list.
    let out = repo.ti().args(["list", "--json"]).assert().success().get_output().stdout.clone();
    let list: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[test]
fn delete_without_yes_fails_non_interactively() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "keep me");
    repo.ti().args(["delete", &id]).assert().failure();
    // Still present.
    repo.ti().args(["show", &id, "--json"]).assert().success();
}

#[test]
fn delete_unknown_id_is_atomic() {
    let repo = TestRepo::new();
    let keep = create_ticket(&repo, "survivor");
    // Second arg is a bogus prefix; nothing should be deleted.
    repo.ti().args(["delete", &keep, "ffffffff", "--yes"]).assert().failure();
    repo.ti().args(["show", &keep, "--json"]).assert().success();
}

#[test]
fn delete_parent_orphans_children_by_default() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    repo.ti().args(["delete", &parent, "--yes"]).assert().success();
    // Child survives and is now top-level (parent is null).
    let out = repo.ti().args(["show", &child, "--json"]).assert().success().get_output().stdout.clone();
    let t: Value = serde_json::from_slice(&out).unwrap();
    assert!(t["parent"].is_null());
}

#[test]
fn delete_recursive_removes_subtree_including_closed() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");
    // Close the grandchild to prove recursive delete ignores status.
    repo.ti().args(["close", &grandchild]).assert().success();
    repo.ti().args(["delete", &parent, "--recursive", "--yes"]).assert().success();
    for id in [&parent, &child, &grandchild] {
        repo.ti().args(["show", id, "--json"]).assert().failure();
    }
}

#[test]
fn delete_multiple_ids_in_one_invocation() {
    let repo = TestRepo::new();
    let a = create_ticket(&repo, "a");
    let b = create_ticket(&repo, "b");
    repo.ti().args(["delete", &a, &b, "--yes"]).assert().success();
    let out = repo.ti().args(["list", "--json"]).assert().success().get_output().stdout.clone();
    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap().as_array().unwrap().len(), 0);
}

#[test]
fn delete_clears_checked_out_ticket() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "current");
    repo.ti().args(["checkout", &id]).assert().success();
    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    // After clearing, `ti show` with no id fails with the "none checked out"
    // message. If the session were NOT cleared, it would instead fail trying to
    // load the dangling current id — so asserting this specific message proves
    // the pointer was cleared, not left dangling.
    repo.ti()
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("none checked out"));
}
```

(Verified: `resolve_ticket` bails with "no ticket specified and none checked out ..." when no id is given and nothing is checked out — see `crates/ticgit/src/commands/mod.rs`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ticgit --test cli delete 2>&1 | tail -30`
Expected: FAIL — `delete` is not a recognized subcommand (clap error) for all.

- [ ] **Step 3: Create `crates/ticgit/src/commands/delete.rs`**

```rust
use std::io::{self, IsTerminal, Write};

use anyhow::{bail, Result};
use clap::Parser;
use uuid::Uuid;

use crate::commands::open_store;
use crate::render;
use crate::session_state::State;
use ticgit_lib::Ticket;

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id(s) or prefix(es) to delete. At least one is required.
    #[arg(required = true)]
    pub ids: Vec<String>,

    /// Also delete all descendant sub-issues (the whole subtree).
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,

    /// Skip the confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Output the deleted tickets as a JSON array.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the deleted tickets as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;

    // Validate-then-act: resolve every id up front and bail before deleting if
    // any is unknown or an ambiguous prefix.
    let mut roots: Vec<Uuid> = Vec::new();
    for reference in &args.ids {
        roots.push(store.resolve_id(reference)?);
    }

    let all_tickets = store.list()?;
    let targets = collect_targets(&roots, &all_tickets, args.recursive);

    // Snapshot ticket objects before deletion (for output).
    let mut snapshots: Vec<Ticket> = Vec::new();
    for id in &targets {
        snapshots.push(store.load(id)?);
    }

    // Confirmation gating.
    let machine = args.json || args.markdown;
    if !args.yes {
        if machine {
            bail!("refusing to delete without --yes in machine mode (--json/--markdown)");
        }
        if !io::stdin().is_terminal() {
            bail!("refusing to delete without --yes (no interactive terminal)");
        }
        if !confirm(&snapshots)? {
            println!("Aborted.");
            return Ok(());
        }
    }

    for id in &targets {
        store.delete_ticket(id)?;
    }

    // Clear session state if a deleted id was the checked-out ticket.
    let git_dir = store.session().repo_git_dir();
    let mut state = State::load().unwrap_or_default();
    if let Some(current) = state.current_for(&git_dir) {
        if targets.contains(&current) {
            state.clear_current(&git_dir);
            state.save()?;
        }
    }

    if args.json {
        println!("{}", render::tickets_json(&snapshots)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", deleted_markdown(&snapshots));
        return Ok(());
    }
    println!("Deleted {} ticket(s):", snapshots.len());
    for t in &snapshots {
        println!("  {} — {}", t.short_id(), t.title);
    }
    Ok(())
}

/// Collect the deduped, ordered set of ids to delete. Without `recursive`, this
/// is just the roots (deduped). With `recursive`, each root is expanded into its
/// full descendant subtree (including closed descendants), guarding cycles and
/// skipping dangling child references.
fn collect_targets(roots: &[Uuid], all: &[Ticket], recursive: bool) -> Vec<Uuid> {
    use std::collections::{HashMap, HashSet};
    let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, t)).collect();
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut ordered: Vec<Uuid> = Vec::new();
    for root in roots {
        collect_one(*root, &by_id, recursive, &mut seen, &mut ordered);
    }
    ordered
}

fn collect_one(
    id: Uuid,
    by_id: &std::collections::HashMap<Uuid, &Ticket>,
    recursive: bool,
    seen: &mut std::collections::HashSet<Uuid>,
    ordered: &mut Vec<Uuid>,
) {
    if !seen.insert(id) {
        return;
    }
    ordered.push(id);
    if recursive {
        if let Some(t) = by_id.get(&id) {
            for child in &t.children {
                if by_id.contains_key(child) {
                    collect_one(*child, by_id, recursive, seen, ordered);
                }
            }
        }
    }
}

fn confirm(snaps: &[Ticket]) -> Result<bool> {
    eprintln!("About to delete {} ticket(s):", snaps.len());
    for t in snaps {
        eprintln!("  {} — {} ({})", t.short_id(), t.title, t.state.as_str());
    }
    eprint!("Delete {} ticket(s)? [y/N] ", snaps.len());
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn deleted_markdown(snaps: &[Ticket]) -> String {
    let mut out = String::from("# Deleted tickets\n\n");
    for t in snaps {
        out.push_str(&format!("- {} — {}\n", t.short_id(), t.title));
    }
    out
}
```

Note: every id in `targets` is guaranteed to exist — roots are validated by `resolve_id`, and recursion only descends into children present in `by_id` — so `store.load(id)` in the snapshot loop cannot fail on a dangling id.

- [ ] **Step 4: Wire the module in `commands/mod.rs`**

Add, in alphabetical position between `pub mod comment;` and `pub mod depends;`:
```rust
pub mod delete;
```

- [ ] **Step 5: Wire the subcommand in `cli.rs`**

(a) Add a menu line in the help template, right after the `close` line (near line 35):
```
  delete        Delete ticket(s) permanently
```
(b) Add the `Command` enum variant, right after the `Close(...)` variant (near line 154):
```rust
    /// Delete one or more tickets permanently.
    Delete(commands::delete::Args),
```
(c) Add the dispatch arm, right after the `Close` arm (near line 243):
```rust
        Some(Command::Delete(args)) => commands::delete::run(args),
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ticgit --test cli delete 2>&1 | tail -30`
Expected: PASS — all seven tests green.

- [ ] **Step 7: Lint + format**

Run: `cargo fmt && cargo clippy --all-targets 2>&1 | tail -20`
Expected: no warnings from `delete.rs`/`cli.rs`/`mod.rs` (a pre-existing unrelated `tui.rs` warning may remain).

- [ ] **Step 8: Commit**

Write the message to a file and `git commit -F <file>`. Subject: `feat: add ti delete command`. Stage `crates/ticgit/src/commands/delete.rs`, `crates/ticgit/src/commands/mod.rs`, `crates/ticgit/src/cli.rs`, `crates/ticgit/tests/cli.rs`.

---

### Task 2: machine-output tests (`--json` array, `--markdown`, machine-mode gating)

Pure test additions that lock the output behavior implemented in Task 1.

**Files:**
- Test: `crates/ticgit/tests/cli.rs`

**Interfaces:**
- Consumes: the `ti delete` behavior from Task 1.
- Produces: regression tests for JSON/Markdown output and machine-mode confirmation gating.

- [ ] **Step 1: Write the tests**

```rust
#[test]
fn delete_json_emits_array_of_deleted_tickets() {
    let repo = TestRepo::new();
    let a = create_ticket(&repo, "alpha");
    let b = create_ticket(&repo, "beta");
    let out = repo
        .ti()
        .args(["delete", &a, &b, "--yes", "--json"])
        .assert()
        .success()
        .stderr(predicate::eq(""))
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    assert!(!stdout.contains("\x1b["), "no ANSI in JSON: {stdout:?}");
    let arr: Value = serde_json::from_str(&stdout).unwrap();
    let ids: std::collections::BTreeSet<String> = arr
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, std::collections::BTreeSet::from([a, b]));
}

#[test]
fn delete_json_is_array_even_for_single_id() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "solo");
    let out = repo
        .ti()
        .args(["delete", &id, "--yes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(v.is_array());
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], id);
}

#[test]
fn delete_json_requires_yes() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "needs yes");
    repo.ti()
        .args(["delete", &id, "--json"])
        .assert()
        .failure()
        .stdout(predicate::eq(""));
    // Not deleted.
    repo.ti().args(["show", &id, "--json"]).assert().success();
}

#[test]
fn delete_markdown_lists_deleted_tickets() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "mark me");
    let out = repo
        .ti()
        .args(["delete", &id, "--yes", "--markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(out).unwrap();
    assert!(md.contains("# Deleted tickets"), "missing heading in:\n{md}");
    assert!(md.contains("mark me"), "missing title in:\n{md}");
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ticgit --test cli delete 2>&1 | tail -30`
Expected: PASS — Task 1 tests plus these four.

- [ ] **Step 3: Commit**

Write the message to a file and `git commit -F <file>`. Subject: `test: cover ti delete machine output and gating`. Stage `crates/ticgit/tests/cli.rs`.

---

### Task 3: Full-suite verification + reinstall

**Files:** none (verification only).

- [ ] **Step 1: Full suite**

Run: `cargo test 2>&1 | grep "test result:"`
Expected: all suites pass (bin unit tests, `cli`, `ticgit_lib`).

- [ ] **Step 2: Lint + format gate**

Run: `cargo fmt --check -- crates/ticgit/src/commands/delete.rs crates/ticgit/src/cli.rs crates/ticgit/src/commands/mod.rs crates/ticgit/tests/cli.rs && echo FMT_OK` and `cargo clippy --all-targets 2>&1 | tail -20`
Expected: `FMT_OK`; no new clippy warnings in the touched files.

- [ ] **Step 3: Reinstall and smoke test**

Run: `cargo install --path crates/ticgit --locked`
Then in a scratch repo: create a parent + child, `ti delete <parent> --yes` (child orphaned), and `ti delete <id> --recursive --yes`. Confirm `ti list` reflects the deletions and `ti delete <id>` without `--yes` errors non-interactively.

---

## Self-Review

**Spec coverage:**
- Required id(s), no default-to-checkout → `#[arg(required = true)] ids: Vec<String>`. ✓
- Validate-then-act → resolve loop before any delete; `delete_unknown_id_is_atomic` test. ✓
- Orphan default / `--recursive` incl. closed → `collect_targets`; `delete_parent_orphans_children_by_default` + `delete_recursive_removes_subtree_including_closed`. ✓
- Confirmation: prompt unless `--yes`; machine requires `--yes`; non-TTY requires `--yes` → gating block; `delete_without_yes_fails_non_interactively` + `delete_json_requires_yes`. ✓
- Session-state clear → `delete_clears_checked_out_ticket`. ✓
- Output text/json(array)/markdown → `tickets_json` + `deleted_markdown`; Task 2 tests. ✓
- No schema change (reuses `ticketList`) → confirmed; no schema task. ✓

**Placeholder scan:** none — full code in every code step; full test bodies inline. ✓

**Type consistency:** `collect_targets(&[Uuid], &[Ticket], bool) -> Vec<Uuid>`, `confirm(&[Ticket]) -> Result<bool>`, `deleted_markdown(&[Ticket]) -> String`, `render::tickets_json(&[Ticket]) -> Result<String, serde_json::Error>` — consistent across the file. ✓

**Verified assumption:** `resolve_ticket` (commands/mod.rs) bails non-zero with "no ticket specified and none checked out ..." when no id is passed and nothing is checked out, so `delete_clears_checked_out_ticket` asserts `.failure()` + that message (distinguishing a cleared pointer from a dangling one).
