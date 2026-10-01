# Recursive Sub-issues in `ti next` and `ti show` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a recursive, open-only sub-issue tree (id + title + state) for the ticket emitted by `ti next` and `ti show`, in plain-text, JSON, and Markdown output.

**Architecture:** A shared tree builder in `render.rs` walks `Ticket.children` recursively over an `id → &Ticket` map (both commands already load the full ticket set), pruning closed sub-issues and guarding cycles with a visited set. Three sibling renderers turn the tree into text / JSON / Markdown. JSON is additive: the emitted ticket object is unchanged and gains one new `subissues` field.

**Tech Stack:** Rust 2021, clap, serde / serde_json, the `ti` CLI integration suite (`assert_cmd` + `tempfile`).

## Global Constraints

- Binary name is `ti`; run from source with `cargo run -p ticgit -- <args>`.
- `--json` is stable schema v1 — the ticket object's existing shape must NOT change; `subissues` is a pure addition. Schema lives in two copies that must stay in sync: `docs/schema/v1.json` and `crates/ticgit/docs/schema/v1.json`.
- Successful JSON → stdout only, no ANSI color; errors → stderr; non-zero exit on failure.
- `gix-actor` stays pinned to `=0.40.0` — do not touch dependencies.
- Lint clean: `cargo clippy --all-targets`. Format: `cargo fmt`.
- The closed-pruning rule: a closed sub-issue and its entire subtree are omitted.

---

### Task 1: Tree builder + three renderers in `render.rs`

Pure library-style functions with unit tests in the `render.rs` `#[cfg(test)]` module. No command wiring yet.

**Files:**
- Modify: `crates/ticgit/src/render.rs` (imports near line 3; add new public items; add unit tests in the existing `mod tests`)

**Interfaces:**
- Consumes: `ticgit_lib::Ticket` (fields `id: Uuid`, `title: String`, `state: TicketState`, `status: TicketStatus`, `children: BTreeSet<Uuid>`), `ticgit_lib::TicketStatus` (already imported), `Ticket::state.as_str() -> &str`, private `short_hex(&Uuid) -> String`, private `flatten(&str) -> String`, private `markdown_inline(&str) -> String`.
- Produces:
  - `pub struct SubissueNode { pub id: Uuid, pub title: String, pub state: String, pub subissues: Vec<SubissueNode> }`
  - `pub fn build_subissue_tree(root: &Ticket, by_id: &HashMap<Uuid, &Ticket>) -> Vec<SubissueNode>`
  - `pub fn subissue_tree_text(nodes: &[SubissueNode]) -> String`
  - `pub fn subissue_tree_json(nodes: &[SubissueNode]) -> serde_json::Value`
  - `pub fn subissue_tree_markdown(nodes: &[SubissueNode]) -> String`

- [ ] **Step 1: Add `HashSet` to the collections import**

In `crates/ticgit/src/render.rs`, change the line:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap};
```

to:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
```

- [ ] **Step 2: Write failing unit tests**

Add these tests to the existing `#[cfg(test)] mod tests` block at the bottom of `render.rs`. They reuse the module's existing `fn ticket(id: &str, title: &str, state: TicketState) -> Ticket` helper (line ~1341), which sets `status` from `state` via `state.status()` — so a `TicketState::Resolved` ticket is closed, and `TicketState::New` is open.

First, extend the test module's collections import. Change its existing line:
```rust
    use std::collections::{BTreeMap, BTreeSet};
```
to:
```rust
    use std::collections::{BTreeMap, BTreeSet, HashMap};
```

Then add this local builder plus the tests:

