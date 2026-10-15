# TicGit Server — platform architecture

**Date:** 2026-10-15
**Status:** Design approved; layered implementation to follow (each layer gets its own spec → plan → build cycle).
**Scope:** Whole-platform architecture for a single self-hosted server binary. "Imitate big repo companies" (GitHub/Linear/Jira-style features and UX) — delivered as a self-hosted daemon, **not** a multi-tenant SaaS.

## 1. Purpose

TicGit today is a git-native, serverless, offline-first issue tracker: tickets live as
git-meta metadata under the `ticgit:` namespace and sync peer-to-peer via any git
remote. There is no central service and no index — tickets are discovered by
prefix-scanning `ticgit:tickets`.

TicGit Server adds an **optional** convenience layer on top of that model: an HTTP
API, an event/webhook bus, a web dashboard, and cross-repo portfolio views — so a
team gets the collaboration UX of a hosted tracker while keeping git as the single
source of truth.

## 2. Non-negotiable principles

These keep TicGit *TicGit*. Every design decision below is constrained by them.

1. **Git is the sole source of truth.** All server state is *derived* from git-meta
   and fully rebuildable. Deleting the server's database and re-pulling must
   reconstruct identical state.
2. **Server offline ≠ work stops.** The CLI continues to read and write git-meta
   directly with no server present. The server is pure convenience.
3. **The server is just another git client.** It never holds truth that git does not.
   Its writes are indistinguishable from a human running `ti`.

Any feature that would make the server a parallel/authoritative store (e.g. a
write queue whose state git doesn't yet reflect) is rejected on sight.

Cross-repo references (section 5) extend this: the forward edge is truth stored in
one repo's git-meta, and the reverse edge is *derived* — never a second stored copy.

## 3. Deployment shape

A single self-hosted binary, `ti serve` (one daemon per team, run next to a git
remote). It holds one or more local git clones of ticgit repos, pulls from their
remotes, serves HTTP, and writes back to git on behalf of web/API clients. No
multi-tenancy, no OAuth, no hosted SaaS. "Big company" refers to the feature set
and UX, not the infrastructure.

## 4. The shared engine (the real spine)

Every layer (L1–L4) sits on one engine. Build it once, build it well.

```
 git remote ──pull──> [ local clone(s) ] ──scan ticgit:──> [ SQLite index ]
                             │                                    │
                        write-back                          read / query / diff
                             │                                    │
         CLI/web write ──────┘                  API · events · dashboard ──┘
```

- **Ingest loop.** Pull each clone on an interval (and on inbound webhook), then
  prefix-scan `ticgit:tickets` via `TicketStore` to repopulate the index.
- **SQLite index (derived, disposable).** The library has no index; the server adds
  an embedded SQLite index for fast filter/sort/aggregate/portfolio queries at
  scale. It is never authoritative — rebuilt from git, safe to delete.
- **Diff-on-ingest.** Each pull compares new vs. previous index state to emit change
  events (`created` / `updated` / `closed` / `reopened` / `commented` / …). This one
  mechanism powers L2 webhooks, L3 live dashboard, and L4 notifications.

### Write path (decision: "server writes as a git client")

Web/API writes are applied through `ticgit-lib` against the server's local clone,
committed to git-meta, and pushed to the remote. Git remains the sole truth; a
server write is indistinguishable from a human running `ti`. Offline clients still
write independently, and git-meta merge reconciles concurrent edits exactly as it
does for two humans today. No new truth, no custom conflict resolution.

Rejected alternatives: **read-only server** (no web writes → dashboard-only, not the
target UX) and **server-owned write queue** (server holds truth git doesn't →
violates principle 1).

## 5. Cross-repo ticket references

A ticket in one repo can reference a ticket in another — most importantly as a
dependency ("A in `backend` depends on B in `frontend`"). This is the feature that
turns the portfolio (L3) from two lists shown side-by-side into one dependency graph
spanning repos, and it is the strongest justification for running the server at all.

### Why it is feasible here

TicGit ids are UUIDs, not per-repo sequential numbers. They are already globally
unique, so — unlike GitHub's `org/repo#123` — a repo coordinate is needed only to
**locate** a referenced ticket, never to **identify** it. There is no cross-repo id
collision to resolve.

### Reference format

A cross-repo reference is a **repo coordinate + UUID**. The coordinate is the git
remote URL (canonical, self-describing), optionally abbreviated by a short alias from
a server-side *known-repos* registry (`frontend#<uuid>`). The registry doubles as a
**security boundary**: resolving a coordinate means pulling that repo, so only
allowlisted remotes resolve.

Storage stays backward-compatible: a same-repo dependency value remains a bare UUID;
a cross-repo value carries a coordinate prefix. The resolver treats a bare UUID as
"this repo." The `depends-on` field layout change lives in `keys.rs` (single source
of truth for the key layout) — not inline.

### Where the edge lives (the one hard decision)

`CONTEXT.md` defines "A depends on B" and "B blocks A" as the same directed relation —
one edge, two views. Within a repo that is one stored fact. Across repos the two sides
would live in two different git-meta stores, and you may not own both.

Resolution: **store the forward edge once, on the depending ticket's repo (the one you
own); derive the reverse edge in the server index — never store a second copy.**

