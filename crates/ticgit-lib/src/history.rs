//! Ticket change history and closed time, projected from the operation log.
//!
//! The reader behind [`crate::store::TicketStore::history`] and
//! [`crate::store::TicketStore::closed_at`]. Ops carry the field changes;
//! comments (which live outside the log) are merged in so tickets in the hybrid
//! format still read sensibly. A ticket with no ops at all yields a synthesized
//! `Created` entry from its stored creation time.

use serde::Serialize;
use time::OffsetDateTime;

use crate::oplog::{Op, OpKind};
use crate::ticket::Comment;

/// What kind of change a [`HistoryEntry`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAction {
    Created,
    Set,
    Cleared,
    Commented,
}

/// One readable change to a ticket: which field, to what, by whom, when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryEntry {
    pub action: HistoryAction,
    /// Field name (`status`, `title`, ...); `comments` for a comment.
    pub field: String,
    /// New value; `None` when the field was cleared.
    pub value: Option<String>,
    /// Author email.
    pub by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

fn sorted(ops: &[Op]) -> Vec<&Op> {
    let mut sorted: Vec<&Op> = ops.iter().collect();
    sorted.sort_by(|a, b| a.lamport.cmp(&b.lamport).then_with(|| a.id.cmp(&b.id)));
    sorted
}

/// Newest-first history from a ticket's ops and comments. `created` is the
/// fallback used to synthesize a `Created` entry when there is no `create` op.
pub(crate) fn project_history(
    ops: &[Op],
    comments: &[Comment],
    created: Option<(OffsetDateTime, &str, &str)>,
) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    let mut has_create = false;
    for op in sorted(ops) {
        match op.decoded() {
            Some(OpKind::Create { fields }) => {
                has_create = true;
                out.push(HistoryEntry {
                    action: HistoryAction::Created,
                    field: "title".to_string(),
                    value: fields.get("title").cloned(),
                    by: op.author.clone(),
                    at: op.created_at,
                });
            }
            Some(OpKind::SetField { fields }) => {
                for (field, value) in fields {
                    out.push(HistoryEntry {
                        action: HistoryAction::Set,
                        field,
                        value: Some(value),
                        by: op.author.clone(),
                        at: op.created_at,
                    });
                }
            }
            Some(OpKind::ClearField { fields }) => {
                for field in fields {
                    out.push(HistoryEntry {
                        action: HistoryAction::Cleared,
                        field,
                        value: None,
                        by: op.author.clone(),
                        at: op.created_at,
                    });
                }
            }
            None => {}
        }
    }
    if !has_create {
        if let Some((at, by, title)) = created {
            out.insert(
                0,
                HistoryEntry {
                    action: HistoryAction::Created,
                    field: "title".to_string(),
                    value: Some(title.to_string()),
                    by: by.to_string(),
                    at,
                },
            );
        }
    }
    for c in comments {
        out.push(HistoryEntry {
            action: HistoryAction::Commented,
            field: "comments".to_string(),
            value: Some(c.body.clone()),
            by: c.author.clone(),
            at: c.at,
        });
    }
    // Stable sort keeps the per-op field order for entries sharing a timestamp.
    out.sort_by_key(|e| e.at);
    out.reverse_groups();
    out
}

trait ReverseGroups {
    fn reverse_groups(&mut self);
}

impl ReverseGroups for Vec<HistoryEntry> {
    /// Reverse to newest-first while keeping entries that share a timestamp
    /// (one compound op) in their original field order.
    fn reverse_groups(&mut self) {
        let mut out: Vec<HistoryEntry> = Vec::with_capacity(self.len());
        let mut i = self.len();
        while i > 0 {
            let at = self[i - 1].at;
            let mut start = i - 1;
            while start > 0 && self[start - 1].at == at {
                start -= 1;
            }
            out.extend(self[start..i].iter().cloned());
            i = start;
        }
        *self = out;
    }
}

/// When the ticket last transitioned into `closed`, or `None` if it is not
/// closed (or no op records the transition).
pub(crate) fn project_closed_at(ops: &[Op]) -> Option<OffsetDateTime> {
    let mut closed_at: Option<OffsetDateTime> = None;
    for op in sorted(ops) {
        let status = match op.decoded() {
            Some(OpKind::Create { fields }) | Some(OpKind::SetField { fields }) => {
                fields.get("status").cloned()
            }
            Some(OpKind::ClearField { fields }) if fields.iter().any(|f| f == "status") => {
                closed_at = None;
                None
            }
            _ => None,
        };
        match status.as_deref() {
            Some("closed") => {
                if closed_at.is_none() {
                    closed_at = Some(op.created_at);
                }
            }
            Some(_) => closed_at = None,
            None => {}
        }
    }
    closed_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use time::Duration;

    fn set(lamport: u64, secs: i64, fields: &[(&str, &str)]) -> Op {
        let map: BTreeMap<String, String> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Op::new(
            lamport,
            "a@b.c",
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs),
            OpKind::SetField { fields: map },
        )
        .unwrap()
    }

    #[test]
    fn closed_at_is_first_close_transition() {
        let ops = vec![
            set(0, 10, &[("status", "open")]),
            set(1, 20, &[("status", "closed"), ("state", "resolved")]),
            set(2, 30, &[("state", "wontfix")]),
        ];
        assert_eq!(
            project_closed_at(&ops),
            Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(20))
        );
    }

    #[test]
    fn reopen_clears_closed_at() {
        let ops = vec![
            set(0, 10, &[("status", "closed")]),
            set(1, 20, &[("status", "open")]),
        ];
        assert_eq!(project_closed_at(&ops), None);
    }
}
