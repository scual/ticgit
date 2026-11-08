//! Operation-log primitives.
//!
//! Step 1 of the op-log roadmap (ticket `ca6420`): content-derived identifiers.
//! An operation's identity is the SHA-256 of its canonical byte encoding, so two
//! clones that independently record the same operation derive the same id — no
//! central coordination, automatic dedupe, and a deterministic tiebreaker for
//! equal Lamport values.
//!
//! It also defines the operation model (ticket `b209a5`): the immutable [`Op`]
//! envelope, the typed [`OpKind`], and [`replay`] — the conflict-free fold that
//! projects an op set into scalar field state. Clones merge by unioning op sets
//! and replaying in `(lamport, id)` order, so concurrent edits to different
//! fields both survive instead of clobbering (the op-log's headline win).
//!
//! Not yet wired here: the live write path, the local derived cache with
//! staleness detection, and `ti verify`. Those integrate this model into
//! [`crate::store`] in the next slice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::Result;

/// Content-derived identifier for an operation: the lowercase hex SHA-256 of a
/// canonical byte encoding. Globally unique without coordination; identical
/// content always yields an identical id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpId(String);

impl OpId {
    /// Full 64-character lowercase hex form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Short 12-character prefix for display, mirroring the ticket short-id
    /// convention. Callers that need disambiguation fall back to a longer
    /// prefix (as [`crate::store::TicketStore::resolve_id`] does for tickets).
    #[must_use]
    pub fn short(&self) -> String {
        self.0.chars().take(12).collect()
    }
}

impl std::fmt::Display for OpId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Derive an [`OpId`] from arbitrary bytes via SHA-256.
#[must_use]
pub fn content_id(bytes: &[u8]) -> OpId {
    let digest = Sha256::digest(bytes);
    OpId(hex::encode(digest))
}

/// Canonical byte encoding feeding [`content_id`]. Deterministic: map keys are
/// emitted in sorted order (callers MUST use ordered collections such as
/// `BTreeMap`, never `HashMap`, so the encoding is reproducible across runs and
/// clones).
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

/// Logical (Lamport) clock value ordering operations across clones.
pub type Lamport = u64;

/// Op envelope format version this binary writes. Older readers skip ops whose
/// `kind` they don't understand (see [`replay`]), so this bumps only on an
/// envelope-shape break, not on new op kinds.
pub const CURRENT_OP_FORMAT: u32 = 1;

/// A single, immutable operation in a ticket's log. The synced source of truth:
/// clones merge by taking the union of their op sets and replaying in
/// `(lamport, id)` order ([`replay`]). The `kind` discriminant is stored as a
/// string and the `payload` opaque so a reader can skip op kinds it does not
/// understand without corrupting replay (forward-compat, roadmap step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    /// Content-derived id ([`content_id`] over every field but this one).
    pub id: String,
    pub lamport: Lamport,
    pub author: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub format_version: u32,
    pub kind: String,
    pub payload: Value,
    /// Armored SSH signature over `id` (step 5). `None` for unsigned ops
    /// (legacy, or clients without a configured signing key). Excluded from the
    /// content hash — the signature signs the id, it cannot be part of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Identity (email) that produced `signature`; its public key lives in the
    /// synced identity chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
}

/// Typed view of a known operation. Unknown kinds have no `OpKind` and are
/// skipped on replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    /// Initial field snapshot for a freshly created ticket.
    Create { fields: BTreeMap<String, String> },
    /// Set one or more scalar fields (compound edit per user command).
    SetField { fields: BTreeMap<String, String> },
    /// Remove one or more scalar fields.
    ClearField { fields: Vec<String> },
    /// Delete the ticket (a soft Deletion: keys, history and relations stay).
    /// Wire kind `del`; binaries that predate it skip it and see the ticket
    /// alive.
    Delete { reason: Option<String> },
    /// Undo a Deletion. Wire kind `undel`.
    Undelete,
}