```rust
    // Build a ticket with a fresh id, given state, and the given children.
    // Reuses the existing `ticket()` helper (status derived from state).
    fn sub(title: &str, state: TicketState, children: &[Uuid]) -> Ticket {
        let mut t = ticket(&Uuid::new_v4().to_string(), title, state);
        t.children = children.iter().copied().collect();
        t
    }

    fn node_ids(nodes: &[SubissueNode]) -> Vec<Uuid> {
        nodes.iter().map(|n| n.id).collect()
    }

    #[test]
    fn build_subissue_tree_nests_multiple_levels() {
        let grand = sub("grandchild", TicketState::New, &[]);
        let child = sub("child", TicketState::New, &[grand.id]);
        let root = sub("root", TicketState::New, &[child.id]);
        let all = [&root, &child, &grand];
        let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, *t)).collect();

        let tree = build_subissue_tree(&root, &by_id);
        assert_eq!(node_ids(&tree), vec![child.id]);
        assert_eq!(node_ids(&tree[0].subissues), vec![grand.id]);
        assert_eq!(tree[0].title, "child");
        assert_eq!(tree[0].state, "new");
    }

    #[test]
    fn build_subissue_tree_prunes_closed_subtree() {
        let grand = sub("grandchild", TicketState::New, &[]);
        // Resolved => status Closed.
        let child = sub("child", TicketState::Resolved, &[grand.id]);
        let root = sub("root", TicketState::New, &[child.id]);
        let all = [&root, &child, &grand];
        let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, *t)).collect();

        // Closed child is dropped along with its (open) grandchild.
        assert!(build_subissue_tree(&root, &by_id).is_empty());
    }

    #[test]
    fn build_subissue_tree_survives_cycle() {
        let mut a = sub("a", TicketState::New, &[]);
        let mut b = sub("b", TicketState::New, &[]);
        a.children = [b.id].into_iter().collect();
        b.children = [a.id].into_iter().collect();
        let all = [&a, &b];
        let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, *t)).collect();

        let tree = build_subissue_tree(&a, &by_id);
        assert_eq!(node_ids(&tree), vec![b.id]);
        assert!(tree[0].subissues.is_empty()); // a already visited
    }

    #[test]
    fn subissue_tree_json_is_recursive() {
        let grand = sub("grandchild", TicketState::New, &[]);
        let child = sub("child", TicketState::New, &[grand.id]);
        let root = sub("root", TicketState::New, &[child.id]);
        let all = [&root, &child, &grand];
        let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, *t)).collect();

        let json = subissue_tree_json(&build_subissue_tree(&root, &by_id));
        assert_eq!(json[0]["title"], "child");
        assert_eq!(json[0]["state"], "new");
        assert_eq!(json[0]["id"], child.id.to_string());
        assert_eq!(json[0]["subissues"][0]["id"], grand.id.to_string());
    }

    #[test]
    fn subissue_tree_json_empty_is_array() {
        assert_eq!(subissue_tree_json(&[]), serde_json::json!([]));
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ticgit --lib render 2>&1 | tail -30`
Expected: FAIL — `cannot find function build_subissue_tree` / `SubissueNode` not found.

- [ ] **Step 4: Implement the tree builder and renderers**

Add to `crates/ticgit/src/render.rs` (place after the `format_related` function, around line 64). `TicketStatus` is already imported at the top; add `use ticgit_lib::TicketState;` only if the renderers need it — they use `node.state: String`, so no extra import is required.

