# Recursive sub-issues in `ti next` and `ti show`

**Date:** 2026-10-01
**Status:** Approved (pending spec review)

## Goal

When the workable/displayed ticket has open sub-issues, show them — recursively,
across multiple nesting levels — in `ti next` and `ti show`, in all three output
modes (plain text, JSON, Markdown).

## Decisions

- **Per-child data:** short id + title + specific `state` (e.g. `new`,
  `in-progress`). Not full ticket detail.
- **Formats:** plain text, JSON, Markdown — all three.
- **Closed children:** excluded. A closed sub-issue is pruned along with its
  entire subtree (open-only tree).
- **Scope:** `ti next` and `ti show`. `ti list` is unchanged.
- **JSON shape:** additive. The emitted **ticket object is unchanged** (schema v1
  top-level shape intact; `children` UUID array stays). A new field
  **`subissues`** is injected onto the ticket object holding the nested tree.

## Architecture

Both commands already load the full ticket set (`store.list()`), so recursion
needs no extra reads.

### Shared tree builder (`render.rs`)

```rust
pub struct SubissueNode {
    pub id: Uuid,
    pub title: String,
    pub state: String,            // specific state of the child
    pub subissues: Vec<SubissueNode>,
}

/// Build the open-only, recursive sub-issue tree for `root`.
/// `by_id` maps every ticket id to its ticket. Closed children (and their
/// subtrees) are omitted. A visited set guards against cycles; child ids absent
/// from `by_id` are skipped.
pub fn build_subissue_tree(
    root: &Ticket,
    by_id: &HashMap<Uuid, &Ticket>,
) -> Vec<SubissueNode>;
```

- Walks `ticket.children` in their existing `BTreeSet` order (stable, sorted).
- Skips any child with `status == Closed`.
- `visited: HashSet<Uuid>` seeded with the root id; each node added before
  recursing. Prevents infinite loops on malformed parent/child cycles.
- Missing ids (dangling child reference) are silently skipped.

### Renderers (`render.rs`)

Three helpers consume `&[SubissueNode]`:

1. **`subissue_tree_text(nodes) -> String`** — indented 2 spaces per level,
   empty string when `nodes` is empty. Caller prepends a `Sub-issues:` header
   only when non-empty.
   ```
   Sub-issues:
     a1b2c3 in-progress  Wire up parser
       d4e5f6 new        Handle edge case
     g7h8i9 assigned     Write tests
   ```
2. **`subissue_tree_json(nodes) -> serde_json::Value`** — array of
   `{ "id": <full uuid>, "title", "state", "subissues": [...] }`. Empty array
   when none.
3. **`subissue_tree_markdown(nodes) -> String`** — nested bullet list, 2-space
   indent per level. Caller prepends `## Sub-issues` only when non-empty.

### `ti next` wiring (`commands/next.rs`)

`run` already has `all_tickets` before it is consumed into `candidates`. Build
the `by_id` map from a clone or restructure so the map survives candidate
selection. After the ticket is chosen:

- **text:** append `subissue_tree_text` (with header) when non-empty.
- **json:** `let mut v = serde_json::to_value(&ticket)?; v["subissues"] =
  subissue_tree_json(&tree); println!("{}", serde_json::to_string_pretty(&v)?);`
  — the `{ "next": null }` sentinel path is unchanged.
- **markdown:** append `subissue_tree_markdown` (with header) when non-empty.

### `ti show` wiring (`commands/show.rs`)

`show` already builds `all = store.list()` in the text and markdown paths; the
JSON path must now also load it. Build `by_id`, build the tree, then:

- **text:** keep the existing flat `Children:` relation line (quick summary),
  and append the recursive `Sub-issues:` tree section below when non-empty.
- **json:** inject `subissues` onto the ticket value, same as `next`.
- **markdown:** append `## Sub-issues` tree when non-empty.

The `--filter` path is unaffected (it operates on the plain ticket value; no
`subissues` key — acceptable, filter is for stored fields).

## Schema docs

Add a `subissues` property to the ticket object definition in **both**:
- `docs/schema/v1.json`
- `crates/ticgit/docs/schema/v1.json`

Defined as an array of recursive sub-issue nodes (`id`, `title`, `state`,
`subissues`). Document it as **optional / additive** — present on `ti next` and
`ti show` output, absent elsewhere; existing consumers that ignore unknown keys
are unaffected. Update the top-level `description` string to mention the field.

## Testing (`crates/ticgit/tests/cli.rs`)

- `next --json` on a ticket with a 2-level open sub-issue tree → asserts nested
  `subissues[0].subissues[0].id/title/state`.
- Closed sub-issue is absent from the tree.
- Ticket with no children → `subissues == []`.
- `show --json` mirrors the same nested assertions.
- Existing `next_*` tests still pass (ticket object top-level unchanged).
- A plain-text test asserting the `Sub-issues:` block and indentation.

## Out of scope

- TUI changes.
- `ti list` sub-issue nesting.
- Depth limiting / collapsing (render full depth).