impl OpKind {
    fn kind_str(&self) -> &'static str {
        match self {
            OpKind::Create { .. } => "create",
            OpKind::SetField { .. } => "set-field",
            OpKind::ClearField { .. } => "clear-field",
            OpKind::Delete { .. } => "del",
            OpKind::Undelete => "undel",
        }
    }

    fn payload(&self) -> Value {
        match self {
            OpKind::Create { fields } | OpKind::SetField { fields } => json!({ "fields": fields }),
            OpKind::ClearField { fields } => json!({ "fields": fields }),
            OpKind::Delete {
                reason: Some(reason),
            } => json!({ "reason": reason }),
            OpKind::Delete { reason: None } | OpKind::Undelete => json!({}),
        }
    }

    fn decode(kind: &str, payload: &Value) -> Option<OpKind> {
        match kind {
            "create" => parse_field_map(payload).map(|fields| OpKind::Create { fields }),
            "set-field" => parse_field_map(payload).map(|fields| OpKind::SetField { fields }),
            "clear-field" => parse_field_list(payload).map(|fields| OpKind::ClearField { fields }),
            "del" => {
                let reason = match payload.get("reason") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(v.as_str()?.to_string()),
                };
                Some(OpKind::Delete { reason })
            }
            "undel" => Some(OpKind::Undelete),
            _ => None,
        }
    }
}

fn parse_field_map(payload: &Value) -> Option<BTreeMap<String, String>> {
    let obj = payload.get("fields")?.as_object()?;
    let mut map = BTreeMap::new();
    for (k, v) in obj {
        map.insert(k.clone(), v.as_str()?.to_string());
    }
    Some(map)
}

fn parse_field_list(payload: &Value) -> Option<Vec<String>> {
    payload
        .get("fields")?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// Fields fed to the content hash — everything but the derived `id`.
#[derive(Serialize)]
struct OpDigest<'a> {
    lamport: Lamport,
    author: &'a str,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    format_version: u32,
    kind: &'a str,
    payload: &'a Value,
}

impl Op {
    /// Build an op from a typed [`OpKind`], computing its content-derived id.
    pub fn new(
        lamport: Lamport,
        author: &str,
        created_at: OffsetDateTime,
        kind: OpKind,
    ) -> Result<Op> {
        let kind_str = kind.kind_str().to_string();
        let payload = kind.payload();
        let digest = OpDigest {
            lamport,
            author,
            created_at,
            format_version: CURRENT_OP_FORMAT,
            kind: &kind_str,
            payload: &payload,
        };
        let id = content_id(&canonical_json(&digest)?).as_str().to_string();
        Ok(Op {
            id,
            lamport,
            author: author.to_string(),
            created_at,
            format_version: CURRENT_OP_FORMAT,
            kind: kind_str,
            payload,
            signature: None,
            signer: None,
        })
    }

    /// Typed view of this op, or `None` if the kind is unknown to this binary.
    #[must_use]
    pub fn decoded(&self) -> Option<OpKind> {
        OpKind::decode(&self.kind, &self.payload)
    }

    /// Recompute the content-derived id from this op's envelope. A healthy op
    /// satisfies `op.id == op.recompute_id()?`; a mismatch means the stored
    /// bytes were tampered with or corrupted (used by `ti verify`).
    pub fn recompute_id(&self) -> Result<String> {
        let digest = OpDigest {
            lamport: self.lamport,
            author: &self.author,
            created_at: self.created_at,
            format_version: self.format_version,
            kind: &self.kind,
            payload: &self.payload,
        };
        Ok(content_id(&canonical_json(&digest)?).as_str().to_string())
    }
}

/// Fold an operation set into the scalar field state it projects to. Ops are
/// sorted by `(lamport, id)` first, so the result is identical on every clone
/// regardless of the order the ops arrived in; unknown kinds are skipped.
#[must_use]
pub fn replay(ops: &[Op]) -> BTreeMap<String, String> {
    let mut sorted: Vec<&Op> = ops.iter().collect();
    sorted.sort_by(|a, b| a.lamport.cmp(&b.lamport).then_with(|| a.id.cmp(&b.id)));
    let mut state: BTreeMap<String, String> = BTreeMap::new();
    for op in sorted {
        match op.decoded() {
            Some(OpKind::Create { fields }) | Some(OpKind::SetField { fields }) => {
                for (k, v) in fields {
                    state.insert(k, v);
                }
            }
            Some(OpKind::ClearField { fields }) => {
                for k in fields {
                    state.remove(&k);
                }
            }
            Some(OpKind::Delete { .. }) | Some(OpKind::Undelete) | None => {}
        }
    }
    state
}