```rust
/// A node in the recursive, open-only sub-issue tree. Each node carries a
/// child's id, title, specific state, and its own sub-issues.
pub struct SubissueNode {
    pub id: Uuid,
    pub title: String,
    pub state: String,
    pub subissues: Vec<SubissueNode>,
}

/// Build the recursive sub-issue tree for `root`, using `by_id` to resolve child
/// ids. Closed sub-issues (and their whole subtree) are omitted; a visited set
/// guards against cycles, and ids absent from `by_id` are skipped.
pub fn build_subissue_tree(root: &Ticket, by_id: &HashMap<Uuid, &Ticket>) -> Vec<SubissueNode> {
    let mut visited = HashSet::new();
    visited.insert(root.id);
    build_subissue_nodes(&root.children, by_id, &mut visited)
}

fn build_subissue_nodes(
    ids: &BTreeSet<Uuid>,
    by_id: &HashMap<Uuid, &Ticket>,
    visited: &mut HashSet<Uuid>,
) -> Vec<SubissueNode> {
    let mut out = Vec::new();
    for id in ids {
        if visited.contains(id) {
            continue;
        }
        let Some(child) = by_id.get(id) else {
            continue;
        };
        if child.status == TicketStatus::Closed {
            continue;
        }
        visited.insert(*id);
        out.push(SubissueNode {
            id: child.id,
            title: child.title.clone(),
            state: child.state.as_str().to_string(),
            subissues: build_subissue_nodes(&child.children, by_id, visited),
        });
    }
    out
}

/// Render the sub-issue tree as indented plain text (2 spaces per level, no
/// header). Returns an empty string when there are no nodes.
pub fn subissue_tree_text(nodes: &[SubissueNode]) -> String {
    let mut out = String::new();
    write_subissue_text(nodes, 1, &mut out);
    out
}

fn write_subissue_text(nodes: &[SubissueNode], depth: usize, out: &mut String) {
    for n in nodes {
        let indent = "  ".repeat(depth);
        let _ = writeln!(out, "{indent}{} {}  {}", short_hex(&n.id), n.state, n.title);
        write_subissue_text(&n.subissues, depth + 1, out);
    }
}

/// Render the sub-issue tree as a recursive JSON array. Each element is
/// `{ id, title, state, subissues }`. Returns `[]` for no nodes.
pub fn subissue_tree_json(nodes: &[SubissueNode]) -> serde_json::Value {
    serde_json::Value::Array(
        nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id.to_string(),
                    "title": n.title,
                    "state": n.state,
                    "subissues": subissue_tree_json(&n.subissues),
                })
            })
            .collect(),
    )
}

/// Render the sub-issue tree as a nested Markdown bullet list (no header).
/// Returns an empty string when there are no nodes.
pub fn subissue_tree_markdown(nodes: &[SubissueNode]) -> String {
    let mut out = String::new();
    write_subissue_markdown(nodes, 0, &mut out);
    out
}

fn write_subissue_markdown(nodes: &[SubissueNode], depth: usize, out: &mut String) {
    for n in nodes {
        let indent = "  ".repeat(depth);
        let _ = writeln!(
            out,
            "{indent}- {} `{}` — {}",
            short_hex(&n.id),
            n.state,
            markdown_inline(&flatten(&n.title))
        );
        write_subissue_markdown(&n.subissues, depth + 1, out);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ticgit --lib render 2>&1 | tail -30`
Expected: PASS — all five new tests green.

- [ ] **Step 6: Lint and format**

Run: `cargo fmt && cargo clippy --all-targets 2>&1 | tail -20`
Expected: no warnings from `render.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/ticgit/src/render.rs
git commit -F <commit-msg-file>
```
Commit subject: `feat: add recursive sub-issue tree builder and renderers`
(Write the message to a file and use `git commit -F <file>` per repo convention — never inline `-m`.)

---

### Task 2: JSON helper that injects `subissues` onto the ticket object

A shared helper both commands use for `--json`, keeping the ticket object additive.

**Files:**
- Modify: `crates/ticgit/src/render.rs` (add after `ticket_json`, ~line 591; add a unit test in `mod tests`)

**Interfaces:**
- Consumes: `build_subissue_tree`, `subissue_tree_json` (Task 1); `ticgit_lib::Ticket`.
- Produces: `pub fn ticket_json_with_subissues(t: &Ticket, by_id: &HashMap<Uuid, &Ticket>) -> Result<String, serde_json::Error>` — pretty-printed ticket object identical to `ticket_json` plus a top-level `"subissues"` array.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `render.rs` (reuses the `sub()` helper from Task 1):

```rust
    #[test]
    fn ticket_json_with_subissues_is_additive() {
        let child = sub("child", TicketState::New, &[]);
        let root = sub("root", TicketState::New, &[child.id]);
        let all = [&root, &child];
        let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, *t)).collect();

        let s = ticket_json_with_subissues(&root, &by_id).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();

        // Original ticket fields are untouched.
        assert_eq!(v["id"], root.id.to_string());
        assert_eq!(v["title"], "root");
        assert!(v["children"].is_array()); // existing UUID array preserved
        // New additive field carries the nested tree.
        assert_eq!(v["subissues"][0]["id"], child.id.to_string());
    }

    #[test]
    fn ticket_json_with_subissues_empty_when_no_children() {
        let root = sub("root", TicketState::New, &[]);
        let by_id: HashMap<Uuid, &Ticket> = [(root.id, &root)].into_iter().collect();
        let s = ticket_json_with_subissues(&root, &by_id).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["subissues"], serde_json::json!([]));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ticgit --lib ticket_json_with_subissues 2>&1 | tail -20`
