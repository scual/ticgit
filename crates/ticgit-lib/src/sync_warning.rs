//! Warnings a Sync raises about what it merged.
//!
//! A deleted ticket keeps its keys, so edits another clone made to it before
//! it learned of the Deletion still merge in (ADR 0003: kept and reported,
//! never dropped or silently applied). The merge itself knows nothing about
//! that; this module watches it from the outside: take a
//! [`DeletedTicketItems`] snapshot before the Sync's first merge, then, once
//! its last merge is done, ask [`TicketStore::edits_on_deleted`] what arrived
//! on tickets that were deleted before and still are.

use std::collections::{BTreeMap, BTreeSet};

use git_meta_lib::MetaValue;
use serde::Serialize;
use uuid::Uuid;

use crate::error::Result;
use crate::keys;
use crate::oplog::{project_deletion, Lamport, Op, OpKind};
use crate::store::TicketStore;

/// One thing a Sync wants the user to know about. Serializes as
/// `{"kind": "<kebab-case kind>", ...fields}` (the `warnings` entries of
/// `ti sync --json` and `ti pull --json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncWarning {
    /// `count` operations, comments or set members arrived for `ticket`, which
    /// is deleted locally. They are kept on the hidden ticket; `ti undelete`
    /// shows them.
    EditOnDeleted { ticket: Uuid, count: usize },
}

/// The items (operations, comments, set members, custom fields) of every
/// ticket that is deleted at snapshot time, taken before a Sync merges
/// anything so [`TicketStore::edits_on_deleted`] can tell what it brought in.
#[derive(Debug, Clone, Default)]
pub struct DeletedTicketItems {
    items: BTreeMap<Uuid, BTreeSet<String>>,
}

/// One ticket's items, keyed by a stable identity, plus the operation that
/// leaves it deleted (if it is).
struct TicketItems {
    deletion: Option<(Lamport, String)>,
    items: BTreeMap<String, ItemOrder>,
}

/// Where an item stands against the Deletion.
enum ItemOrder {
    /// An operation, ordered by `(lamport, id)` like every op (ADR 0002).
    Op(Lamport, String),
    /// A del/undel operation: the Deletion itself, never an edit.
    Lifecycle,
    /// A comment, set member or custom field: no Lamport clock, and its wall
    /// clock (if any) belongs to another clone, so it cannot be ordered
    /// against the Deletion.
    Unordered,
}

impl TicketStore {
    /// Snapshot the items of every deleted ticket. Take it before a Sync's
    /// first merge and pass it to [`edits_on_deleted`](Self::edits_on_deleted)
    /// after its last.
    pub fn deleted_ticket_items(&self) -> Result<DeletedTicketItems> {
        let items = self
            .ticket_items()?
            .into_iter()
            .filter(|(_, t)| t.deletion.is_some())
            .map(|(id, t)| (id, t.items.into_keys().collect()))
            .collect();
        Ok(DeletedTicketItems { items })
    }

    /// Edits that arrived since `before` on tickets that were deleted then and
    /// still are, one [`SyncWarning::EditOnDeleted`] per ticket. An operation
    /// counts when it follows the ticket's delete operation in `(lamport, id)`
    /// order; comments, set members and custom fields carry no Lamport clock
    /// and count whenever they arrived. Delete/undelete operations never count.
    pub fn edits_on_deleted(&self, before: &DeletedTicketItems) -> Result<Vec<SyncWarning>> {
        let mut warnings = Vec::new();
        for (id, ticket) in self.ticket_items()? {
            let (Some(deletion), Some(seen)) = (ticket.deletion, before.items.get(&id)) else {
                continue;
            };
            let count = ticket
                .items
                .iter()
                .filter(|(item, _)| !seen.contains(*item))
                .filter(|(_, order)| match order {
                    ItemOrder::Op(lamport, op_id) => {
                        (*lamport, op_id.as_str()) > (deletion.0, deletion.1.as_str())
                    }
                    ItemOrder::Lifecycle => false,
                    ItemOrder::Unordered => true,
                })
                .count();
            if count > 0 {
                warnings.push(SyncWarning::EditOnDeleted { ticket: id, count });
            }
        }
        Ok(warnings)
    }