/// The op that leaves a ticket deleted, or `None` when it is not deleted.
///
/// A ticket is deleted iff the highest `(lamport, id)` op among its `del` and
/// `undel` ops is a `del` — equivalently, iff some `del` is not covered by a
/// later `undel`. Other op kinds never compete, so a Deletion wins over edits
/// made concurrently on another clone. The returned op carries who deleted the
/// ticket and when.
#[must_use]
pub fn project_deletion(ops: &[Op]) -> Option<&Op> {
    ops.iter()
        .filter_map(|op| match op.decoded() {
            Some(OpKind::Delete { .. }) => Some((op, true)),
            Some(OpKind::Undelete) => Some((op, false)),
            _ => None,
        })
        .max_by(|(a, _), (b, _)| a.lamport.cmp(&b.lamport).then_with(|| a.id.cmp(&b.id)))
        .and_then(|(op, deleted)| deleted.then_some(op))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use time::OffsetDateTime;

    fn ts() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    fn set_field(lamport: u64, field: &str, value: &str) -> Op {
        let mut fields = BTreeMap::new();
        fields.insert(field.to_string(), value.to_string());
        Op::new(lamport, "a@b.c", ts(), OpKind::SetField { fields }).unwrap()
    }

    #[test]
    fn op_id_is_content_derived_and_stable() {
        let a = set_field(1, "title", "hello");
        let b = set_field(1, "title", "hello");
        assert_eq!(a.id, b.id, "same content → same id");
        let c = set_field(1, "title", "world");
        assert_ne!(a.id, c.id, "different content → different id");
    }

    #[test]
    fn replay_applies_fields_in_lamport_order() {
        let ops = vec![
            set_field(2, "title", "second"),
            set_field(1, "title", "first"),
        ];
        let state = replay(&ops);
        assert_eq!(state.get("title").map(String::as_str), Some("second"));
    }

    #[test]
    fn replay_is_independent_of_input_order() {
        let a = vec![set_field(1, "x", "1"), set_field(2, "x", "2")];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(replay(&a), replay(&b));
    }

    #[test]
    fn replay_unions_concurrent_edits_to_different_fields() {
        // The headline win: two clones edit DIFFERENT scalar fields; both
        // survive a merge instead of one clobbering the other.
        let ops = vec![
            set_field(1, "title", "new title"),
            set_field(1, "priority", "2"),
        ];
        let state = replay(&ops);
        assert_eq!(state.get("title").map(String::as_str), Some("new title"));
        assert_eq!(state.get("priority").map(String::as_str), Some("2"));
    }

    #[test]
    fn replay_breaks_equal_lamport_ties_by_id_deterministically() {
        let op1 = set_field(1, "title", "alpha");
        let op2 = set_field(1, "title", "beta");
        // Whichever id sorts last wins, and the result is the same regardless
        // of input order.
        let forward = replay(&[op1.clone(), op2.clone()]);
        let reverse = replay(&[op2.clone(), op1.clone()]);
        assert_eq!(forward, reverse);
        let winner = if op1.id > op2.id { "alpha" } else { "beta" };
        assert_eq!(forward.get("title").map(String::as_str), Some(winner));
    }

    #[test]
    fn replay_skips_unknown_op_kinds() {
        // Forward-compat: an op kind this binary doesn't understand advances
        // nothing but must not break replay (it stays in the log for others).
        let known = set_field(1, "title", "kept");
        let unknown = Op {
            id: "deadbeef".to_string(),
            lamport: 2,
            author: "a@b.c".to_string(),
            created_at: ts(),
            format_version: 1,
            kind: "future-op-kind".to_string(),
            payload: json!({ "whatever": true }),
            signature: None,
            signer: None,
        };
        let state = replay(&[known, unknown]);
        assert_eq!(state.get("title").map(String::as_str), Some("kept"));
    }

    fn del(lamport: u64) -> Op {
        Op::new(lamport, "a@b.c", ts(), OpKind::Delete { reason: None }).unwrap()
    }

    fn undel(lamport: u64) -> Op {
        Op::new(lamport, "a@b.c", ts(), OpKind::Undelete).unwrap()
    }

    #[test]
    fn delete_and_undelete_round_trip_through_their_wire_kinds() {
        let d = Op::new(
            1,
            "a@b.c",
            ts(),
            OpKind::Delete {
                reason: Some("dup".to_string()),
            },
        )
        .unwrap();
        assert_eq!(d.kind, "del");
        assert_eq!(
            d.decoded(),
            Some(OpKind::Delete {
                reason: Some("dup".to_string())
            })
        );
        let u = undel(2);
        assert_eq!(u.kind, "undel");
        assert_eq!(u.decoded(), Some(OpKind::Undelete));
    }

    #[test]
    fn ticket_without_delete_ops_is_not_deleted() {
        assert!(project_deletion(&[set_field(0, "title", "t")]).is_none());
    }

    #[test]
    fn replay_with_delete_last_yields_deleted() {
        let ops = [set_field(0, "title", "t"), undel(1), del(2)];
        let winner = project_deletion(&ops).expect("deleted");
        assert_eq!(winner.lamport, 2);
    }

    #[test]
    fn undelete_after_delete_brings_the_ticket_back() {
        let ops = [set_field(0, "title", "t"), del(1), undel(2)];
        assert!(project_deletion(&ops).is_none());
    }

    #[test]
    fn deletion_ignores_later_field_edits() {
        // Deletion wins over concurrent edits: only del/undel ops compete.
        let ops = [del(1), set_field(5, "title", "edited")];
        assert!(project_deletion(&ops).is_some());
    }

    #[test]
    fn deletion_is_independent_of_input_order() {
        let ops = [undel(3), del(1), del(4), undel(2)];
        let mut reversed = ops.clone();
        reversed.reverse();
        assert_eq!(project_deletion(&ops).map(|o| o.lamport), Some(4));
        assert_eq!(project_deletion(&reversed).map(|o| o.lamport), Some(4));
    }

    #[test]
    fn deletion_breaks_equal_lamport_ties_by_id() {
        let d = del(3);
        let u = undel(3);
        let expect_deleted = d.id > u.id;
        let ops = [d.clone(), u.clone()];
        assert_eq!(project_deletion(&ops).is_some(), expect_deleted);
        assert_eq!(project_deletion(&[u, d]).is_some(), expect_deleted);
    }

    #[test]
    fn replay_ignores_delete_ops_for_scalar_fields() {
        let ops = [set_field(0, "title", "t"), del(1)];
        assert_eq!(replay(&ops).get("title").map(String::as_str), Some("t"));
    }

    #[test]
    fn replay_clear_removes_field() {
        let set = set_field(1, "milestone", "v1");
        let clear = Op::new(
            2,
            "a@b.c",
            ts(),
            OpKind::ClearField {
                fields: vec!["milestone".to_string()],
            },
        )
        .unwrap();
        let state = replay(&[set, clear]);
        assert!(!state.contains_key("milestone"));
    }

    #[test]
    fn content_id_is_deterministic() {
        let a = content_id(b"hello op");
        let b = content_id(b"hello op");
        assert_eq!(a, b);
    }

    #[test]
    fn content_id_differs_on_single_byte_change() {
        let a = content_id(b"hello op");
        let b = content_id(b"hello oq");
        assert_ne!(a, b);
    }

    #[test]
    fn content_id_is_sixty_four_hex_chars() {
        let id = content_id(b"anything");
        let s = id.as_str();
        assert_eq!(s.len(), 64);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn short_id_is_twelve_hex_chars() {
        let id = content_id(b"anything");
        let short = id.short();
        assert_eq!(short.len(), 12);
        assert!(id.as_str().starts_with(&short));
    }

    #[test]
    fn canonical_json_has_sorted_keys() {
        let mut map = BTreeMap::new();
        map.insert("zeta".to_string(), "1".to_string());
        map.insert("alpha".to_string(), "2".to_string());
        let bytes = canonical_json(&map).expect("serialize");
        let s = String::from_utf8(bytes).expect("utf8");
        // alpha must appear before zeta regardless of insertion order.
        let alpha = s.find("alpha").expect("alpha present");
        let zeta = s.find("zeta").expect("zeta present");
        assert!(alpha < zeta, "canonical encoding must sort keys: {s}");
    }

    #[test]
    fn canonical_json_is_insertion_order_independent() {
        let mut a = BTreeMap::new();
        a.insert("b".to_string(), "2".to_string());
        a.insert("a".to_string(), "1".to_string());

        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "1".to_string());
        b.insert("b".to_string(), "2".to_string());

        assert_eq!(
            canonical_json(&a).unwrap(),
            canonical_json(&b).unwrap(),
            "same content in different insertion order must encode identically"
        );
    }

    #[test]
    fn content_id_over_canonical_json_is_stable() {
        let mut map = BTreeMap::new();
        map.insert("field".to_string(), "value".to_string());
        let id1 = content_id(&canonical_json(&map).unwrap());
        let id2 = content_id(&canonical_json(&map).unwrap());
        assert_eq!(id1, id2);
    }
}