Expected: FAIL — `cannot find function ticket_json_with_subissues`.

- [ ] **Step 3: Implement the helper**

Add after `ticket_json` in `render.rs`:

```rust
/// Serialize a ticket as pretty JSON with an additive `subissues` field holding
/// the recursive open-only sub-issue tree. The ticket object itself is
/// unchanged (schema v1), so `subissues` is a pure addition.
pub fn ticket_json_with_subissues(
    t: &Ticket,
    by_id: &HashMap<Uuid, &Ticket>,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(t)?;
    if let serde_json::Value::Object(map) = &mut value {
        let tree = build_subissue_tree(t, by_id);
        map.insert("subissues".to_string(), subissue_tree_json(&tree));
    }
    serde_json::to_string_pretty(&value)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ticgit --lib ticket_json 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ticgit/src/render.rs
git commit -F <commit-msg-file>
```
Commit subject: `feat: add additive subissues JSON serializer for tickets`

---

### Task 3: Wire `ti next` to emit the sub-issue tree (all three formats)

**Files:**
- Modify: `crates/ticgit/src/commands/next.rs`
- Test: `crates/ticgit/tests/cli.rs`

**Interfaces:**
- Consumes: `render::build_subissue_tree`, `render::subissue_tree_text`, `render::subissue_tree_markdown`, `render::ticket_json_with_subissues` (Tasks 1–2).
- Produces: `ti next` output augmented with sub-issues; `ti next --json` ticket object gains `subissues`.

- [ ] **Step 1: Write the failing CLI tests**

Add to `crates/ticgit/tests/cli.rs` near the other `next_*` tests (after `next_skips_subissues`, ~line 2498). `create_subissue(&repo, parent, title)` already exists and returns the new child's id.

```rust
#[test]
fn next_json_includes_recursive_subissues() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");

    let v = next_json(&repo);
    // Ticket object is unchanged; subissues is additive.
    assert_eq!(v["id"], parent);
    assert_eq!(v["subissues"][0]["id"], child);
    assert_eq!(v["subissues"][0]["title"], "child");
    assert_eq!(v["subissues"][0]["state"], "new");
    assert_eq!(v["subissues"][0]["subissues"][0]["id"], grandchild);
}

#[test]
fn next_json_subissues_excludes_closed() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    repo.ti().args(["close", &child]).assert().success();

    let v = next_json(&repo);
    assert_eq!(v["id"], parent);
    assert_eq!(v["subissues"], serde_json::json!([]));
}

#[test]
fn next_text_shows_subissue_tree() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    create_subissue(&repo, &parent, "child");

    let out = repo.ti().arg("next").assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Sub-issues:"), "missing header in:\n{text}");
    assert!(text.contains("child"), "missing child in:\n{text}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ticgit --test cli next_json_includes_recursive_subissues next_json_subissues_excludes_closed next_text_shows_subissue_tree 2>&1 | tail -30`
Expected: FAIL — `subissues` key missing / `Sub-issues:` not found.

- [ ] **Step 3: Restructure candidate selection to keep the ticket set alive**

Replace the body of `run` in `crates/ticgit/src/commands/next.rs` from the `let all_tickets = store.list()?;` line down to the `let ticket = match candidates.into_iter().next()` block. The change: keep `all_tickets` owned, select candidates as `&Ticket`, and build a `by_id` map. Full replacement for that region:

