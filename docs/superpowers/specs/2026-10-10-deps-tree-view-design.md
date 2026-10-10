# `ti deps` — transitive dependency tree — design

Ticket: `720ab0` — Transitive / tree view of ticket Dependencies.

## Problem

`ti list --depends-on X` / `--blocks X` return **direct** relations only
(decided in 2870b8). Users sometimes need the full chain — everything upstream
of X (transitive blockers) or downstream (transitive dependents) — with the
chain structure visible.

## Command

`ti deps [TICKET]` — a new subcommand.

- Ticket reference: positional or `-t`, defaults to the checked-out ticket,
  `@` sentinel works (resolved via `commands::resolve_ref`).
- Default: the **blockers** tree (upstream) — walk `depends_on` edges
  transitively ("what must finish before this ticket").
- `--dependents`: the **dependents** tree (downstream) — walk `blocks` edges
  ("what finishing this ticket unblocks").
- `--both`: print both trees.
- `--all`: include closed nodes (rendered with their state) and keep walking
  through them. Default prunes a closed ticket and its subtree, because a
  resolved blocker no longer blocks — consistent with `ti next` and the
  sub-issue tree. This answers the A→B(closed)→C question: C does **not**
  block A by default.
- `--json` / `--markdown`: machine output. Default is indented text.

## Where the code lives

Mirror the existing recursive sub-issue tree (`render::build_subissue_tree` +
`subissue_tree_{text,json,markdown}`).

- **Library** (`ticgit-lib`, in `query.rs` — the dependency home): a pure walk

  ```rust
  pub enum DepDirection { Blockers, Dependents }

  pub struct DepNode {
      pub id: Uuid,
      pub title: String,
      pub state: TicketState,
      pub status: TicketStatus,
      pub children: Vec<DepNode>,
  }

  /// Transitive dependency tree rooted at `root`.
  /// Blockers follows depends_on edges; Dependents follows blocks edges.
  /// When include_closed is false, closed tickets are pruned (node and subtree).
  /// Cycle-safe via a visited set on the current path.
  pub fn dependency_tree(
      tickets: &[Ticket],
      root: Uuid,
      direction: DepDirection,
      include_closed: bool,
  ) -> Vec<DepNode>;
  ```

  Unit-testable without the CLI.

- **CLI**:
  - `render.rs`: `dep_tree_text`, `dep_tree_json`, `dep_tree_markdown`
    mirroring the sub-issue renderers.
  - `commands/deps.rs`: new module, clap `Args`, `run`.
  - `cli.rs`: register the subcommand + menu/help entry.

## JSON shape

```json
{
  "ticket": "<full-uuid>",
  "direction": "blockers",        // or "dependents"
  "nodes": [ { "id", "title", "state", "status", "children": [ … ] } ]
}
```

`--both` emits an array of two such objects (blockers first, then dependents).

## Edge cases

- Root has no dependencies in the chosen direction → empty `nodes`, exit 0,
  text output says so ("no blockers" / "no dependents").
- A diamond (two paths reach the same ticket) renders the ticket under each
  parent; the visited-set guard only prevents infinite cycles on the active
  path, not diamond re-appearance (matches the sub-issue tree behaviour).
- Unknown / ambiguous ticket → non-zero exit (standard `resolve_ref` error).

## Testing

- **Library unit tests** (`query.rs`): A depends on B depends on C →
  `dependency_tree(.., Blockers, false)` from A yields B→C nested; reversed
  direction from C yields B→A; a closed B prunes B and C by default but appears
  with `--all` (include_closed = true); a cycle A→B→A terminates.
- **CLI integration** (`tests/cli.rs`): `ti deps` prints the transitive
  blockers chain; `--dependents` flips direction; `--all` includes a closed
  intermediate; `ti deps @` resolves the checked-out ticket; `--json` has the
  documented shape.

## Documentation

- `ti deps` added to: the `ti agent` guide (BOTH `docs/agents.md` and
  `crates/ticgit/docs/agents.md` — build.rs asserts they match), the ticgit
  skill `reference.md` and `SKILL.md`, the README command table, and the
  `cli.rs` help/menu.

## Out of scope

- Changing `ti next`'s direct-only blocking semantics.
- Editing dependencies from this command (`ti depends` stays the mutator).
- A graphical/DOT export.
