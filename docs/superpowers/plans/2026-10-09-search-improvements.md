# Search Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ti` search Unicode-correct, multi-term (AND + quoted phrases, per-term scope), and highlight matches in `ti list` and `ti show`.

**Architecture:** `SearchFilter` becomes a list of `SearchTerm`s (scope + needle). Parsing tokenizes on whitespace honoring `"..."` quotes; each token may carry its own `scope:` prefix. Matching is AND across terms. Highlighting is a cosmetic render-layer concern that wraps matched substrings in bold-yellow and restores the surrounding color.

**Tech Stack:** Rust 2021, clap, crossterm (terminal width only), `assert_cmd` + `tempfile` for CLI tests.

## Global Constraints

- `gix-actor` stays pinned to `=0.40.0` — do not touch dependency versions.
- `docs/agents.md` and `crates/ticgit/docs/agents.md` must stay **byte-identical** — `build.rs` fails the build if they diverge. Edit both with the same content.
- `--json` / `--markdown` output must carry **no ANSI** and no shape change; highlighting only affects the human table/detail renderers, which are never used on the json/markdown paths.
- `docs/schema/v1.json` is unaffected (search is an input filter).
- Commit messages: write to a file and `git commit -F <file>` (never inline `-m` for multi-line / trailer bodies). Simple one-line subjects via `-m` are fine.
- No new crate dependencies.

---

### Task 1: Multi-term, Unicode `SearchFilter` (library)

**Files:**
- Modify: `crates/ticgit-lib/src/query.rs:35-158` (the `SearchFilter` / `SearchScope` block and its `impl`s)
- Modify: `crates/ticgit-lib/src/query.rs:351-353` (the `contains` helper)
- Test: `crates/ticgit-lib/src/query.rs` (existing `#[cfg(test)] mod tests`, ~line 860+)

**Interfaces:**
- Consumes: `Ticket` (`title: String`, `description: Option<String>`, `comments: Vec<Comment>` with `body: String`).
- Produces:
  - `pub struct SearchFilter { pub terms: Vec<SearchTerm> }`
  - `pub struct SearchTerm { pub scope: SearchScope, pub needle: String }`
  - `SearchFilter::parse(spec: &str) -> Result<SearchFilter, String>` (signature unchanged; now never errors but keeps `Result` for callers)
  - `SearchFilter::needles(&self) -> Vec<String>` — lowercased needles for highlighting
  - `SearchFilter::matches(&self, &Ticket) -> bool` (private, used by `apply`)

- [ ] **Step 1: Write failing library tests**

Add to the `#[cfg(test)] mod tests` block in `crates/ticgit-lib/src/query.rs`. These sit alongside the existing `search_matches_title_description_and_comments` test (which must keep passing unchanged).

```rust
#[test]
fn search_multi_term_is_and() {
    let f = SearchFilter::parse("login timeout").unwrap();
    let mut both = Ticket::new("fix login timeout on retry".into(), None);
    let only_one = Ticket::new("login screen".into(), None);
    assert!(f.matches(&both));
    assert!(!f.matches(&only_one));
    both.title = "LOGIN during TIMEOUT".into();
    assert!(f.matches(&both)); // case-insensitive, order-independent
}

#[test]
fn search_quoted_phrase_is_contiguous() {
    let f = SearchFilter::parse("\"login timeout\"").unwrap();
    let phrase = Ticket::new("fix login timeout bug".into(), None);
    let apart = Ticket::new("login on the timeout screen".into(), None);
    assert!(f.matches(&phrase));
    assert!(!f.matches(&apart));
}

#[test]
fn search_per_term_scope() {
    let f = SearchFilter::parse("title:login description:timeout").unwrap();
    let hit = Ticket::new("login page".into(), Some("timeout after 30s".into()));
    let miss_desc = Ticket::new("login page".into(), Some("no issue".into()));
    let miss_title = Ticket::new("home page".into(), Some("timeout after 30s".into()));
    assert!(f.matches(&hit));
    assert!(!f.matches(&miss_desc));
    assert!(!f.matches(&miss_title));
}

#[test]
fn search_is_unicode_case_insensitive() {
    let f = SearchFilter::parse("CITTÀ").unwrap();
    let t = Ticket::new("gestione città".into(), None);
    assert!(f.matches(&t));
}

#[test]
fn search_unknown_prefix_is_literal_needle() {
    // `foo:` is not a known scope, so the whole token is the needle (back-compat).
    let f = SearchFilter::parse("foo:bar").unwrap();
    let t = Ticket::new("see foo:bar reference".into(), None);
    assert!(f.matches(&t));
    assert_eq!(f.terms.len(), 1);
    assert_eq!(f.terms[0].scope, SearchScope::Any);
    assert_eq!(f.terms[0].needle, "foo:bar");
}

#[test]
fn search_empty_matches_everything() {
    let f = SearchFilter::parse("   ").unwrap();
    assert!(f.terms.is_empty());
    assert!(f.matches(&Ticket::new("anything".into(), None)));
}
```

