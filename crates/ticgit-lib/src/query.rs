//! Filtering and sorting for `ti list`.
//!
//! Mirrors the legacy CLI's `-s STATE`, `-t TAG`, `-a ASSIGNED`, `-T`,
//! `-o ORDER` selectors, with a stable, testable semantics.

use std::cmp::Ordering;

use crate::ticket::{Ticket, TicketState, TicketStatus};
use uuid::Uuid;

/// All knobs `ti list` understands. Build one by parsing CLI flags and
/// pass it through [`apply`].
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub status: Option<TicketStatus>,
    pub state: Option<TicketState>,
    pub tag: Option<String>,
    pub tags: Vec<String>,
    pub tag_match_all: bool,
    pub assigned: Option<String>,
    pub only_tagged: bool,
    pub search: Option<SearchFilter>,
    pub order: Option<SortOrder>,
    pub depends_on: Option<Uuid>,
    pub blocks: Option<Uuid>,
    /// Restrict to the direct sub-issues (children) of this ticket.
    pub parent: Option<Uuid>,
    /// When true, exclude tickets that have a parent (i.e. sub-issues).
    /// Bypassed whenever `depends_on`, `blocks`, or `parent` is set, so a
    /// relationship filter never silently hides a sub-issue blocker,
    /// dependent, or child.
    pub hide_subissues: bool,
}

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

/// Sort orders accepted by `ti list -o`. Each can be inverted with the
/// `desc` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Priority,
    Title,
    State,
    Assigned,
    Created,
}

#[derive(Debug, Clone, Copy)]
pub struct SortOrder {
    pub key: SortKey,
    pub desc: bool,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "priority" | "prio" => Some(SortKey::Priority),
            "title" => Some(SortKey::Title),
            "state" => Some(SortKey::State),
            "assigned" => Some(SortKey::Assigned),
            "created" | "date" | "time" => Some(SortKey::Created),
            _ => None,
        }
    }
}

impl SortOrder {
    /// Parse a `key[.desc]` spec like `state.desc` or `created`.
    pub fn parse(spec: &str) -> Option<Self> {
        let (key_str, desc) = match spec.split_once('.') {
            Some((k, "desc")) => (k, true),
            Some((k, "asc")) => (k, false),
            _ => (spec, false),
        };
        Some(SortOrder {
            key: SortKey::parse(key_str)?,
            desc,
        })
    }
}