```rust
    let store = open_store()?;
    let git_dir = store.session().repo_git_dir();
    let all_tickets = store.list()?;

    let by_id: std::collections::HashMap<uuid::Uuid, &Ticket> =
        all_tickets.iter().map(|t| (t.id, t)).collect();

    // Build a set of closed ticket IDs for dependency checking
    let closed_ids: std::collections::HashSet<uuid::Uuid> = all_tickets
        .iter()
        .filter(|t| t.status == TicketStatus::Closed)
        .map(|t| t.id)
        .collect();

    let mut candidates: Vec<&Ticket> = all_tickets
        .iter()
        .filter(|t| t.status == TicketStatus::Open)
        .filter(|t| t.parent.is_none())
        .filter(|t| t.depends_on.iter().all(|dep| closed_ids.contains(dep)))
        .filter(|t| {
            if let Some(tag) = &args.tag {
                t.tags.contains(tag)
            } else {
                true
            }
        })
        .filter(|t| {
            if let Some(assigned) = &args.assigned {
                t.assigned.as_deref() == Some(assigned.as_str())
            } else {
                true
            }
        })
        .collect();

    let max_priority = candidates
        .iter()
        .filter_map(|t| t.priority)
        .max()
        .unwrap_or(0);

    candidates.sort_by_key(|t| std::cmp::Reverse(score(t, max_priority)));

    let ticket = match candidates.into_iter().next() {
        Some(t) => t,
        None => {
            if args.json {
                println!("{}", serde_json::json!({ "next": null }));
            } else if args.markdown {
                println!("# Next Ticket\n\nNo open tickets match the criteria.");
            } else {
                println!("No open tickets to work on.");
            }
            return Ok(());
        }
    };
```

Note: `ticket` is now `&Ticket`, and `score` is called with `t: &&Ticket` so it stays `fn score(t: &Ticket, ...)` — `sort_by_key` derefs automatically. If the compiler complains about `score(t, ...)`, change that one call to `score(t, max_priority)` where the closure param already yields `&&Ticket`; Rust auto-derefs the method-free call, but if needed write `score(&**t, max_priority)`.

- [ ] **Step 4: Emit the tree in each output path**

Below the `let ticket = ...` block, the code sets session state (`state.set_current(&git_dir, ticket.id)`) — unchanged. Then replace the three output branches:

Replace:
```rust
    if args.json {
        println!("{}", render::ticket_json(&ticket)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(&ticket));
        return Ok(());
    }
```
with:
```rust
    if args.json {
        println!("{}", render::ticket_json_with_subissues(ticket, &by_id)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(ticket));
        let tree = render::build_subissue_tree(ticket, &by_id);
        if !tree.is_empty() {
            println!("## Sub-issues\n\n{}", render::subissue_tree_markdown(&tree));
        }
        return Ok(());
    }
```

Then, at the end of the plain-text branch (after the existing `println!("Checked out.");` line, before `Ok(())`), append:

```rust
    let tree = render::build_subissue_tree(ticket, &by_id);
    if !tree.is_empty() {
        println!("Sub-issues:");
        print!("{}", render::subissue_tree_text(&tree));
    }
```

Also update the remaining `ticket.` references in the text branch that borrowed `&ticket` — they now use `ticket` directly (it is already `&Ticket`); e.g. `ticket.short_id()`, `ticket.title`, `ticket.state`, `&ticket.assigned`, `ticket.tags` all work unchanged because field/method access auto-derefs. No signature of `render` functions changes.

- [ ] **Step 5: Run the new + existing next tests**

Run: `cargo test -p ticgit --test cli next 2>&1 | tail -30`
Expected: PASS — new three plus existing `next_*` (including `next_skips_subissues`, `next_json_is_null_when_nothing_workable`) all green.

- [ ] **Step 6: Lint and format**

Run: `cargo fmt && cargo clippy --all-targets 2>&1 | tail -20`
Expected: no warnings from `next.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/ticgit/src/commands/next.rs crates/ticgit/tests/cli.rs
git commit -F <commit-msg-file>
```
Commit subject: `feat: show recursive sub-issue tree in ti next`

---

### Task 4: Wire `ti show` to emit the sub-issue tree (all three formats)

`show` keeps its existing flat `Children:` relation line and gains a recursive `Sub-issues:` section below it.

**Files:**
- Modify: `crates/ticgit/src/commands/show.rs`
- Test: `crates/ticgit/tests/cli.rs`

**Interfaces:**
- Consumes: `render::build_subissue_tree`, `render::subissue_tree_text`, `render::subissue_tree_markdown`, `render::ticket_json_with_subissues` (Tasks 1–2); `store.list()`.
- Produces: `ti show` output with recursive sub-issues; `ti show --json` ticket object gains `subissues`.

- [ ] **Step 1: Write the failing CLI tests**

Add to `crates/ticgit/tests/cli.rs` near the other show tests (grep for `fn show_` to find the cluster):