> Note on `Ticket::new`: confirm the constructor's exact signature in `crates/ticgit-lib/src/ticket.rs` before running. If it is not `Ticket::new(title, description)`, adapt these test fixtures to however tickets are built in the existing `query.rs` tests (mirror the existing `search_matches_title_description_and_comments` test's construction).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ticgit-lib search_`
Expected: compile error / FAIL — `SearchFilter` has no `terms` field yet.

- [ ] **Step 3: Replace the `SearchFilter` / `SearchScope` types and impls**

Replace `crates/ticgit-lib/src/query.rs:35-158` (from `#[derive(...)] pub struct SearchFilter {` through the end of `impl SearchScope { ... }`) with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFilter {
    pub terms: Vec<SearchTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerm {
    pub scope: SearchScope,
    pub needle: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Any,
    Title,
    Description,
    Comments,
}

impl SearchFilter {
    /// Parse a search spec into AND-combined terms.
    ///
    /// Tokens split on whitespace, except a `"..."` run is one phrase token.
    /// A token may carry a leading `scope:` prefix (`title:`,
    /// `description:`/`desc:`, `comments:`/`comment:`); an unknown prefix is
    /// left as part of the needle. Needles are lowercased (full Unicode).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut terms = Vec::new();
        for token in tokenize(spec) {
            let (scope, needle) = match token.split_once(':') {
                Some((prefix, rest)) => match SearchScope::parse(prefix) {
                    Some(scope) => (scope, rest.to_string()),
                    None => (SearchScope::Any, token.clone()),
                },
                None => (SearchScope::Any, token.clone()),
            };
            let needle = needle.to_lowercase();
            if !needle.is_empty() {
                terms.push(SearchTerm { scope, needle });
            }
        }
        Ok(SearchFilter { terms })
    }

    /// Lowercased needles, for match highlighting in the renderers.
    pub fn needles(&self) -> Vec<String> {
        self.terms.iter().map(|term| term.needle.clone()).collect()
    }

    fn matches(&self, ticket: &Ticket) -> bool {
        self.terms.iter().all(|term| term.matches(ticket))
    }
}

impl SearchTerm {
    fn matches(&self, ticket: &Ticket) -> bool {
        match self.scope {
            SearchScope::Any => {
                contains(&ticket.title, &self.needle)
                    || ticket
                        .description
                        .as_deref()
                        .is_some_and(|description| contains(description, &self.needle))
                    || ticket
                        .comments
                        .iter()
                        .any(|comment| contains(&comment.body, &self.needle))
            }
            SearchScope::Title => contains(&ticket.title, &self.needle),
            SearchScope::Description => ticket
                .description
                .as_deref()
                .is_some_and(|description| contains(description, &self.needle)),
            SearchScope::Comments => ticket
                .comments
                .iter()
                .any(|comment| contains(&comment.body, &self.needle)),
        }
    }
}

impl SearchScope {
    fn parse(scope: &str) -> Option<Self> {
        match scope.trim().to_ascii_lowercase().as_str() {
            "title" => Some(SearchScope::Title),
            "description" | "desc" => Some(SearchScope::Description),
            "comment" | "comments" => Some(SearchScope::Comments),
            _ => None,
        }
    }
}