impl SearchFilter {
    /// Parse a search spec into AND-combined terms.
    ///
    /// Tokens split on whitespace, except a `"..."` run is one phrase token.
    /// A token may carry a leading `scope:` prefix (`title:`,
    /// `description:`/`desc:`, `comments:`/`comment:`); an unknown prefix is
    /// left as part of the needle. The prefix must be unquoted: quoting
    /// escapes it, so `"title:foo"` searches for the literal text `title:foo`.
    /// Needles are lowercased (full Unicode).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut terms = Vec::new();
        for token in tokenize(spec) {
            let (scope, needle) = match token.colon {
                Some(i) => match SearchScope::parse(&token.text[..i]) {
                    Some(scope) => (scope, token.text[i + 1..].to_string()),
                    None => (SearchScope::Any, token.text),
                },
                None => (SearchScope::Any, token.text),
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

/// One whitespace-delimited unit of a search spec.
struct Token {
    text: String,
    /// Byte index in `text` of an unquoted `:` that may end a scope prefix.
    /// `None` once a quote has opened before any colon, so quoting escapes it.
    colon: Option<usize>,
}

/// Split a search spec into tokens: whitespace separates, `"..."` groups a
/// phrase (quote chars are dropped, inner whitespace preserved). An unclosed
/// quote runs to end of input.
fn tokenize(spec: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut colon = None;
    let mut quoted = false;
    let mut in_quotes = false;
    let mut has_token = false;
    for ch in spec.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            quoted = true;
            has_token = true;
        } else if ch.is_whitespace() && !in_quotes {
            if has_token {
                tokens.push(Token {
                    text: std::mem::take(&mut cur),
                    colon: colon.take(),
                });
                has_token = false;
                quoted = false;
            }
        } else {
            if ch == ':' && !quoted && colon.is_none() {
                colon = Some(cur.len());
            }
            cur.push(ch);
            has_token = true;
        }
    }
    if has_token {
        tokens.push(Token { text: cur, colon });
    }
    tokens
}

/// Filter and sort `tickets` according to `filter`. Returns a new vec.
pub fn apply(tickets: Vec<Ticket>, filter: &Filter) -> Vec<Ticket> {
    let mut tickets: Vec<Ticket> = tickets
        .into_iter()
        .filter(|t| {
            if let Some(state) = filter.state {
                if t.state != state {
                    return false;
                }
            }
            if let Some(status) = filter.status {
                if t.status != status {
                    return false;
                }
            }
            let tags = filter_tags(filter);
            if !tags.is_empty() {
                let matches = if filter.tag_match_all {
                    tags.iter().all(|tag| t.tags.contains(*tag))
                } else {
                    tags.iter().any(|tag| t.tags.contains(*tag))
                };
                if !matches {
                    return false;
                }
            }
            if let Some(assigned) = &filter.assigned {
                if t.assigned.as_deref() != Some(assigned.as_str()) {
                    return false;
                }
            }
            if filter.only_tagged && t.tags.is_empty() {
                return false;
            }
            if let Some(search) = &filter.search {
                if !search.matches(t) {
                    return false;
                }
            }
            if let Some(ticket_id) = filter.depends_on {
                if !t.depends_on.contains(&ticket_id) {
                    return false;
                }
            }
            if let Some(ticket_id) = filter.blocks {
                if !t.blocks.contains(&ticket_id) {
                    return false;
                }
            }
            if let Some(ticket_id) = filter.parent {
                if t.parent != Some(ticket_id) {
                    return false;
                }
            }
            let relationship_filter_active =
                filter.depends_on.is_some() || filter.blocks.is_some() || filter.parent.is_some();
            if filter.hide_subissues && !relationship_filter_active && t.parent.is_some() {
                return false;
            }
            true
        })
        .collect();

    if let Some(order) = filter.order {
        tickets.sort_by(|a, b| compare(a, b, order.key, order.desc));
    } else {
        // Stable default: open first, then by priority (lower = more important),
        // then by recency (newer first).
        tickets.sort_by(|a, b| {
            let by_status = status_rank(a.status).cmp(&status_rank(b.status));
            if by_status != Ordering::Equal {
                return by_status;
            }
            let by_priority = priority_rank(a.priority).cmp(&priority_rank(b.priority));
            if by_priority != Ordering::Equal {
                return by_priority;
            }
            b.created_at.cmp(&a.created_at)
        });
    }

    tickets
}

/// Tags that park a ticket out of the `ti next` work queue by default.
/// Restore them with [`NextOptions::include_deferred`].
pub const DEFERRED_TAGS: &[&str] = &["deferred", "backlog"];

/// Knobs for [`next_queue`] — the ticket `ti next` picks to work on.
#[derive(Debug, Clone, Default)]
pub struct NextOptions {
    /// Only consider tickets carrying this tag.
    pub tag: Option<String>,
    /// Only consider tickets assigned to this user.
    pub assigned: Option<String>,
    /// Include tickets tagged with a [`DEFERRED_TAGS`] value (normally hidden).
    pub include_deferred: bool,
}

/// Rank for the `ti next` sort key's state component: tickets being actively
/// worked rank first, `blocked` ranks last. Closed states never reach here
/// (they are excluded before sorting) but are given high ranks for totality.
fn next_state_rank(s: TicketState) -> u8 {
    match s {
        TicketState::InProgress => 0,
        TicketState::Assigned => 1,
        TicketState::Review => 2,
        TicketState::New => 3,
        TicketState::Blocked => 4,
        // Closed states are filtered out of the queue; keep the match total.
        TicketState::Resolved => 5,
        TicketState::Wontfix => 6,
        TicketState::Duplicate => 7,
        TicketState::Invalid => 8,
    }
}

/// Open, actionable tickets ordered best-first for `ti next`.
///
/// A ticket is **excluded** from the queue when any of these hold:
/// - it is closed (`status == Closed`);
/// - it is a sub-issue (has a `parent`);
/// - it has an unresolved dependency (a `depends_on` id whose ticket is not
///   closed, or is missing);
/// - it is tagged with a [`DEFERRED_TAGS`] value, unless
///   [`NextOptions::include_deferred`] is set.
///
/// Plus the optional [`NextOptions::tag`] / [`NextOptions::assigned`] narrowing.
///
/// Remaining tickets are ordered by an ascending lexicographic key (first =
/// work on next):
/// 1. **priority** — numeric priorities first, ascending (lower = more
///    important); `none` sorts last (it is the least-important band, so a
///    numeric priority — even a large one — always ranks above unprioritised
///    tickets; numbers cannot sink a ticket below the unprioritised pile).
/// 2. **state** — `in-progress`, `assigned`, `review`, `new`, then `blocked`
///    (blocked sorts last but is not excluded).
/// 3. **created_at** — oldest first.
pub fn next_queue<'a>(tickets: &'a [Ticket], opts: &NextOptions) -> Vec<&'a Ticket> {
    let closed_ids: std::collections::HashSet<Uuid> = tickets
        .iter()
        .filter(|t| t.status == TicketStatus::Closed)
        .map(|t| t.id)
        .collect();

    let mut candidates: Vec<&Ticket> = tickets
        .iter()
        .filter(|t| t.status == TicketStatus::Open)
        .filter(|t| t.parent.is_none())
        .filter(|t| t.depends_on.iter().all(|dep| closed_ids.contains(dep)))
        .filter(|t| {
            opts.include_deferred
                || !t
                    .tags
                    .iter()
                    .any(|tag| DEFERRED_TAGS.contains(&tag.as_str()))
        })
        .filter(|t| match &opts.tag {
            Some(tag) => t.tags.contains(tag),
            None => true,
        })
        .filter(|t| match &opts.assigned {
            Some(assigned) => t.assigned.as_deref() == Some(assigned.as_str()),
            None => true,
        })
        .collect();

    candidates.sort_by(|a, b| {
        priority_rank(a.priority)
            .cmp(&priority_rank(b.priority))
            .then_with(|| next_state_rank(a.state).cmp(&next_state_rank(b.state)))
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });

    candidates
}