```rust
#[test]
fn show_json_includes_recursive_subissues() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");

    let out = repo
        .ti()
        .args(["show", &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["id"], parent);
    assert!(v["children"].is_array()); // flat UUID array preserved
    assert_eq!(v["subissues"][0]["id"], child);
    assert_eq!(v["subissues"][0]["subissues"][0]["id"], grandchild);
}

#[test]
fn show_text_shows_subissue_tree() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    create_subissue(&repo, &parent, "child");

    let out = repo
        .ti()
        .args(["show", &parent])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Sub-issues:"), "missing header in:\n{text}");
    assert!(text.contains("child"), "missing child in:\n{text}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ticgit --test cli show_json_includes_recursive_subissues show_text_shows_subissue_tree 2>&1 | tail -30`
Expected: FAIL — `subissues` missing / `Sub-issues:` not found.

- [ ] **Step 3: Update the JSON path to inject `subissues`**

In `crates/ticgit/src/commands/show.rs`, the JSON branch currently reads:

```rust
    if args.json {
        println!("{}", render::ticket_json(&ticket)?);
    } else if args.markdown {
```

Replace the `if args.json { ... }` body with a version that loads the full set and injects the tree:

```rust
    if args.json {
        let all = store.list().unwrap_or_default();
        let by_id: std::collections::HashMap<uuid::Uuid, &ticgit_lib::Ticket> =
            all.iter().map(|t| (t.id, t)).collect();
        println!("{}", render::ticket_json_with_subissues(&ticket, &by_id)?);
    } else if args.markdown {
```

- [ ] **Step 4: Append the tree to the markdown and text paths**

The markdown branch currently builds `all` and `rels` then prints `ticket_markdown_with_rels`. After that `println!`, within the same `else if args.markdown` block, append:

```rust
        let by_id: std::collections::HashMap<uuid::Uuid, &ticgit_lib::Ticket> =
            all.iter().map(|t| (t.id, t)).collect();
        let tree = render::build_subissue_tree(&ticket, &by_id);
        if !tree.is_empty() {
            println!("\n## Sub-issues\n\n{}", render::subissue_tree_markdown(&tree));
        }
```
(`all` is already in scope in this branch — reuse it; do not call `store.list()` twice.)

The final `else` (plain text) branch builds `all`, `rels`, `nicks` and prints `ticket_detail`. After that `print!`, append:

```rust
        let by_id: std::collections::HashMap<uuid::Uuid, &ticgit_lib::Ticket> =
            all.iter().map(|t| (t.id, t)).collect();
        let tree = render::build_subissue_tree(&ticket, &by_id);
        if !tree.is_empty() {
            println!("Sub-issues:");
            print!("{}", render::subissue_tree_text(&tree));
        }
```
(Again reuse the existing `all` binding in that branch.)

- [ ] **Step 5: Run the new + existing show tests**

Run: `cargo test -p ticgit --test cli show 2>&1 | tail -30`
Expected: PASS — new tests plus all existing `show_*` green (flat `Children:` line and `--filter` unaffected).

- [ ] **Step 6: Lint and format**

Run: `cargo fmt && cargo clippy --all-targets 2>&1 | tail -20`
Expected: no warnings from `show.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/ticgit/src/commands/show.rs crates/ticgit/tests/cli.rs
git commit -F <commit-msg-file>
```
Commit subject: `feat: show recursive sub-issue tree in ti show`

---

### Task 5: Document `subissues` in the JSON schema (both copies)

**Files:**
- Modify: `docs/schema/v1.json`
- Modify: `crates/ticgit/docs/schema/v1.json`

**Interfaces:**
- Consumes: the JSON shape produced in Tasks 2–4.
- Produces: schema documents describing the additive `subissues` field.

- [ ] **Step 1: Add a reusable node definition and the `subissues` property**

In `docs/schema/v1.json`, add a `subissues` property to the ticket object's `properties` block, immediately after the `children` property (around line 155). Use a `$defs` self-referential node so nesting is expressed once:

```json
        "subissues": {
          "type": "array",
          "description": "Recursive open-only sub-issue tree. Additive field emitted by `ti next` and `ti show`; absent from `ti list` and mutation output. Existing consumers may ignore it.",
          "items": { "$ref": "#/$defs/subissueNode" }
        },
```

Then add (or extend) a top-level `$defs` block in the same file:

