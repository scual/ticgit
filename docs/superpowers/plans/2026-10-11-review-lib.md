# Move Code Review into ticgit-lib Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move code review domain logic and storage from `crates/ticgit` into `crates/ticgit-lib`, establish a VCS adapter seam (`ReviewVcs` with `RealVcsAdapter` and `FakeVcsAdapter`), cover review rules with lib tests, and make CLI and TUI thin callers with zero raw review metadata keys in `crates/ticgit`.

**Architecture:** A new module `crates/ticgit-lib/src/review.rs` defines the `Review` model, `ReviewVcs` trait, `RealVcsAdapter` (using `git` and `but`), and `FakeVcsAdapter`. Review operations are exposed via `TicketStore`. CLI (`ti review`) and TUI (`tui.rs`) delegate all review mutations and queries to `TicketStore` and `ReviewVcs`, deleting duplicated helpers.

**Tech Stack:** Rust (2021 edition), `git-meta-lib`, `gix`, `serde`, `serde_json`, `time`, `uuid`.

## Global Constraints

- No raw review metadata keys written from `crates/ticgit`.
- CLI `--json` output and schema remain 100% backward-compatible.
- Existing CLI integration tests (`crates/ticgit/tests/cli.rs`) pass unchanged.
- TUI review behavior and formatting remain unchanged.
- Review rules (approvals, revision change history, all-approved) covered by tests in `crates/ticgit-lib` using `test_store()` and `FakeVcsAdapter`.

---

### Task 1: Review Keys and Domain Models in `crates/ticgit-lib`

**Files:**
- Modify: `crates/ticgit-lib/src/keys.rs`
- Create: `crates/ticgit-lib/src/review.rs`
- Modify: `crates/ticgit-lib/src/lib.rs`

**Interfaces:**
- Produces:
  - Key constants in `keys.rs`: `REVIEW_INDEX_KEY`, `REVIEW_REVISIONS_KEY`, `REVIEW_REVISION_HISTORY_KEY`, `REVIEW_MESSAGES_KEY`, `REVIEW_REVIEWERS_KEY`, `REVIEW_APPROVALS_KEY`, `REVIEW_REVIEWED_KEY`, `REVIEW_CREATED_AT_KEY`, `REVIEW_CREATED_BY_KEY`, `REVIEW_MERGED_AT_KEY`, `REVIEW_MERGED_BY_KEY`
  - Domain structs in `review.rs`: `Review`, `ReviewRevisionChange`, `ReviewMessage`, `CommitReviewStatus`, `ReviewProgress`, `ReviewBranchSnapshot`, `ReviewBranchChoice`, `ReviewCommitInfo`, `NewReviewOpts`
  - Re-exports in `lib.rs`: `pub use review::*;`

- [ ] **Step 1: Write failing test in `crates/ticgit-lib/src/keys.rs` for review key constants**

Add a test in `crates/ticgit-lib/src/keys.rs::tests`:
```rust
#[test]
fn review_keys_are_consistent() {
    assert_eq!(REVIEW_INDEX_KEY, "review:branches");
    assert_eq!(REVIEW_REVISIONS_KEY, "review:revisions");
    assert_eq!(REVIEW_REVISION_HISTORY_KEY, "review:revision-history");
    assert_eq!(REVIEW_MESSAGES_KEY, "review:messages");
    assert_eq!(REVIEW_REVIEWERS_KEY, "review:reviewers");
    assert_eq!(REVIEW_APPROVALS_KEY, "review:approvals");
    assert_eq!(REVIEW_REVIEWED_KEY, "review:reviewed");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ticgit-lib keys::tests::review_keys_are_consistent`
Expected: FAIL with undeclared constant errors.

- [ ] **Step 3: Define constants in `keys.rs`**