/// Split a search spec into tokens: whitespace separates, `"..."` groups a
/// phrase (quote chars are dropped, inner whitespace preserved). An unclosed
/// quote runs to end of input.
fn tokenize(spec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for ch in spec.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            has_token = true;
        } else if ch.is_whitespace() && !in_quotes {
            if has_token {
                tokens.push(std::mem::take(&mut cur));
                has_token = false;
            }
        } else {
            cur.push(ch);
            has_token = true;
        }
    }
    if has_token {
        tokens.push(cur);
    }
    tokens
}
```

- [ ] **Step 4: Make `contains` Unicode-correct**

Replace `crates/ticgit-lib/src/query.rs:351-353`:

```rust
fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}
```

(The needle is already lowercased in `parse`; lowercasing the haystack with full-Unicode `to_lowercase()` is what fixes `CITTÀ`/`città`.)

- [ ] **Step 5: Run the whole library test suite**

Run: `cargo test -p ticgit-lib`
Expected: PASS — new `search_*` tests green, and the pre-existing `search_matches_title_description_and_comments` still green.

- [ ] **Step 6: Commit**

```bash
git add crates/ticgit-lib/src/query.rs
git commit -m "feat(search): multi-term AND, quoted phrases, per-term scope, Unicode fold"
```

---

### Task 2: Match highlighting in the renderers

**Files:**
- Modify: `crates/ticgit/src/render.rs` — add `ANSI_BOLD`, add `highlight()`, add a `needles: &[String]` param to `tickets_table_with_refs`, `tickets_table_with_width`, and `ticket_detail`; update their internal callers (`tickets_table`) and the 4 in-file test callers of `tickets_table_with_width`.
- Test: `crates/ticgit/src/render.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `SearchFilter::needles()` output shape (`&[String]`, lowercased) from Task 1 — but this task only needs the `&[String]` type; callers pass real needles in Tasks 3 and 4.
- Produces:
  - `fn highlight(cell: &str, needles: &[String], base: &str) -> String`
  - `pub fn tickets_table_with_refs(tickets, current, ref_lengths, nicks, needles: &[String]) -> String`
  - `pub fn ticket_detail(t, nicks, rels, needles: &[String]) -> String`

- [ ] **Step 1: Write a failing render test for highlighting**

Add to `crates/ticgit/src/render.rs`'s `#[cfg(test)] mod tests`. `strip_ansi` already exists in that module (line ~1592).

```rust
#[test]
fn list_highlights_search_term_in_title() {
    let ticket = make_ticket("abc", "fix login timeout", TicketState::New);
    let refs = open_ticket_ref_lengths(&[ticket.clone()]);
    let needles = vec!["login".to_string()];

    let colored = tickets_table_with_width(
        &[ticket.clone()],
        None,
        &refs,
        100,
        OffsetDateTime::UNIX_EPOCH,
        None,
        &needles,
    );
    // Bold + yellow escape is present around the match...
    assert!(colored.contains("\x1b[1m\x1b[33m"));
    // ...and the plain text is still intact after stripping ANSI.
    assert!(strip_ansi(&colored).contains("fix login timeout"));

    // No needles => no highlight escape injected.
    let plain = tickets_table_with_width(
        &[ticket],
        None,
        &refs,
        100,
        OffsetDateTime::UNIX_EPOCH,
        None,
        &[],
    );
    assert!(!plain.contains("\x1b[1m\x1b[33m"));
}
```