```
backend  git-meta:  ticket A  depends-on  frontend#B     ← TRUTH (write only repos you own)
frontend git-meta:  (untouched — stores nothing about A)
server   index:     A ⇄ B, derives "B blocks A"          ← DERIVED (like stats/events/portfolio)
```

This keeps every principle intact: the forward edge is the sole stored truth, the
reverse is derived exactly as every other server-side projection is, and a writer only
ever mutates repos it controls. Mirroring the edge into both repos is rejected (two
commits, needs write access to both, drifts).

### Resolution tiers (offline-first degrades honestly)

| Context | Behaviour |
| --- | --- |
| CLI offline, referenced repo not present | **Unresolved reference**: show the coordinate (`frontend#B`) + an "unresolved" marker. No crash, no blank. |
| CLI with the referenced repo cloned locally | Resolve the forward reference by reading that repo's git-meta — show title/state. |
| Server / portfolio | Resolve everything, derive reverse edges, and render `ti deps` as a **multi-repo** dependency graph. |

### Surface changes

`@` (checked-out ticket), `ti deps`, and the `--depends-on` / `--blocks` filters
become repo-aware. `CONTEXT.md` terminology is updated: the symmetric relation holds,
but cross-repo it is realized as *forward stored, reverse derived* rather than a single
stored fact.

## 6. Cross-repo requests

Section 5 covers pointing at a ticket that already exists elsewhere. This section
covers the opposite, write-side primitive: asking for work to be *created* in a repo
you do not own — e.g. a frontend developer needing the backend team to change an API
signature. The two are symmetric:

- **Cross-repo reference** (read side): "I point at your existing ticket."
- **Cross-repo request** (write side): "I propose a ticket into your repo; you accept."

### The constraint it respects

A writer only ever mutates repos it controls. A frontend developer normally has no
write access to the backend repo's git-meta, so they cannot simply create a ticket
there. The request flow routes around this *without* violating the rule.

### Flow

```
# frontend dev — writes only their OWN repo
ti new "Backend: getUser() should also return email" --request-to backend
ti depends @ --on backend#?         # pending cross-repo reference — the "unresolved" tier (section 5), reused

# server (holds both repos) surfaces it in the backend's inbox — a DERIVED view
# backend maintainer — the repo's own authority:
ti inbox                            # sees inbound proposals
ti accept <id>                      # server writes the real ticket into backend git-meta + pushes;
                                    #   the pending reference now resolves, reverse "blocks frontend#X" derives
ti reject <id> --reason "..."       # stays a note in the frontend repo; nothing written to backend
```

If the requester *does* hold a writer token on the target repo (L4 auth), they
skip the proposal entirely and create directly via the write path (`ti new --repo
backend …`). The request flow is specifically the no-write-access path.

### Where each piece lives

| Piece | Who writes it | Where it lives |
| --- | --- | --- |
| Request ticket + pending reference | frontend (its own repo) | frontend git-meta — truth |
| Proposal in the backend inbox | no one | **derived** by the server |
| Accepted ticket in the backend | backend (its owner authorizes) | backend git-meta — truth |
| Reverse "blocks" edge | no one | **derived** |

Only `accept` writes a repo the requester doesn't own, and it is triggered by *that
repo's* owner — never by the requester. The authority boundary stays crisp, and no new
stored truth is introduced: the proposal is derived state until the backend accepts.

### Serverless degradation