    fn ticket_items(&self) -> Result<BTreeMap<Uuid, TicketItems>> {
        let mut out = BTreeMap::new();
        for (id, record) in self.ticket_records()? {
            let deletion = project_deletion(&record.ops).map(|op| (op.lamport, op.id.clone()));
            let mut items = BTreeMap::new();
            for op in record.ops {
                let order = if is_lifecycle(&op) {
                    ItemOrder::Lifecycle
                } else {
                    ItemOrder::Op(op.lamport, op.id.clone())
                };
                items.insert(format!("op\0{}", op.id), order);
            }
            for (field, value) in record.fields {
                if field == keys::FORMAT_VERSION_FIELD {
                    continue;
                }
                match value {
                    MetaValue::List(entries) => {
                        for entry in entries {
                            let item = format!("{field}\0{}\0{}", entry.timestamp, entry.value);
                            items.insert(item, ItemOrder::Unordered);
                        }
                    }
                    MetaValue::Set(members) => {
                        for member in members {
                            items.insert(format!("{field}\0{member}"), ItemOrder::Unordered);
                        }
                    }
                    MetaValue::String(s) => {
                        items.insert(format!("{field}\0{s}"), ItemOrder::Unordered);
                    }
                    // A value kind this binary does not know: identify it by key.
                    _ => {
                        items.insert(field, ItemOrder::Unordered);
                    }
                }
            }
            out.insert(id, TicketItems { deletion, items });
        }
        Ok(out)
    }
}

fn is_lifecycle(op: &Op) -> bool {
    matches!(op.decoded(), Some(OpKind::Delete { .. } | OpKind::Undelete))
}

#[cfg(test)]
mod tests {
    use git_meta_lib::Target;
    use time::{Duration, OffsetDateTime};

    use super::SyncWarning;
    use crate::keys;
    use crate::oplog::{Op, OpKind};
    use crate::test_support::test_store;
    use crate::ticket::NewTicketOpts;

    fn set_priority(lamport: u64, created_at: OffsetDateTime, value: &str) -> Op {
        let fields = [("priority".to_string(), value.to_string())].into();
        Op::new(
            lamport,
            "b@example.com",
            created_at,
            OpKind::SetField { fields },
        )
        .unwrap()
    }

    /// Operations order by Lamport clock, never by wall clock (ADR 0002): an
    /// operation from a clone whose clock runs behind still counts when it
    /// follows the Deletion, and one that precedes the Deletion never does,
    /// however late its wall clock.
    #[test]
    fn operations_count_by_lamport_order_against_the_deletion_not_wall_clock() {
        let (store, _td) = test_store();
        let ticket = store.create("doomed", NewTicketOpts::default()).unwrap();
        store.delete_ticket(&ticket.id, None).unwrap();
        let del = store
            .load_ops(&ticket.id)
            .unwrap()
            .into_iter()
            .find(|op| op.kind == "del")
            .unwrap();
        let before = store.deleted_ticket_items().unwrap();

        // What a pull merges from another clone: one op after the Deletion
        // (clock an hour behind), one before it (clock a day ahead).
        let after_del = set_priority(del.lamport + 1, del.created_at - Duration::hours(1), "1");
        let before_del = set_priority(0, del.created_at + Duration::days(1), "2");
        let p = store.session().target(&Target::project());
        let merge = |op: &Op| {
            p.set(
                &keys::ticket_op(&ticket.id, op.lamport, &op.id),
                serde_json::to_string(op).unwrap().as_str(),
            )
            .unwrap();
        };
        let one_edit = vec![SyncWarning::EditOnDeleted {
            ticket: ticket.id,
            count: 1,
        }];

        merge(&after_del);
        assert_eq!(store.edits_on_deleted(&before).unwrap(), one_edit);
        merge(&before_del);
        assert_eq!(store.edits_on_deleted(&before).unwrap(), one_edit);
    }
}