Add constants to `crates/ticgit-lib/src/keys.rs`:
```rust
pub const REVIEW_INDEX_KEY: &str = "review:branches";
pub const REVIEW_REVISIONS_KEY: &str = "review:revisions";
pub const REVIEW_REVISION_HISTORY_KEY: &str = "review:revision-history";
pub const REVIEW_MESSAGES_KEY: &str = "review:messages";
pub const REVIEW_REVIEWERS_KEY: &str = "review:reviewers";
pub const REVIEW_APPROVALS_KEY: &str = "review:approvals";
pub const REVIEW_REVIEWED_KEY: &str = "review:reviewed";
pub const REVIEW_CREATED_AT_KEY: &str = "review:created-at";
pub const REVIEW_CREATED_BY_KEY: &str = "review:created-by";
pub const REVIEW_MERGED_AT_KEY: &str = "review:merged-at";
pub const REVIEW_MERGED_BY_KEY: &str = "review:merged-by";
```

- [ ] **Step 4: Create `crates/ticgit-lib/src/review.rs` with models and serialization tests**

Define:
```rust
use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRevisionChange {
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewMessage {
    pub author: String,
    pub body: String,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub branch_id: String,
    pub branch_name: Option<String>,
    pub ticket_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub status: String,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub integration_sha: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub reviewers: Vec<String>,
    pub revisions: Vec<ReviewRevisionChange>,
    pub revision_history: Vec<ReviewRevisionChange>,
    pub messages: Vec<ReviewMessage>,
}

impl Review {
    #[must_use]
    pub fn branch(&self) -> &str {
        self.branch_name.as_deref().unwrap_or(&self.branch_id)
    }

    #[must_use]
    pub fn commits(&self) -> Vec<String> {
        if !self.revisions.is_empty() {
            self.revisions.iter().map(|entry| entry.sha.clone()).collect()
        } else {
            self.head_sha.iter().cloned().collect()
        }
    }
}
```
Include parsing and formatting helpers:
- `format_review_revision(&ReviewRevisionChange) -> String`
- `parse_review_revision_change(&str) -> Option<ReviewRevisionChange>`
- `review_progress_label(approved: usize, total: usize) -> String`

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ticgit-lib review`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ticgit-lib/src/keys.rs crates/ticgit-lib/src/review.rs crates/ticgit-lib/src/lib.rs
git commit -m "feat(lib): add review models, key constants, and formatting helpers"
```

---

### Task 2: VCS Adapter Seam (`ReviewVcs`, `RealVcsAdapter`, `FakeVcsAdapter`)

**Files:**
- Create: `crates/ticgit-lib/src/review/vcs.rs` (or within `crates/ticgit-lib/src/review.rs`)
- Test: unit tests in `crates/ticgit-lib/src/review.rs`

**Interfaces:**
- Produces:
  - Trait `ReviewVcs`:
    ```rust
    pub trait ReviewVcs: Send + Sync {
        fn current_branch(&self) -> crate::Result<String>;
        fn resolve_ref(&self, reference: &str) -> crate::Result<String>;
        fn default_base_ref(&self) -> String;
        fn merge_base(&self, base_ref: &str, branch_name: &str) -> crate::Result<String>;
        fn resolve_branch_head(&self, branch_name: &str) -> crate::Result<String>;
        fn revision_list(&self, base_sha: &str, head_sha: &str) -> crate::Result<Vec<String>>;
        fn commit_change_id(&self, sha: &str) -> Option<String>;
        fn commit_patch_id(&self, sha: &str) -> Option<String>;
        fn remote_url(&self) -> crate::Result<Option<String>>;
        fn load_branch_snapshot(&self, branch_name: &str) -> crate::Result<ReviewBranchSnapshot>;
        fn load_branch_choices(&self, connected_branches: &BTreeSet<String>) -> crate::Result<Vec<ReviewBranchChoice>>;
        fn commit_info(&self, sha: &str) -> ReviewCommitInfo;
        fn commit_patch_lines(&self, sha: &str) -> Vec<String>;
        fn changed_file_count(&self, oldest: &str, head: &str) -> usize;
    }
    ```
  - Struct `RealVcsAdapter`: implements `ReviewVcs` using Git subprocesses and `but` JSON commands.
  - Struct `FakeVcsAdapter`: in-memory mock implementation for testing without git.

- [ ] **Step 1: Write failing test verifying `FakeVcsAdapter` satisfies `ReviewVcs`**

