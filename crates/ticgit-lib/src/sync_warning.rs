//! Warnings a Sync raises about what it merged.
//!
//! A deleted ticket keeps its keys, so edits another clone made to it before
//! it learned of the Deletion still merge in (ADR 0003: kept and reported,
//! never dropped or silently applied). The pull itself knows nothing about
//! that; this module watches it from the outside: take a
//! [`DeletedTicketItems`] snapshot before the pull, then ask
//! [`TicketStore::edits_on_deleted`] what arrived on tickets that were
//! deleted before the pull and still are.

use std::collections::{BTreeMap, BTreeSet};

use git_meta_lib::{MetaValue, Target};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;
use crate::keys;
use crate::oplog::{project_deletion, Op, OpKind};
use crate::store::TicketStore;

/// One thing a Sync wants the user to know about. Serializes as
/// `{"kind": "<kebab-case kind>", ...fields}` (the `ti sync --json`
/// `warnings` entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncWarning {
    /// `count` operations, comments or set members arrived for `ticket`, which
    /// is deleted locally. They are kept on the hidden ticket; `ti undelete`
    /// shows them.
    EditOnDeleted { ticket: Uuid, count: usize },
}

/// The items (operations, comments, set members, custom fields) of every
/// ticket that is deleted at snapshot time, taken before a pull so
/// [`TicketStore::edits_on_deleted`] can tell what the pull brought in.
#[derive(Debug, Clone, Default)]
pub struct DeletedTicketItems {
    items: BTreeMap<Uuid, BTreeSet<String>>,
}

/// One ticket's items, keyed by a stable identity, each with the time it was
/// made when the store records one (operations and comments do; set members
/// and custom fields do not).
struct TicketItems {
    deleted_at: Option<OffsetDateTime>,
    items: BTreeMap<String, ItemTime>,
}

enum ItemTime {
    At(OffsetDateTime),
    /// A del/undel operation: the Deletion itself, never an edit.
    Lifecycle,
    Unknown,
}

impl TicketStore {
    /// Snapshot the items of every deleted ticket. Take it right before a
    /// pull and pass it to [`edits_on_deleted`](Self::edits_on_deleted) after.
    pub fn deleted_ticket_items(&self) -> Result<DeletedTicketItems> {
        let items = self
            .ticket_items()?
            .into_iter()
            .filter(|(_, t)| t.deleted_at.is_some())
            .map(|(id, t)| (id, t.items.into_keys().collect()))
            .collect();
        Ok(DeletedTicketItems { items })
    }

    /// Edits that arrived since `before` on tickets that were deleted then and
    /// still are, one [`SyncWarning::EditOnDeleted`] per ticket. An operation
    /// or comment counts only when it is newer than the ticket's delete
    /// operation; set members and custom fields carry no time and count
    /// whenever they arrived. Delete/undelete operations never count.
    pub fn edits_on_deleted(&self, before: &DeletedTicketItems) -> Result<Vec<SyncWarning>> {
        let mut warnings = Vec::new();
        for (id, ticket) in self.ticket_items()? {
            let (Some(deleted_at), Some(seen)) = (ticket.deleted_at, before.items.get(&id)) else {
                continue;
            };
            let count = ticket
                .items
                .iter()
                .filter(|(item, _)| !seen.contains(*item))
                .filter(|(_, time)| match time {
                    ItemTime::At(at) => *at > deleted_at,
                    ItemTime::Lifecycle => false,
                    ItemTime::Unknown => true,
                })
                .count();
            if count > 0 {
                warnings.push(SyncWarning::EditOnDeleted { ticket: id, count });
            }
        }
        Ok(warnings)
    }

    fn ticket_items(&self) -> Result<BTreeMap<Uuid, TicketItems>> {
        let p = self.session().target(&Target::project());
        let mut ops_by_id: BTreeMap<Uuid, Vec<Op>> = BTreeMap::new();
        let mut items_by_id: BTreeMap<Uuid, BTreeMap<String, ItemTime>> = BTreeMap::new();
        for (key, value) in p.get_all_values(Some(&keys::tickets_prefix()))? {
            if let Some((id, _, _)) = keys::parse_ticket_op(&key) {
                let op = match &value {
                    MetaValue::String(s) => serde_json::from_str::<Op>(s).ok(),
                    _ => None,
                };
                let time = match &op {
                    Some(op) if is_lifecycle(op) => ItemTime::Lifecycle,
                    Some(op) => ItemTime::At(op.created_at),
                    None => ItemTime::Unknown,
                };
                items_by_id.entry(id).or_default().insert(key, time);
                ops_by_id.entry(id).or_default().extend(op);
                continue;
            }
            let Some((id, field)) = keys::parse_ticket_field(&key) else {
                continue;
            };
            if field == keys::FORMAT_VERSION_FIELD {
                continue;
            }
            let items = items_by_id.entry(id).or_default();
            match value {
                MetaValue::List(entries) => {
                    for entry in entries {
                        let at = OffsetDateTime::from_unix_timestamp_nanos(
                            i128::from(entry.timestamp) * 1_000_000,
                        )
                        .map_or(ItemTime::Unknown, ItemTime::At);
                        let item = format!("{key}\0{}\0{}", entry.timestamp, entry.value);
                        items.insert(item, at);
                    }
                }
                MetaValue::Set(members) => {
                    for member in members {
                        items.insert(format!("{key}\0{member}"), ItemTime::Unknown);
                    }
                }
                MetaValue::String(s) => {
                    items.insert(format!("{key}\0{s}"), ItemTime::Unknown);
                }
                // A value kind this binary does not know: identify it by key.
                _ => {
                    items.insert(key, ItemTime::Unknown);
                }
            }
        }
        Ok(items_by_id
            .into_iter()
            .map(|(id, items)| {
                let ops = ops_by_id.remove(&id).unwrap_or_default();
                let deleted_at = project_deletion(&ops).map(|op| op.created_at);
                (id, TicketItems { deleted_at, items })
            })
            .collect())
    }
}

fn is_lifecycle(op: &Op) -> bool {
    matches!(
        op.decoded(),
        Some(OpKind::Delete { .. } | OpKind::Undelete)
    )
}