Without a shared daemon there is no inbox. The frontend dev still writes only the
request ticket (pending reference) in their own repo; promotion requires someone with
the backend repo to run `ti accept` / `ti new`. Honest degradation — the server is
what makes the hand-off fluid, consistent with every other layer.

## 7. Communication model (and an explicit non-goal)

Communication in TicGit is **anchored to work and stored where it is worth keeping** —
never a general messenger. This section records what is in scope and, deliberately,
what is not, so the boundary is not re-litigated later.

### In scope

- **Comment threads** (exist today via `ti comment`): discussion lives attached to the
  ticket, not in a vacuum. The server renders them live via the L2 SSE stream, so a
  dashboard viewer sees new comments in real time without a separate chat system.
- **`@mention` + directed notification:** the git-native form of "tell one person
  something" — you address them *about a ticket*, so there is always context. It is
  durable and rides the existing L2 event / L4 notification rails (into email/Slack).
- **`ti announce` — durable broadcast:** a team-wide notice (`"freeze on main
  Friday"`), synced to everyone, shown as a dashboard banner and emitted as an L2
  event. Low-volume, durable, worth keeping — a notice, not a conversation. (Whether
  it is a special ticket kind or a distinct object is an open question.)

### Explicit non-goal: 1-to-1 chat

Real-time private chat is **rejected**, because git-meta inverts nearly every property
it needs: git syncs on pull/interval (async, eventually-consistent) where chat wants
low latency; git history is append-only forever (no clean message deletion) where chat
is high-volume and ephemeral; git-meta is public to the repo where DMs are private; and
every message would be a commit + push. Building it would also turn an issue tracker
into a second-rate messenger and dilute its focus.

If ephemeral back-and-forth is needed, the L4 webhook sink **notifies into the tool
people already use** (Slack/email) rather than rebuilding it. Three needs, no chat to
maintain: tell a person → `@mention` on the relevant ticket; tell the team →
`ti announce`; chatter → Slack via the sink.

### Outbound notifications & channels

The server's only outbound contract is the canonical L2 event stream. Delivery channels
are *consumers* of that stream, never features baked into the daemon:

- **Free today:** the L2 SSE stream (dashboard) and generic webhooks. Slack, Discord,
  Teams — anything with an incoming-webhook URL — need **zero new code**; register a
  hook.
- **Optional adapters** (feature-gated, config-driven, consumer-only, never touch git):
  Telegram, WhatsApp and similar are not plain webhooks — each needs a bot token and a
  service-specific payload, so each is a thin "event → channel API" adapter. Kept
  satellite to the core so the binary does not rot when a provider changes their API,
  and a team compiles in only what it uses. WhatsApp specifically means the Business
  API (templates, cost) — heavier, clearly optional.
- **Push:** Web Push (VAPID) for the dashboard is reasonable since the dashboard is
  ours; native mobile push needs an app and is out of scope for the self-hosted binary.
- **Per-user routing** ("ping me on my assigned tickets via Telegram") is personal,
  server-local config — the same non-derived, outside-git category as `session_state`
  and webhook subscriptions, not git truth.

Rejected: hard-wiring each third-party service into the core as a first-class feature —
maintenance debt and dependency surface the event + webhook contract avoids.

## 8. Derived capabilities (what git's history and graph unlock)

The server's real superpower is derivation over data git already holds — its full
history and the dependency graph. These features add **no stored truth**; they index
and expose what is latent in git. This is where the git-native model *beats* a
traditional tracker rather than imitating it.

### History & audit (time-travel)

git-meta's entire history is already in git. The server indexes it to offer:

- `ti log <ticket>` — every status/field change, with author and commit.
- `ti blame <ticket>` — who set a field (e.g. `blocked`) and when, read from the commit.
- `ti stats --since …` — burndown / velocity from real history, not a separately
  maintained activity log.

Because the audit trail *is* git — and commits can be signed — it cannot be forged or
retro-edited the way a tracker's own database can. This is the headline differentiator:
"big company" features on a foundation the big companies structurally cannot match.

### Issue ↔ code binding

Commits reference a ticket UUID (a `Ticket: <uuid>` trailer or a branch name). The
server, already watching the repo, **derives** the link — "this ticket's code landed in
these commits / branches / PRs" — with nothing new stored. Closes the issue↔code loop
git-natively.

### Dependency-aware scheduling

Over the cross-repo dependency graph (section 5):