Write a test checking that a `FakeVcsAdapter` returns configured branches, revisions, patch-ids, change-ids, and snapshots.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ticgit-lib review_vcs_fake`
Expected: FAIL

- [ ] **Step 3: Implement `ReviewVcs`, `FakeVcsAdapter`, and `RealVcsAdapter`**

Move git/but execution logic from `review.rs` / `tui.rs` into `RealVcsAdapter`.
Implement `FakeVcsAdapter` with in-memory map backing.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ticgit-lib`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ticgit-lib/src/review.rs
git commit -m "feat(lib): add ReviewVcs trait, RealVcsAdapter, and FakeVcsAdapter"
```

---

### Task 3: Review Operations on `TicketStore` with Comprehensive Lib Tests

**Files:**
- Modify: `crates/ticgit-lib/src/store.rs`
- Test: `crates/ticgit-lib/src/store.rs` (unit tests)

**Interfaces:**
- Consumes: `Review`, `ReviewRevisionChange`, `ReviewMessage`, `ReviewVcs`, `keys`
- Produces methods on `TicketStore`:
  - `create_review(&self, opts: &NewReviewOpts, vcs: &impl ReviewVcs) -> Result<Review>`
  - `list_reviews(&self) -> Result<Vec<Review>>`
  - `load_review(&self, branch_id: &str) -> Result<Review>`
  - `resolve_review_branch_id(&self, explicit: Option<&str>, vcs: Option<&impl ReviewVcs>) -> Result<String>`
  - `load_ticket_reviews(&self, tickets: &[Ticket]) -> Result<std::collections::HashMap<Uuid, Review>>`
  - `add_reviewer(&self, branch_id: &str, email: &str) -> Result<()>`
  - `add_review_comment(&self, branch_id: &str, message: ReviewMessage) -> Result<()>`
  - `request_changes(&self, branch_id: &str, message: ReviewMessage) -> Result<()>`
  - `approve_review_commit(&self, branch_id: Option<&str>, sha: &str, body: &str) -> Result<bool>`
  - `approve_review(&self, branch_id: &str, body: &str, commit: Option<String>) -> Result<()>`
  - `update_review(&self, branch_id: &str, head: Option<&str>, vcs: &impl ReviewVcs) -> Result<(Review, String)>`
  - `integrate_review(&self, branch_id: &str, integration_sha: &str) -> Result<()>`
  - `close_review(&self, branch_id: &str) -> Result<()>`
  - `reopen_review(&self, branch_id: &str) -> Result<()>`
  - `set_review_details(&self, branch_id: &str, title: &str, description: Option<&str>) -> Result<()>`
  - `commit_review_status(&self, sha: &str) -> CommitReviewStatus`
  - `review_commits_all_approved(&self, review: &Review, approved_sha: Option<&str>) -> bool`
  - `review_progress(&self, commits: &[String], messages: &[ReviewMessage]) -> ReviewProgress`
  - `ensure_commit_patch_id(&self, sha: &str, vcs: &impl ReviewVcs) -> Result<Option<String>>`

- [ ] **Step 1: Write failing tests for Review rules in `store.rs` using `test_store()` and `FakeVcsAdapter`**

Cover:
1. `create_review` creates review metadata, links ticket, indexes review.
2. `update_review` updates head, appends to revision history with patch_id and change_id, keeps cumulative history.
3. `approve_review_commit` approves individual commit, checks `all_approved` rule to mark review approved when all commits are approved.
4. `request_changes` sets review status to `changes-requested`.
5. `close_review` and `reopen_review` lifecycle transitions.
6. `integrate_review` sets `status = "merged"` and integration SHA.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ticgit-lib store::tests::review_`
Expected: FAIL

- [ ] **Step 3: Implement methods in `store.rs`**

Implement each method using `keys::` constants and `Target::branch`, `Target::commit`, `Target::project`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ticgit-lib store::tests::review_`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ticgit-lib/src/store.rs
git commit -m "feat(lib): implement review operations on TicketStore with FakeVcsAdapter tests"
```

---

### Task 4: Refactor CLI `review.rs` to Call `ticgit-lib` and Delete Duplicated Logic

**Files:**
- Modify: `crates/ticgit/src/commands/review.rs`
- Test: `cargo test -p ticgit --test cli review_cli_records_branch_review_flow`

**Interfaces:**
- Consumes: `ticgit_lib::review::*`, `ticgit_lib::RealVcsAdapter`, `TicketStore` review methods.
- Preserves: `ReviewOutput` JSON format and table/markdown rendering.
- Deletes: raw metadata calls, git subprocess execution, duplicated revision change parsing/formatting.

