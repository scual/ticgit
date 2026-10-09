# Search improvements for `ti list --search`

Date: 2026-10-09
Status: approved design, pre-implementation

## Problem

`ti list --search` today is a single ASCII-lowercased substring match
(`query.rs`: `haystack.to_ascii_lowercase().contains(needle)`), with one
optional `scope:` prefix covering the whole needle. Limitations:

- **ASCII-only case fold** — `to_ascii_lowercase()` leaves non-ASCII letters
  untouched, so `CITTÀ` does not match `città`.
- **Single literal needle** — no way to require multiple terms; `login timeout`
  is matched as the literal substring `login timeout`, not "both words".
- **No match feedback** — results give no visual cue where the term hit.

## Goals

1. Unicode-correct, case-insensitive matching.
2. Multi-term search: whitespace-separated terms combine with **AND**; a
   quoted `"..."` group is a single phrase term.
3. Per-term scope: each term may carry its own `title:` / `description:` /
   `comments:` prefix; unscoped terms search any field.
4. Highlight matched terms in `ti list` (title column) and `ti show`
   (title, description, comment bodies).

Explicit non-goals: fuzzy/typo tolerance, relevance ranking, regex.

## Design

### 1. Parser — `ticgit-lib/src/query.rs`

Replace the single-needle `SearchFilter` with a list of terms:

```rust
pub struct SearchFilter { pub terms: Vec<SearchTerm> }
pub struct SearchTerm   { pub scope: SearchScope, pub needle: String }
```

`SearchFilter::parse(spec)`:

- Tokenize `spec` on whitespace, except a `"..."` run is one token (phrase).
  A quote opened and never closed runs to end of string.
- For each token, split once on `:`. If the left side is a known scope
  (`title`, `description`|`desc`, `comments`|`comment`) the remainder is the
  needle at that scope; otherwise the whole token is the needle at
  `SearchScope::Any`. This keeps `title:foo` and bare `foo` behaving exactly as
  before.
- A scope prefix on a quoted token applies to the phrase:
  `title:"login timeout"` → scope `Title`, needle `login timeout`.
- Lowercase each needle with `str::to_lowercase()` (full Unicode), not
  `to_ascii_lowercase()`.
- Empty `spec` → empty `terms` → matches everything (unchanged behavior).

### 2. Match rule — `query.rs`

A ticket matches the filter iff **every** term matches (AND). A term matches
when its scoped field(s) contain the needle:

- `Any` → title OR description OR any comment body
- `Title` / `Description` / `Comments` → that field only

`contains` helper becomes Unicode:

```rust
fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}
```

`to_lowercase()` allocates per call; acceptable — there is no search index and
tickets are already fully scanned from git-meta on every `list`.

### 3. Highlighting — `ticgit/src/render.rs`

New helper:

```rust
fn highlight(cell: &str, needles: &[String], base: &str) -> String
```

Wraps each matched (case-insensitive) substring in bold yellow
(`\x1b[1m` + `ANSI_YELLOW`), then re-opens `base` after `ANSI_RESET` so the
surrounding column color survives the inserted span.

- **list:** apply to the title cell **after** `fit()` has done width
  truncation/padding (ANSI escapes are zero display width and would corrupt
  `UnicodeWidthStr::width` / `truncate_display` if inserted earlier). The
  highlight goes inside the existing `ANSI_BLUE` title span, with blue as the
  `base` to restore.
- **show:** apply to title, description, and each comment body as they are
  rendered.
- The needle list for highlighting is the parsed terms' needles (phrase terms
  highlight the whole phrase).

Color gating follows the existing render path: the plain-text table/detail
renderers are not used for `--json` / `--markdown`, so highlighting is
inherently excluded there. Respect the same TTY/no-color conditions the current
renderer already honors; do not emit highlight escapes when color is off.

### 4. `ti show` search input — `ticgit/src/commands/show.rs`

Add an explicit `--search <SEARCH>` flag to `show` (same syntax as `list`).
When present, `show` parses it with `SearchFilter::parse` and highlights the
terms. No coupling to `session_state` — absent flag means no highlight. This
avoids stale-state surprises and keeps the feature discoverable via `--help`.

### 5. CLI help, docs, and agent instructions

- **clap help:** update the `list --search` doc comment and add the new
  `show --search` doc comment to describe multi-term AND, quoted phrases, and
  per-term scope. Keep wording short enough to read well in `--help`.
- **Agent guide (two copies, must stay byte-identical):** `docs/agents.md` and
  `crates/ticgit/docs/agents.md` are checked for divergence by `build.rs` — edit
  both with the same content or the build fails. Update the search guidance so
  agents know the AND/phrase/per-scope syntax.
- **Website docs:** `docs/docs/creating-tickets.html` and
  `docs/docs/writeups.html` mention search; update the examples to the new
  syntax where relevant.
- **README.md:** add a short search example if a search section exists; if not,
  no change required (it currently does not document `--search`).
- **Schema:** `--search` is an input filter and does not change JSON output, so
  `docs/schema/v1.json` is unaffected.

After editing, run `cargo build` to confirm the `build.rs` agent-guide sync
check passes, and `ti list --help` / `ti show --help` to eyeball the rendered
help text.

## Testing — `ticgit/tests/cli.rs`

- Multi-term AND: `--search "login timeout"` returns only tickets containing
  both words; a ticket with only one is excluded.
- Quoted phrase: `--search '"login timeout"'` matches the phrase but not a
  ticket that has the two words apart.
- Per-term scope: `--search 'title:login description:timeout'` requires login
  in title AND timeout in description.
- Unicode fold: a ticket titled `Città` matches `--search CITTÀ`.
- Back-compat: `--search title:foo` and bare `--search foo` behave as today.
- Highlight: on a forced-color/TTY run the title cell contains the highlight
  escape; `--json` output contains no ANSI.
- `ti show <id> --search term` highlights in detail output; `--json` stays
  clean.

Library-level parser unit tests in `query.rs` for tokenization edge cases
(unclosed quote, scope prefix on phrase, unknown prefix fallthrough).

## Risks

- **Nested ANSI in list cells** is the fiddliest part; the post-`fit()`
  ordering and base-color restore must be exact or columns misalign. Covered by
  a highlight escape-presence test plus manual check.
- `to_lowercase()` can change byte length vs. the original (e.g. `İ`), so
  highlight substring offsets must be computed on a consistently-cased copy, not
  by mixing raw and lowercased indices.