- `ti ready` — tickets whose dependencies are all closed (workable now).
- `ti critical-path` — the chain gating a milestone.
- `ti cycles` — dependency cycles, including cross-repo, surfaced as warnings.

Pure derivation; uniquely enabled by the server's multi-repo view.

### Non-goals in this space

Real-time collaborative ticket editing (co-editing cursors) is rejected for the same
reason as 1-to-1 chat (section 7): it fights git's append-only, pull-synced model.
Workflow state-machines are allowed only if their *configuration* lives in git-meta
(like saved views) and the server merely enforces it — the server never owns workflow
truth.

## 9. Layers

Build order is bottom-up; each layer depends on the one below and delivers value
standalone.

### L1 — Read/Write API (the spine)

HTTP surface mirroring the stable `--json` contract. Reuses `render.rs` JSON,
`stats.rs`, `query.rs` filters, and `dependency_tree` — no reimplementation.

```
GET   /api/v1/tickets          ?status&state&tag&assignee&sort&q   → array
GET   /api/v1/tickets/:id      (full UUID or unique prefix)        → object
GET   /api/v1/stats                                                → TicketStats
GET   /api/v1/deps/:id                                             → dependency tree
POST  /api/v1/tickets          create        ┐
PATCH /api/v1/tickets/:id      update/close   ├ write path → lib → commit → push
POST  /api/v1/tickets/:id/comment             ┘
GET   /api/v1/health           sync status, last-pull time, repo list
```

- Prefix id resolution and ambiguity→4xx mirror the CLI's exit-code rules
  (ambiguous/missing prefix fails non-zero / 4xx).
- `?repo=` selects which clone (see L3 portfolio).
- JSON carries no ANSI color; errors/diagnostics never pollute success bodies.
- Schema reuses and extends `docs/schema/v1.json` (versioned).
- Dependency values may be a bare UUID (same repo) or a `coordinate#UUID` cross-repo
  reference (section 5); the API resolves both, and marks a cross-repo reference it
  cannot reach as unresolved rather than failing the request.

### L2 — Event bus / webhooks

Built on diff-on-ingest.

- `GET /api/v1/events` — **Server-Sent Events** stream (one-way, proxy-friendly,
  simpler than websockets). Powers the live dashboard.
- Webhook subscriptions: `POST /api/v1/hooks {url, filter}`. On a matching change,
  POST a payload (ticket JSON + `{event: created|updated|closed|…}`). Delivery
  retries with backoff.
- Subscriptions and delivery state persist in the server-local SQLite sidecar
  (server config, **not** git).

### L3 — Views (dashboard + portfolio)

- **Dashboard.** Server-rendered HTML served by the binary, assets embedded via
  `rust-embed` (still one binary). Board (columns = `state`), filters, and
  `stats.rs` charts. Live updates via SSE. Writes go through L1 POST/PATCH.
  **Front-end: htmx + SSE** — minimal dependencies, no JS build toolchain, fits the
  one-binary low-dep ethos. (React SPA considered and deferred; revisit only if
  richer interactivity is required.)
- **Portfolio.** The daemon holds N clones; `/api/v1/tickets?repo=all` aggregates
  across them for a cross-project backlog and combined stats. Only the server can do
  this — each repo is otherwise blind to the others. With cross-repo references
  (section 5) resolved and their reverse edges derived, `ti deps` becomes a
  **multi-repo dependency graph** — the portfolio's headline capability, not just two
  lists side by side.

### L4 — Collaboration (single-binary flavor)

- **Auth.** Static API tokens (config file / `ti serve --token`) for API and
  webhooks. Optional basic user list for dashboard login. The authenticated identity
  maps to the git-meta author on write-back, so web edits attribute correctly.
- **Access control.** Coarse: reader vs. writer token, scoped per repo (the daemon
  holds many). A cross-repo request's `accept` (section 6) is gated on a writer token
  for the *target* repo — that is exactly what keeps the authority boundary crisp.
  Per-field ACL is YAGNI until asked.
- **Notifications.** Built on L2 events — email/Slack sinks are webhook consumers,
  not a separate subsystem.

## 10. Tech stack & code layout

- **Async/HTTP:** `axum` + `tokio`. New deps, confined to the server crate so the
  CLI and library stay synchronous and lean.