- [ ] **Step 1: Replace raw metadata writes and git commands in `crates/ticgit/src/commands/review.rs` with `ticgit-lib` calls**

Update:
- `new` -> `store.create_review(&opts, &vcs)`
- `list` -> `store.list_reviews()`
- `show` -> `store.load_review(&branch_id)`
- `add_reviewer` -> `store.add_reviewer(&branch_id, &email)`
- `comment` -> `store.add_review_comment(&branch_id, message)`
- `approve` -> `store.approve_review_commit(...)` / `store.approve_review(...)`
- `request_changes` -> `store.request_changes(&branch_id, message)`
- `update` -> `store.update_review(&branch_id, head, &vcs)`
- `integrate` -> `store.integrate_review(&branch_id, &sha)`
Remove duplicated helper functions from `review.rs`.

- [ ] **Step 2: Run CLI integration test**

Run: `cargo test -p ticgit --test cli review_cli_records_branch_review_flow`
Expected: PASS

- [ ] **Step 3: Run full CLI test suite**

Run: `cargo test -p ticgit --test cli`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/ticgit/src/commands/review.rs
git commit -m "refactor(cli): delegate review commands to ticgit-lib"
```

---

### Task 5: Refactor TUI `tui.rs` to Call `ticgit-lib` and Delete Duplicated Helpers

**Files:**
- Modify: `crates/ticgit/src/commands/tui.rs`

**Interfaces:**
- Consumes: `ticgit_lib::review::*`, `TicketStore` review methods.
- Deletes:
  - Duplicated helpers: `create_review_branch_id`, `refresh_review_revisions_from_commits`, `append_review_revision_changes`, `review_revision_change_for_commit`, `format_review_revision`, `parse_review_revision_change`, `review_revision_list`, `review_base_sha`, `default_review_base_ref`, `ensure_commit_patch_id`, `commit_patch_id`, `commit_change_id`, `commit_review_status`, `review_commits_all_approved`.
  - Raw git-meta metadata writes across all TUI review methods.

- [ ] **Step 1: Update TUI review actions to call `ticgit-lib`**

Update:
- `create_review_for_ticket` -> `store.create_review(&opts, &vcs)`
- `update_selected_review_from_branch` -> `store.update_review(...)`
- `approve_review_commit` -> `store.approve_review_commit(...)`
- `capture_review_message_in_editor` (comment / changes-requested) -> `store.add_review_comment` / `store.request_changes`
- `close_pending_review` -> `store.close_review`
- `reopen_selected_review` -> `store.reopen_review`
- edit review title/description in editor -> `store.set_review_details`
- `load_ticket_reviews` -> `store.load_ticket_reviews(&self.all_tickets)`
- Delete duplicated helper functions from `tui.rs`.

- [ ] **Step 2: Verify binary compilation and tests**

Run: `cargo test -p ticgit`
Expected: PASS

- [ ] **Step 3: Verify zero raw review metadata keys written in `crates/ticgit`**

Run check:
```bash
git grep 'Target::branch' crates/ticgit
git grep 'Target::commit' crates/ticgit
```
Confirm no raw writes to review keys remain in `crates/ticgit`.

- [ ] **Step 4: Commit**

```bash
git add crates/ticgit/src/commands/tui.rs
git commit -m "refactor(tui): delegate review operations to ticgit-lib and delete duplicated helpers"
```

---

### Task 6: Workspace Verification and Quality Gates

**Files:**
- Entire workspace

- [ ] **Step 1: Run linter**

Run: `cargo clippy --all-targets`
Expected: No errors or warnings.

- [ ] **Step 2: Run code formatter**

Run: `cargo fmt --check`
Expected: Clean formatting.

- [ ] **Step 3: Run full workspace test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 4: Commit any cleanup**

```bash
git commit -am "chore: formatting and clippy cleanup"
```

---

### Task 7: Final Review and Verification

- [ ] **Step 1: Check ticket acceptance criteria against deliverables**
- [ ] **Step 2: Open code review via `ti review`**
- [ ] **Step 3: Update ticket comment and record completion**