```json
  "$defs": {
    "subissueNode": {
      "type": "object",
      "required": ["id", "title", "state", "subissues"],
      "properties": {
        "id": { "type": "string", "format": "uuid" },
        "title": { "type": "string" },
        "state": { "type": "string" },
        "subissues": {
          "type": "array",
          "items": { "$ref": "#/$defs/subissueNode" }
        }
      }
    }
  }
```

If a `$defs` block already exists, merge `subissueNode` into it rather than adding a second one. Place the top-level `$defs` as a sibling of the existing top-level keys (e.g. after the root `properties`/`description`), valid anywhere at the document root.

- [ ] **Step 2: Update the top-level description string**

In the root `"description"` (line 5), append a sentence: ``Ticket objects from `ti next` and `ti show` also carry an additive `subissues` array (recursive open-only sub-issue tree).``

- [ ] **Step 3: Mirror both edits into the bundled copy**

Apply the identical two edits (Steps 1–2) to `crates/ticgit/docs/schema/v1.json` so the copies stay byte-identical. Verify:

Run: `diff docs/schema/v1.json crates/ticgit/docs/schema/v1.json && echo IN_SYNC`
Expected: `IN_SYNC` (no diff output).

- [ ] **Step 4: Validate JSON is well-formed**

Run: `python3 -m json.tool docs/schema/v1.json >/dev/null && python3 -m json.tool crates/ticgit/docs/schema/v1.json >/dev/null && echo OK`
Expected: `OK`.

- [ ] **Step 5: Commit**

```bash
git add docs/schema/v1.json crates/ticgit/docs/schema/v1.json
git commit -F <commit-msg-file>
```
Commit subject: `docs: document additive subissues field in schema v1`

---

### Task 6: Full-suite verification

**Files:** none (verification only).

- [ ] **Step 1: Run the entire test suite**

Run: `cargo test 2>&1 | tail -30`
Expected: all tests pass, including the full `cli` integration suite.

- [ ] **Step 2: Lint + format gate**

Run: `cargo fmt --check && cargo clippy --all-targets 2>&1 | tail -20`
Expected: no formatting diff, no clippy warnings.

- [ ] **Step 3: Manual smoke test**

Run from a scratch repo:
```bash
cd "$(mktemp -d)" && git init -q && cargo run -q -p ticgit --manifest-path <repo>/Cargo.toml -- new --title parent --id-only
```
Then create a child via `ti subissue` / `new --subissue`, run `ti next` and `ti next --json`, and confirm the `Sub-issues:` block and the `subissues` JSON field appear. (Use the real repo path for `--manifest-path`.)
Expected: tree visible in text; `subissues` array present and nested in JSON.

---

## Self-Review

**Spec coverage:**
- Per-child id+title+state → `SubissueNode` + renderers (Task 1). ✓
- Three formats → text/json/markdown renderers (Task 1), wired in Tasks 3–4. ✓
- Closed pruning → `build_subissue_nodes` skip + `*_excludes_closed` tests. ✓
- Scope next + show → Tasks 3 & 4; `list` untouched. ✓
- Additive JSON (ticket object unchanged, new `subissues`) → Task 2 + tests asserting `children` preserved. ✓
- show keeps flat `Children:` line → Task 4 appends below, does not remove. ✓
- Schema both copies → Task 5 with `diff` sync check. ✓
- Cycle/orphan safety → `visited` set + `build_subissue_tree_survives_cycle` test. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code; test code included inline. ✓

**Type consistency:** `build_subissue_tree(&Ticket, &HashMap<Uuid,&Ticket>) -> Vec<SubissueNode>`, `subissue_tree_{text,json,markdown}(&[SubissueNode])`, `ticket_json_with_subissues(&Ticket, &HashMap<Uuid,&Ticket>) -> Result<String, serde_json::Error>` — used identically in Tasks 3–4. `SubissueNode` field `subissues` matches JSON key and schema node. ✓

**Test helpers:** Task 1 tests reuse the existing `fn ticket(id, title, state)` helper (render.rs ~line 1341; `status` derived from `state` via `state.status()`) through a thin `sub(title, state, children)` wrapper, and add `HashMap` to the test module's `use std::collections` line. No invented fields. ✓