fn filter_tags(filter: &Filter) -> Vec<&String> {
    let mut tags = Vec::new();
    if let Some(tag) = &filter.tag {
        tags.push(tag);
    }
    for tag in &filter.tags {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

fn state_rank(s: TicketState) -> u8 {
    match s {
        TicketState::New => 0,
        TicketState::Assigned => 1,
        TicketState::InProgress => 2,
        TicketState::Blocked => 3,
        TicketState::Review => 4,
        TicketState::Resolved => 5,
        TicketState::Wontfix => 6,
        TicketState::Duplicate => 7,
        TicketState::Invalid => 8,
    }
}

/// Tickets with a priority sort before those without; among prioritised
/// tickets, lower numbers come first (1 = most important).
fn priority_rank(p: Option<i64>) -> (u8, i64) {
    match p {
        Some(v) => (0, v),
        None => (1, 0),
    }
}

fn status_rank(s: TicketStatus) -> u8 {
    match s {
        TicketStatus::Open => 0,
        TicketStatus::Closed => 1,
    }
}

fn compare(a: &Ticket, b: &Ticket, key: SortKey, desc: bool) -> Ordering {
    let ord = match key {
        SortKey::Priority => priority_rank(a.priority)
            .cmp(&priority_rank(b.priority))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id)),
        SortKey::Title => a.title.cmp(&b.title),
        SortKey::State => status_rank(a.status)
            .cmp(&status_rank(b.status))
            .then_with(|| state_rank(a.state).cmp(&state_rank(b.state))),
        SortKey::Assigned => a
            .assigned
            .as_deref()
            .unwrap_or("")
            .cmp(b.assigned.as_deref().unwrap_or("")),
        SortKey::Created => a.created_at.cmp(&b.created_at),
    };
    if desc {
        ord.reverse()
    } else {
        ord
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ticket::Comment;
    use std::collections::{BTreeMap, BTreeSet};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn t(
        title: &str,
        status: TicketStatus,
        state: TicketState,
        tag: Option<&str>,
        assigned: Option<&str>,
        ts: i64,
    ) -> Ticket {
        let mut tags = BTreeSet::new();
        if let Some(s) = tag {
            tags.insert(s.to_string());
        }
        Ticket {
            id: Uuid::new_v4(),
            title: title.into(),
            description: None,
            spec: None,
            status,
            state,
            assigned: assigned.map(String::from),
            closed_by: None,
            priority: None,
            points: None,
            milestone: None,
            code: None,
            parent: None,
            children: BTreeSet::new(),
            depends_on: BTreeSet::new(),
            blocks: BTreeSet::new(),
            tags,
            meta: BTreeMap::new(),
            comments: vec![],
            created_at: OffsetDateTime::from_unix_timestamp(ts).unwrap(),
            created_by: "tester".into(),
        }
    }

    #[test]
    fn filter_by_state() {
        let input = vec![
            t("a", TicketStatus::Open, TicketState::New, None, None, 1),
            t(
                "b",
                TicketStatus::Closed,
                TicketState::Resolved,
                None,
                None,
                2,
            ),
        ];
        let f = Filter {
            state: Some(TicketState::New),
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "a");
    }

    #[test]
    fn filter_by_status() {
        let input = vec![
            t("a", TicketStatus::Open, TicketState::Blocked, None, None, 1),
            t(
                "b",
                TicketStatus::Closed,
                TicketState::Resolved,
                None,
                None,
                2,
            ),
        ];
        let f = Filter {
            status: Some(TicketStatus::Open),
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "a");
    }

    #[test]
    fn filter_by_tag() {
        let input = vec![
            t(
                "a",
                TicketStatus::Open,
                TicketState::New,
                Some("bug"),
                None,
                1,
            ),
            t(
                "b",
                TicketStatus::Open,
                TicketState::New,
                Some("ui"),
                None,
                2,
            ),
        ];
        let f = Filter {
            tag: Some("ui".into()),
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "b");
    }

    #[test]
    fn filter_by_any_tag() {
        let mut bug = t(
            "bug",
            TicketStatus::Open,
            TicketState::New,
            Some("bug"),
            None,
            1,
        );
        bug.tags.insert("cli".into());
        let ui = t(
            "ui",
            TicketStatus::Open,
            TicketState::New,
            Some("ui"),
            None,
            2,
        );
        let docs = t(
            "docs",
            TicketStatus::Open,
            TicketState::New,
            Some("docs"),
            None,
            3,
        );
        let f = Filter {
            tags: vec!["bug".into(), "ui".into()],
            tag_match_all: false,
            ..Default::default()
        };
        let out = apply(vec![bug, ui, docs], &f);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "ui");
        assert_eq!(out[1].title, "bug");
    }

    #[test]
    fn filter_by_all_tags() {
        let mut both = t(
            "both",
            TicketStatus::Open,
            TicketState::New,
            Some("bug"),
            None,
            1,
        );
        both.tags.insert("ui".into());
        let bug = t(
            "bug",
            TicketStatus::Open,
            TicketState::New,
            Some("bug"),
            None,
            2,
        );
        let f = Filter {
            tags: vec!["bug".into(), "ui".into()],
            tag_match_all: true,
            ..Default::default()
        };
        let out = apply(vec![both, bug], &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "both");
    }

    #[test]
    fn filter_by_assigned() {
        let input = vec![
            t(
                "a",
                TicketStatus::Open,
                TicketState::New,
                None,
                Some("alice@x"),
                1,
            ),
            t(
                "b",
                TicketStatus::Open,
                TicketState::New,
                None,
                Some("bob@x"),
                2,
            ),
        ];
        let f = Filter {
            assigned: Some("bob@x".into()),
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "b");
    }

    #[test]
    fn only_tagged_filters_untagged() {
        let input = vec![
            t(
                "untagged",
                TicketStatus::Open,
                TicketState::New,
                None,
                None,
                1,
            ),
            t(
                "tagged",
                TicketStatus::Open,
                TicketState::New,
                Some("bug"),
                None,
                2,
            ),
        ];
        let f = Filter {
            only_tagged: true,
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "tagged");
    }

    #[test]
    fn filter_by_dependency() {
        let dependency_id = Uuid::new_v4();
        let mut dependent = t(
            "dependent",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        dependent.depends_on.insert(dependency_id);
        let unrelated = t(
            "unrelated",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        let f = Filter {
            depends_on: Some(dependency_id),
            ..Default::default()
        };

        let out = apply(vec![dependent, unrelated], &f);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "dependent");
    }

    #[test]
    fn filter_by_blocks() {
        let dependent_id = Uuid::new_v4();
        let mut blocker = t(
            "blocker",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        blocker.blocks.insert(dependent_id);
        let unrelated = t(
            "unrelated",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        let f = Filter {
            blocks: Some(dependent_id),
            ..Default::default()
        };

        let out = apply(vec![blocker, unrelated], &f);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "blocker");
    }

    #[test]
    fn filter_by_parent_surfaces_children_despite_hide_subissues() {
        let parent_id = Uuid::new_v4();
        let mut child = t("child", TicketStatus::Open, TicketState::New, None, None, 1);
        child.parent = Some(parent_id);
        let unrelated = t(
            "unrelated",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        // hide_subissues is on by default; a --parent match must still show.
        let f = Filter {
            parent: Some(parent_id),
            hide_subissues: true,
            ..Default::default()
        };

        let out = apply(vec![child, unrelated], &f);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "child");
    }

    #[test]
    fn relationship_filters_bypass_hide_subissues() {
        let dependency_id = Uuid::new_v4();
        let mut sub_dependent = t(
            "sub-dependent",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        sub_dependent.parent = Some(Uuid::new_v4());
        sub_dependent.depends_on.insert(dependency_id);

        let out = apply(
            vec![sub_dependent],
            &Filter {
                depends_on: Some(dependency_id),
                hide_subissues: true,
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);

        let dependent_id = Uuid::new_v4();
        let mut sub_blocker = t(
            "sub-blocker",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        sub_blocker.parent = Some(Uuid::new_v4());
        sub_blocker.blocks.insert(dependent_id);

        let out = apply(
            vec![sub_blocker],
            &Filter {
                blocks: Some(dependent_id),
                hide_subissues: true,
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn default_order_puts_open_first_then_priority_then_newer_first() {
        let mut high_pri = t(
            "high-pri",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        high_pri.priority = Some(1);
        let mut low_pri = t(
            "low-pri",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            50,
        );
        low_pri.priority = Some(3);
        let no_pri_new = t(
            "no-pri-new",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            80,
        );
        let no_pri_old = t(
            "no-pri-old",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            10,
        );
        let closed = t(
            "closed",
            TicketStatus::Closed,
            TicketState::Resolved,
            None,
            None,
            100,
        );
        let input = vec![
            no_pri_old.clone(),
            closed.clone(),
            low_pri.clone(),
            no_pri_new.clone(),
            high_pri.clone(),
        ];
        let out = apply(input, &Filter::default());
        // Open before closed, priority 1 before 3, prioritised before unprioritised,
        // then newer before older.
        assert_eq!(out[0].title, "high-pri");
        assert_eq!(out[1].title, "low-pri");
        assert_eq!(out[2].title, "no-pri-new");
        assert_eq!(out[3].title, "no-pri-old");
        assert_eq!(out[4].title, "closed");
    }

    #[test]
    fn sort_by_title_desc() {
        let input = vec![
            t("alpha", TicketStatus::Open, TicketState::New, None, None, 1),
            t("beta", TicketStatus::Open, TicketState::New, None, None, 2),
            t("gamma", TicketStatus::Open, TicketState::New, None, None, 3),
        ];
        let f = Filter {
            order: Some(SortOrder {
                key: SortKey::Title,
                desc: true,
            }),
            ..Default::default()
        };
        let out = apply(input, &f);
        assert_eq!(out[0].title, "gamma");
        assert_eq!(out[2].title, "alpha");
    }

    #[test]
    fn sort_order_parse() {
        assert!(matches!(
            SortOrder::parse("title").unwrap().key,
            SortKey::Title
        ));
        let o = SortOrder::parse("state.desc").unwrap();
        assert_eq!(o.key, SortKey::State);
        assert!(o.desc);
        assert!(SortOrder::parse("nonsense").is_none());
    }

    #[test]
    fn search_matches_title_description_and_comments() {
        let title = t(
            "parser panic",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        let mut description = t("docs", TicketStatus::Open, TicketState::New, None, None, 2);
        description.description = Some("explain parser recovery".into());
        let mut comment = t("ui", TicketStatus::Open, TicketState::New, None, None, 3);
        comment.comments.push(Comment {
            author: "tester".into(),
            at: OffsetDateTime::UNIX_EPOCH,
            body: "parser fails on empty input".into(),
        });

        let out = apply(
            vec![title.clone(), description.clone(), comment.clone()],
            &Filter {
                search: Some(SearchFilter::parse("parser").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 3);

        let out = apply(
            vec![title.clone(), description.clone(), comment.clone()],
            &Filter {
                search: Some(SearchFilter::parse("title:parser").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, title.title);

        let out = apply(
            vec![title, description, comment.clone()],
            &Filter {
                search: Some(SearchFilter::parse("comments:empty").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, comment.title);
    }

    #[test]
    fn search_multi_term_is_and() {
        let both = t(
            "fix LOGIN during TIMEOUT",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        let only_one = t(
            "login screen",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        let out = apply(
            vec![both.clone(), only_one],
            &Filter {
                search: Some(SearchFilter::parse("login timeout").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, both.title); // AND, case-insensitive, order-independent
    }

    #[test]
    fn search_quoted_phrase_is_contiguous() {
        let phrase = t(
            "fix login timeout bug",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        let apart = t(
            "login on the timeout screen",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        let out = apply(
            vec![phrase.clone(), apart],
            &Filter {
                search: Some(SearchFilter::parse("\"login timeout\"").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, phrase.title);
    }

    #[test]
    fn search_per_term_scope() {
        let mut hit = t(
            "login page",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        hit.description = Some("timeout after 30s".into());
        let mut miss_desc = t(
            "login page",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            2,
        );
        miss_desc.description = Some("no issue".into());
        let mut miss_title = t(
            "home page",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            3,
        );
        miss_title.description = Some("timeout after 30s".into());

        let out = apply(
            vec![hit.clone(), miss_desc, miss_title],
            &Filter {
                search: Some(SearchFilter::parse("title:login description:timeout").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, hit.title);
    }

    #[test]
    fn search_is_unicode_case_insensitive() {
        let ticket = t(
            "gestione città",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        let out = apply(
            vec![ticket],
            &Filter {
                search: Some(SearchFilter::parse("CITTÀ").unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn search_unknown_prefix_is_literal_needle() {
        // `foo:` is not a known scope, so the whole token is the needle (back-compat).
        let f = SearchFilter::parse("foo:bar").unwrap();
        assert_eq!(f.terms.len(), 1);
        assert_eq!(f.terms[0].scope, SearchScope::Any);
        assert_eq!(f.terms[0].needle, "foo:bar");
    }

    #[test]
    fn search_quotes_escape_scope_prefix() {
        let f = SearchFilter::parse("\"title:foo\"").unwrap();
        assert_eq!(f.terms.len(), 1);
        assert_eq!(f.terms[0].scope, SearchScope::Any);
        assert_eq!(f.terms[0].needle, "title:foo");

        // A quoted space after the colon is part of the literal, not a stray
        // leading space on a scoped needle.
        let f = SearchFilter::parse("\"title: foo\"").unwrap();
        assert_eq!(f.terms[0].scope, SearchScope::Any);
        assert_eq!(f.terms[0].needle, "title: foo");
    }

    #[test]
    fn search_unquoted_prefix_scopes_quoted_phrase() {
        let f = SearchFilter::parse("title:\"login timeout\"").unwrap();
        assert_eq!(f.terms.len(), 1);
        assert_eq!(f.terms[0].scope, SearchScope::Title);
        assert_eq!(f.terms[0].needle, "login timeout");
    }

    #[test]
    fn search_empty_matches_everything() {
        let f = SearchFilter::parse("   ").unwrap();
        assert!(f.terms.is_empty());
        let ticket = t(
            "anything",
            TicketStatus::Open,
            TicketState::New,
            None,
            None,
            1,
        );
        let out = apply(
            vec![ticket],
            &Filter {
                search: Some(f),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
    }

    // --- `ti next` queue (next_queue) ---------------------------------------

    fn open(title: &str, ts: i64) -> Ticket {
        t(title, TicketStatus::Open, TicketState::New, None, None, ts)
    }

    #[test]
    fn next_numeric_priority_ranks_above_none() {
        let mut numbered = open("numbered", 1);
        numbered.priority = Some(100);
        let unprioritised = open("unprioritised", 2);

        let input = [unprioritised, numbered];
        let out = next_queue(&input, &NextOptions::default());

        // A large number still outranks `none`: none is the floor.
        assert_eq!(out[0].title, "numbered");
        assert_eq!(out[1].title, "unprioritised");
    }

    #[test]
    fn next_numeric_priorities_order_ascending() {
        let mut low = open("low-number", 1);
        low.priority = Some(1);
        let mut high = open("high-number", 2);
        high.priority = Some(5);

        let input = [high, low];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out[0].title, "low-number");
        assert_eq!(out[1].title, "high-number");
    }

    #[test]
    fn next_blocked_sorts_last_at_equal_priority() {
        let actionable = open("actionable", 1);
        let blocked = t(
            "blocked",
            TicketStatus::Open,
            TicketState::Blocked,
            None,
            None,
            2,
        );

        let input = [blocked, actionable];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out[0].title, "actionable");
        assert_eq!(out[1].title, "blocked");
    }

    #[test]
    fn next_excludes_deferred_and_backlog_tags_by_default() {
        let active = open("active", 1);
        let deferred = t(
            "deferred-work",
            TicketStatus::Open,
            TicketState::New,
            Some("deferred"),
            None,
            2,
        );
        let backlog = t(
            "backlog-work",
            TicketStatus::Open,
            TicketState::New,
            Some("backlog"),
            None,
            3,
        );

        let input = [active, deferred, backlog];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "active");
    }

    #[test]
    fn next_include_deferred_restores_tagged_tickets() {
        let active = open("active", 1);
        let deferred = t(
            "deferred-work",
            TicketStatus::Open,
            TicketState::New,
            Some("deferred"),
            None,
            2,
        );

        let input = [active, deferred];
        let out = next_queue(
            &input,
            &NextOptions {
                include_deferred: true,
                ..Default::default()
            },
        );

        assert_eq!(out.len(), 2);
    }

    #[test]
    fn next_excludes_closed_tickets() {
        let open_ticket = open("open", 1);
        let closed = t(
            "closed",
            TicketStatus::Closed,
            TicketState::Resolved,
            None,
            None,
            2,
        );

        let input = [open_ticket, closed];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "open");
    }

    #[test]
    fn next_excludes_subissues() {
        let top = open("top", 1);
        let mut child = open("child", 2);
        child.parent = Some(Uuid::new_v4());

        let input = [top, child];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "top");
    }

    #[test]
    fn next_excludes_tickets_with_unresolved_dependencies() {
        let blocker = open("blocker", 1);
        let blocker_id = blocker.id;
        let mut dependent = open("dependent", 2);
        dependent.depends_on.insert(blocker_id);

        // Blocker is open -> dependent is not actionable yet.
        let input = [blocker.clone(), dependent.clone()];
        let out = next_queue(&input, &NextOptions::default());
        assert!(out.iter().all(|x| x.title != "dependent"));

        // Close the blocker -> dependent becomes actionable.
        let mut closed_blocker = blocker;
        closed_blocker.status = TicketStatus::Closed;
        closed_blocker.state = TicketState::Resolved;
        let input = [closed_blocker, dependent];
        let out = next_queue(&input, &NextOptions::default());
        assert!(out.iter().any(|x| x.title == "dependent"));
    }

    #[test]
    fn next_breaks_ties_by_oldest_created_first() {
        let newer = open("newer", 100);
        let older = open("older", 10);

        let input = [newer, older];
        let out = next_queue(&input, &NextOptions::default());

        assert_eq!(out[0].title, "older");
        assert_eq!(out[1].title, "newer");
    }
}