> `make_ticket` is the existing helper used by the other render tests (it builds a `Ticket` with an id prefix, title, and state). Confirm its exact name/signature at the top of the render `mod tests` and match it; if the existing tests use a different constructor, mirror that.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p ticgit --lib list_highlights_search_term_in_title`
Expected: compile error — `tickets_table_with_width` takes 6 args, not 7; `highlight` undefined.

- [ ] **Step 3: Add `ANSI_BOLD` and the `highlight` helper**

In `crates/ticgit/src/render.rs`, next to the other `const ANSI_*` (around line 216-222) add:

```rust
const ANSI_BOLD: &str = "\x1b[1m";
```

Add the helper near `fn ansi` (around line 1203):

```rust
/// Wrap each case-insensitive occurrence of a needle in bold yellow, then
/// re-open `base` so the surrounding column color survives the reset.
///
/// Highlighting is cosmetic: if lowercasing changes the byte length of `cell`
/// (rare — e.g. `İ`), byte offsets from the lowercased copy can't be mapped
/// back safely, so we skip highlighting and return the cell unchanged. Search
/// matching itself (Task 1) is unaffected.
fn highlight(cell: &str, needles: &[String], base: &str) -> String {
    if needles.is_empty() {
        return cell.to_string();
    }
    let lower = cell.to_lowercase();
    if lower.len() != cell.len() {
        return cell.to_string();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(pos) = lower[from..].find(needle.as_str()) {
            let start = from + pos;
            let end = start + needle.len();
            ranges.push((start, end));
            from = end;
        }
    }
    if ranges.is_empty() {
        return cell.to_string();
    }

    ranges.sort_by_key(|&(start, _)| start);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    let mut out = String::new();
    let mut cursor = 0;
    for (start, end) in merged {
        out.push_str(&cell[cursor..start]);
        out.push_str(ANSI_BOLD);
        out.push_str(ANSI_YELLOW);
        out.push_str(&cell[start..end]);
        out.push_str(ANSI_RESET);
        out.push_str(base);
        cursor = end;
    }
    out.push_str(&cell[cursor..]);
    out
}
```

- [ ] **Step 4: Thread `needles` through the table renderers**

In `tickets_table` (line ~225-228) pass an empty slice:

```rust
pub fn tickets_table(tickets: &[Ticket], current: Option<&uuid::Uuid>) -> String {
    let ref_lengths = open_ticket_ref_lengths(tickets);
    tickets_table_with_refs(tickets, current, &ref_lengths, None, &[])
}
```

Update `tickets_table_with_refs` (line ~231) to accept and forward `needles`:

```rust
pub fn tickets_table_with_refs(
    tickets: &[Ticket],
    current: Option<&uuid::Uuid>,
    ref_lengths: &BTreeMap<uuid::Uuid, usize>,
    nicks: Option<&NickMap>,
    needles: &[String],
) -> String {
    let width = crossterm::terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(100)
        .max(40);
    tickets_table_with_width(
        tickets,
        current,
        ref_lengths,
        width,
        OffsetDateTime::now_utc(),
        nicks,
        needles,
    )
}
```

Add `needles: &[String]` as the final param of `tickets_table_with_width` (line ~386-392):

```rust
fn tickets_table_with_width(
    tickets: &[Ticket],
    current: Option<&uuid::Uuid>,
    ref_lengths: &BTreeMap<uuid::Uuid, usize>,
    width: usize,
    now: OffsetDateTime,
    nicks: Option<&NickMap>,
    needles: &[String],
) -> String {
```

In the title-rendering block (lines ~463-472), highlight the fitted cell **after** `fit()` (so width math is untouched) and use `ANSI_BLUE` as the base to restore:

```rust
        if t.children.is_empty() {
            let cell = fit(&flatten(&t.title), layout.title_width);
            out.push_str(&ansi(ANSI_BLUE, &highlight(&cell, needles, ANSI_BLUE)));
        } else {
            let suffix = format!(" [+{}]", t.children.len());
            let avail = layout.title_width.saturating_sub(suffix.len());
            let cell = fit(&flatten(&t.title), avail);
            out.push_str(&ansi(ANSI_BLUE, &highlight(&cell, needles, ANSI_BLUE)));
            out.push_str(&ansi(ANSI_DIM, &fit(&suffix, suffix.len())));
        }
```

- [ ] **Step 5: Thread `needles` through `ticket_detail`**

Add `needles: &[String]` as the final param of `ticket_detail` (line ~587). Highlight the title, description body, and comment bodies. For the title, base is `ANSI_BLUE`; for the uncolored description/comment bodies, base is `""`.

Title field (line ~591):

```rust
    out.push_str(&detail_field(
        "Title",
        &ansi(ANSI_BLUE, &highlight(&t.title, needles, ANSI_BLUE)),
    ));
```

Description body (the `Some(description) =>` arm, ~line 669):

```rust
        Some(description) => {
            out.push('\n');
            let body = description.replace('\n', "\n  ");
            out.push_str(&format!("  {}\n", highlight(&body, needles, "")));
        }
```

Comment body (the `for c in &t.comments` loop):

```rust
        for c in &t.comments {
            let body = c.body.replace('\n', "\n  ");
            out.push_str(&format!(
                "\n{} {} {}\n  {}\n",
                ansi(ANSI_CYAN, &display_name(&c.author, nicks)),
                ansi(ANSI_DIM, "-"),
                ansi(ANSI_DIM, &c.at.format(&Rfc3339).unwrap_or_default()),
                highlight(&body, needles, ""),
            ));
        }
```

- [ ] **Step 6: Fix the 4 in-file test callers of `tickets_table_with_width`**

In `crates/ticgit/src/render.rs`'s `mod tests`, the 4 calls at lines ~1328, 1356, 1370, 1392 each end with `None,` as the last arg — add `&[]` after it:

```rust
        let table = strip_ansi(&tickets_table_with_width(
            &[ticket],
            None,
            &refs,
            100,
            OffsetDateTime::UNIX_EPOCH,
            None,
            &[],
        ));
```

Apply the same `&[],` addition to all four call sites (one may use width `60`/`40` — keep those values, only add the trailing `&[]`).

- [ ] **Step 7: Run render tests**

Run: `cargo test -p ticgit --lib`
Expected: PASS — new highlight test green, all existing render tests green.

- [ ] **Step 8: Commit**

```bash
git add crates/ticgit/src/render.rs
git commit -m "feat(search): highlight matched terms in list and show renderers"
```

---

### Task 3: Wire needles into `ti list`

**Files:**
- Modify: `crates/ticgit/src/commands/list.rs:45-47` (the `--search` doc comment) and `:264-...` (the `tickets_table_with_refs` call)

**Interfaces:**
- Consumes: `SearchFilter::parse` / `SearchFilter::needles` (Task 1), `tickets_table_with_refs(..., needles)` (Task 2).
- Produces: nothing new.

- [ ] **Step 1: Update the `--search` help text**

Replace the doc comment at `crates/ticgit/src/commands/list.rs:45`:

```rust
    /// Search title, description, and comments. Space-separated terms must all
    /// match (AND); quote a "phrase" to match it whole. Prefix a term with
    /// `title:`, `description:`, or `comments:` to scope just that term.
    #[arg(long = "search")]
    pub search: Option<String>,
```

- [ ] **Step 2: Compute needles and pass them to the table**

The `search` filter is parsed at `list.rs:162-165` and moved into `Filter` at `:195`. Derive the needle list before that move. Right after the `let search = match args.search.as_deref() { ... };` block (ends ~line 165), add:

```rust
    let search_needles = search
        .as_ref()
        .map(|filter| filter.needles())
        .unwrap_or_default();
```

Then update the `render::tickets_table_with_refs(` call (starts line ~264) to pass `&search_needles` as the new final argument:

```rust
    let mut table = render::tickets_table_with_refs(
        &tickets,
        current.as_ref(),
        &open_ref_lengths,
        Some(&nicks),
        &search_needles,
    );
```

> Confirm the current argument list of that call and append `&search_needles` as the last arg, matching the new `tickets_table_with_refs` signature from Task 2. `search` is consumed by `Filter` at line ~195; since `needles()` is computed into `search_needles` *before* that move, there is no borrow conflict.

- [ ] **Step 3: Build and smoke-test by hand**

Run:
```bash
cargo build -p ticgit
```
Expected: compiles. (Behavioral assertions live in Task 5.)

- [ ] **Step 4: Commit**

```bash
git add crates/ticgit/src/commands/list.rs
git commit -m "feat(search): highlight list results and document multi-term syntax"
```

---

### Task 4: Add `--search` to `ti show`

**Files:**
- Modify: `crates/ticgit/src/commands/show.rs` — add the `--search` arg, parse it to needles, pass to `ticket_detail`.

**Interfaces:**
- Consumes: `ticgit_lib::SearchFilter` (`parse` + `needles`), `render::ticket_detail(..., needles)` (Task 2).
- Produces: nothing new.

- [ ] **Step 1: Add the `--search` flag to `show::Args`**

In `crates/ticgit/src/commands/show.rs`, add to the `Args` struct (after `markdown`, before/after `filter`):

```rust
    /// Highlight matches for this search spec (same syntax as `ti list --search`).
    #[arg(long = "search")]
    pub search: Option<String>,
```

- [ ] **Step 2: Parse needles and pass them to `ticket_detail`**

In `run`, compute the needle list near the top (after `let ticket = store.load(&id)?;`):

```rust
    let needles = match args.search.as_deref() {
        Some(spec) => ticgit_lib::SearchFilter::parse(spec)
            .map_err(|e| anyhow::anyhow!(e))?
            .needles(),
        None => Vec::new(),
    };
```

Update the human-output `ticket_detail` call (currently `render::ticket_detail(&ticket, Some(&nicks), Some(&rels))`, line ~66) to:

```rust
            render::ticket_detail(&ticket, Some(&nicks), Some(&rels), &needles)
```

Leave the `--json`, `--markdown`, and `--filter` branches untouched (they must stay ANSI-free; they never call `ticket_detail`).

- [ ] **Step 3: Build**

Run: `cargo build -p ticgit`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/ticgit/src/commands/show.rs
git commit -m "feat(search): add --search to ti show for match highlighting"
```

---

### Task 5: CLI integration tests + docs/help/agent-guide

**Files:**
- Test: `crates/ticgit/tests/cli.rs` (new tests near the existing search test, ~line 2114)
- Modify: `docs/agents.md` **and** `crates/ticgit/docs/agents.md` (identical edits)
- Modify: `docs/docs/creating-tickets.html`

**Interfaces:**
- Consumes: the finished CLI from Tasks 1-4.

- [ ] **Step 1: Write failing CLI integration tests**

Append to `crates/ticgit/tests/cli.rs` (uses the existing `TestRepo`, `create_ticket`, and comment patterns shown around line 2040-2114):

```rust
#[test]
fn list_search_multi_term_and() {
    let repo = TestRepo::new();
    let both = create_ticket(&repo, "fix login timeout on retry");
    let _one = create_ticket(&repo, "login screen polish");

    let output = repo
        .ti()
        .args(["list", "--search", "login timeout", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], both);
}

#[test]
fn list_search_quoted_phrase() {
    let repo = TestRepo::new();
    let phrase = create_ticket(&repo, "fix login timeout bug");
    let _apart = create_ticket(&repo, "login on the timeout screen");

    let output = repo
        .ti()
        .args(["list", "--search", "\"login timeout\"", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], phrase);
}

#[test]
fn list_search_is_unicode_case_insensitive() {
    let repo = TestRepo::new();
    let hit = create_ticket(&repo, "gestione città");

    let output = repo
        .ti()
        .args(["list", "--search", "CITTÀ", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], hit);
}

#[test]
fn list_search_highlights_in_text_output_but_not_json() {
    let repo = TestRepo::new();
    create_ticket(&repo, "fix login timeout");

    // Human table output carries the bold+yellow highlight escape.
    let text = repo
        .ti()
        .args(["list", "--search", "login"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("\u{1b}[1m\u{1b}[33m"));

    // JSON output is ANSI-free.
    let json = repo
        .ti()
        .args(["list", "--search", "login", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json = String::from_utf8(json).unwrap();
    assert!(!json.contains('\u{1b}'));
}

#[test]
fn show_search_highlights_detail() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "fix login timeout");

    let text = repo
        .ti()
        .args(["show", &id, "--search", "login"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("\u{1b}[1m\u{1b}[33m"));

    // show --json stays ANSI-free even with --search.
    let json = repo
        .ti()
        .args(["show", &id, "--search", "login", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json = String::from_utf8(json).unwrap();
    assert!(!json.contains('\u{1b}'));
}
```

> `create_ticket` returns the ticket id string (see its definition at `cli.rs:66`). If `list` defaults to open-only and hides results, these fixtures are all open/new so they appear by default. The highlight test relies on the renderer emitting color even to a pipe (there is no TTY gating in `render.rs`); if that ever changes, this test is the canary.

- [ ] **Step 2: Run the new CLI tests to verify they pass**

Run: `cargo test -p ticgit --test cli list_search_`
Then: `cargo test -p ticgit --test cli show_search_`
Expected: PASS. If any fail, fix the implementation (not the assertions) before continuing.

- [ ] **Step 3: Update the agent guide (BOTH copies, identical)**

In **both** `docs/agents.md` and `crates/ticgit/docs/agents.md`, replace the two search example lines (at `docs/agents.md:45-46`) inside the `## Finding And Browsing` fenced block:

Replace:
```sh
ti list --search "parser" --markdown
ti list --search "title:timeout" --markdown
```
With:
```sh
ti list --search "parser recovery" --markdown      # all terms must match (AND)
ti list --search "\"exact phrase\"" --markdown      # quote to match a phrase
ti list --search "title:timeout comments:retry" --markdown   # per-term scope
```

Make the edit byte-identical in both files.

- [ ] **Step 4: Verify the agent-guide sync check passes**

Run: `cargo build`
Expected: builds clean. (If `build.rs` reports the guides diverged, the two edits are not identical — reconcile them.)

- [ ] **Step 5: Update the website docs**

In `docs/docs/creating-tickets.html`, in the `<h3>Search</h3>` block (lines ~160-170), update the dim description line and add multi-term examples. Replace:

```html
            <div class="line"><span class="prompt">$</span> ti list --search "parser"</div>
            <div class="line dim">Searches title, description, and comments.</div>
            <div class="line">&nbsp;</div>
            <div class="line"><span class="prompt">$</span> ti list --search "title:parser"</div>
            <div class="line"><span class="prompt">$</span> ti list --search "comments:workaround"</div>
            <div class="line"><span class="prompt">$</span> ti list --search "description:migration"</div>
```
With:
```html
            <div class="line"><span class="prompt">$</span> ti list --search "parser recovery"</div>
            <div class="line dim">Searches title, description, and comments. All terms must match (AND).</div>
            <div class="line">&nbsp;</div>
            <div class="line"><span class="prompt">$</span> ti list --search "&quot;exact phrase&quot;"</div>
            <div class="line dim">Quote to match a phrase; matches are highlighted in the results.</div>
            <div class="line">&nbsp;</div>
            <div class="line"><span class="prompt">$</span> ti list --search "title:parser"</div>
            <div class="line"><span class="prompt">$</span> ti list --search "comments:workaround"</div>
            <div class="line"><span class="prompt">$</span> ti list --search "description:migration"</div>
```

- [ ] **Step 6: Full verification sweep**

Run:
```bash
cargo fmt
cargo clippy --all-targets
cargo test
cargo run -p ticgit -- list --help
cargo run -p ticgit -- show --help
```
Expected: fmt clean, no new clippy warnings, all tests pass, both help screens show the updated `--search` description.

- [ ] **Step 7: Commit**

```bash
git add crates/ticgit/tests/cli.rs docs/agents.md crates/ticgit/docs/agents.md docs/docs/creating-tickets.html
git commit -m "test(search): cover multi-term/phrase/unicode/highlight; update docs"
```

---

## Self-Review Notes

- **Spec coverage:** Unicode fold → Task 1 Step 4 + tests. Multi-term AND → Task 1. Quoted phrases → Task 1 `tokenize`. Per-term scope → Task 1 `parse`. Highlight list+show → Task 2 + wiring Tasks 3/4. `--search` on `show` → Task 4. Help/docs/agent-guide (both copies) → Task 3 Step 1, Task 5 Steps 3-5. Schema untouched → Global Constraints.
- **No-ANSI-in-JSON:** enforced structurally (json/markdown paths never call the highlighting renderers) and asserted in Task 5 Steps 1-2.
- **Nested-ANSI risk:** handled by highlighting *after* `fit()` and restoring `base` color; length-changing lowercase bails out (Task 2 `highlight`).
- **Type consistency:** `needles: &[String]` used identically across `highlight`, `tickets_table_with_refs`, `tickets_table_with_width`, `ticket_detail`, and the two command call sites. `SearchFilter::needles()` is the single producer.