- **Index:** `rusqlite` (embedded, bundled).
- **Dashboard assets:** `rust-embed`.
- **Git-meta I/O:** reuse `ticgit-lib` for every read and write — the server never
  touches the storage layout directly (that stays in `keys.rs`).
- **New crate:** `crates/ticgit-server`, keeping `ticgit-lib` clap-free/server-free
  and the CLI sync. Entry point is `ti serve` dispatching into it (vs. a separate
  `ti-server` binary — final call deferred to the L1 spec).
- **Dependency caution:** the `gix-actor = 0.40.0` pin (see CLAUDE.md) must survive
  adding the async/HTTP stack; check the `gix`/`winnow` graph when introducing
  `axum`/`tokio`.

## 11. Testing approach

Mirror the existing CLI integration suite (`crates/ticgit/tests/cli.rs`): each test
builds a throwaway git repo in a tempdir, boots the server on an ephemeral port, and
drives real HTTP.

- **Read tests:** assert JSON body and status against seeded tickets.
- **Write-path tests:** assert git-meta actually changed *and* was pushed.
- **Event tests:** assert the SSE stream / webhook fires on an ingested change, with
  the correct `event` type and payload.
- **Cross-repo tests:** two tempdir repos; assert a forward `coordinate#UUID`
  dependency resolves, the reverse edge is derived in the portfolio, and an absent
  referenced repo yields an unresolved marker (not an error).
- **Request tests:** a `--request-to` ticket writes only the requester's repo and
  leaves a pending reference; `accept` (with a target-repo writer token) writes the
  target git-meta and resolves the reference; `reject` writes nothing to the target;
  `accept` without the token is refused.
- **History tests:** mutate a ticket across several commits; assert `ti log` / `ti
  blame` report the right authors and commits and that historical stats reflect the
  real timeline.
- **Scheduling tests:** build a dependency graph; assert `ti ready` excludes tickets
  with open dependencies and a cross-repo cycle is detected and surfaced.
- Keep the tempdir pattern for hermeticity; prefer end-to-end HTTP tests for anything
  user-observable.

## 12. Build sequence

1. **L1** — shared engine (ingest loop, SQLite index, diff-on-ingest) + read/write
   API. The bottleneck everything else needs. Lands the cross-repo reference *format*
   (`keys.rs` layout + bare-UUID back-compat) and indexes git-meta history for
   `ti log` / `ti blame` / historical stats (section 8).
2. **L2** — events/webhooks on the diff machinery; the canonical event stream that
   every notification channel consumes (section 7).
3. **L3** — dashboard (htmx+SSE) and portfolio aggregation: cross-repo reference
   resolution, derived reverse edges, the multi-repo `ti deps` graph, the request
   inbox, plus the derived scheduling (`ti ready` / `critical-path` / `cycles`) and
   issue↔code binding views (section 8).
4. **L4** — auth, access control, notification sinks and optional channel adapters
   (Telegram/WhatsApp/push); gates cross-repo `accept` on a writer token for the target
   repo. `@mention` and `ti announce` ride these rails (section 7); no chat subsystem is
   built.

Each layer is specced, planned, and built on its own before the next begins; L1–L3
are useful standalone (internal/single-team) before L4's cost is paid.

## 13. Open questions (resolve at per-layer spec time)

- `ti serve` subcommand vs. separate `ti-server` binary.
- Index schema detail and migration strategy for `docs/schema/v1.json` extensions.
- Pull interval / webhook-driven-pull tuning, and multi-remote auth for the clones.
- Exact SSE reconnection / event-replay semantics.
- Cross-repo coordinate canonical form (full remote URL vs. alias) and how the
  known-repos registry is configured and secured.
- How a locally-cloned (serverless) CLI discovers sibling repos to resolve forward
  references without a server present.
- Cross-repo request lifecycle beyond accept/reject: how a pending request is
  superseded or expires, and whether `reject` notifies the requester (via L2 events).
- Whether `ti announce` is a special ticket kind or a distinct git-meta object, its
  lifecycle (dismiss/expire), and `@mention` resolution to a notification target.
- Commit↔ticket binding convention (`Ticket:` trailer vs. branch name) and how PRs are
  detected across hosting providers (section 8).
- Which notification channel adapters ship in-tree vs. are left to an external bridge,
  and the per-user routing config format (section 7).
- History indexing strategy: full replay vs. incremental, and performance on large
  git-meta histories (section 8).
