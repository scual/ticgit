//! Ticket statistics shared by `ti stats` and the TUI dashboard.

use std::collections::{BTreeMap, HashMap};

use time::OffsetDateTime;
use uuid::Uuid;

use crate::ticket::{Ticket, TicketStatus};

/// Aggregate counts and recency lists over a set of tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketStats {
    pub total: usize,
    pub open: usize,
    pub closed: usize,
    pub with_comments: usize,
    /// Tickets created in the 7 days before `now`.
    pub created_7d: usize,
    /// Tickets closed in the 7 days before `now`, by close time.
    pub closed_7d: usize,
    /// `(state, count)`, most common first, ties alphabetical.
    pub states: Vec<(String, usize)>,
    /// `(tag, count)`, most common first, ties alphabetical.
    pub tags: Vec<(String, usize)>,
    /// `(assignee label, count)`, most common first, ties alphabetical.
    pub assignees: Vec<(String, usize)>,
    /// `(id, title)`, newest created first.
    pub recently_opened: Vec<(Uuid, String)>,
    /// `(id, closed at, title)`, most recently closed first.
    pub recently_closed: Vec<(Uuid, OffsetDateTime, String)>,
}

impl TicketStats {
    /// Compute stats as of `now`. `closed_at` supplies close times (see
    /// `TicketStore::closed_times`); a closed ticket without one falls back
    /// to its creation time. `assignee_label` turns an assignee email into
    /// the label counted under `assignees`.
    pub fn compute(
        tickets: &[Ticket],
        closed_at: &HashMap<Uuid, OffsetDateTime>,
        now: OffsetDateTime,
        assignee_label: impl Fn(&str) -> String,
    ) -> Self {
        let week_ago = now - time::Duration::days(7);
        let mut open = 0;
        let mut closed = 0;
        let mut with_comments = 0;
        let mut created_7d = 0;
        let mut states = BTreeMap::<String, usize>::new();
        let mut tags = BTreeMap::<String, usize>::new();
        let mut assignees = BTreeMap::<String, usize>::new();
        let mut opened = Vec::new();
        let mut recently_closed = Vec::new();

        for ticket in tickets {
            match ticket.status {
                TicketStatus::Open => open += 1,
                TicketStatus::Closed => {
                    closed += 1;
                    let at = closed_at
                        .get(&ticket.id)
                        .copied()
                        .unwrap_or(ticket.created_at);
                    recently_closed.push((ticket.id, at, ticket.title.clone()));
                }
            }
            if !ticket.comments.is_empty() {
                with_comments += 1;
            }
            if ticket.created_at >= week_ago {
                created_7d += 1;
            }
            *states.entry(ticket.state.as_str().to_string()).or_default() += 1;
            for tag in &ticket.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
            if let Some(assigned) = &ticket.assigned {
                *assignees.entry(assignee_label(assigned)).or_default() += 1;
            }
            opened.push((ticket.created_at, ticket.id, ticket.title.clone()));
        }

        opened.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        recently_closed.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let closed_7d = recently_closed
            .iter()
            .filter(|(_, at, _)| *at >= week_ago)
            .count();

        Self {
            total: tickets.len(),
            open,
            closed,
            with_comments,
            created_7d,
            closed_7d,
            states: sort_counts(states),
            tags: sort_counts(tags),
            assignees: sort_counts(assignees),
            recently_opened: opened
                .into_iter()
                .map(|(_, id, title)| (id, title))
                .collect(),
            recently_closed,
        }
    }
}

fn sort_counts(map: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut values = map.into_iter().collect::<Vec<_>>();
    values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ticket::TicketState;
    use std::collections::BTreeSet;

    fn ticket(
        title: &str,
        status: TicketStatus,
        state: TicketState,
        created: OffsetDateTime,
    ) -> Ticket {
        Ticket {
            id: Uuid::new_v4(),
            title: title.into(),
            description: None,
            spec: None,
            status,
            state,
            assigned: None,
            closed_by: None,
            priority: None,
            points: None,
            milestone: None,
            code: None,
            parent: None,
            children: BTreeSet::new(),
            depends_on: BTreeSet::new(),
            blocks: BTreeSet::new(),
            tags: BTreeSet::new(),
            meta: BTreeMap::new(),
            comments: vec![],
            created_at: created,
            created_by: "tester".into(),
        }
    }

    #[test]
    fn compute_counts_and_orders() {
        let now = OffsetDateTime::from_unix_timestamp(10_000_000).unwrap();
        let day = time::Duration::days(1);
        let mut fresh = ticket("fresh", TicketStatus::Open, TicketState::New, now - day);
        fresh.tags.insert("b".into());
        fresh.tags.insert("a".into());
        fresh.assigned = Some("ann@example.com".into());
        let mut old_open = ticket("old", TicketStatus::Open, TicketState::New, now - day * 30);
        old_open.tags.insert("a".into());
        let old_closed = ticket(
            "closed-old",
            TicketStatus::Closed,
            TicketState::Resolved,
            now - day * 30,
        );
        let mut recent_close = ticket(
            "closed-now",
            TicketStatus::Closed,
            TicketState::Wontfix,
            now - day * 20,
        );
        recent_close.comments.push(crate::ticket::Comment {
            author: "x".into(),
            at: now,
            body: "hi".into(),
        });
        let closed_at = HashMap::from([(recent_close.id, now - day * 2)]);
        let tickets = vec![fresh.clone(), old_open, old_closed, recent_close.clone()];

        let stats = TicketStats::compute(&tickets, &closed_at, now, |a| {
            a.split_once('@').map_or(a, |(l, _)| l).to_string()
        });

        assert_eq!((stats.total, stats.open, stats.closed), (4, 2, 2));
        assert_eq!(stats.with_comments, 1);
        assert_eq!(stats.created_7d, 1);
        // Only the ticket with a recent close time counts; the other falls
        // back to its (old) creation time.
        assert_eq!(stats.closed_7d, 1);
        assert_eq!(
            stats.states,
            vec![
                ("new".into(), 2),
                ("resolved".into(), 1),
                ("wontfix".into(), 1)
            ]
        );
        assert_eq!(stats.tags, vec![("a".into(), 2), ("b".into(), 1)]);
        assert_eq!(stats.assignees, vec![("ann".into(), 1)]);
        assert_eq!(stats.recently_opened[0].0, fresh.id);
        assert_eq!(stats.recently_closed[0].0, recent_close.id);
    }
}
