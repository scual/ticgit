//! Operation-log primitives.
//!
//! Step 1 of the op-log roadmap (ticket `ca6420`): content-derived identifiers.
//! An operation's identity is the SHA-256 of its canonical byte encoding, so two
//! clones that independently record the same operation derive the same id — no
//! central coordination, automatic dedupe, and a deterministic tiebreaker for
//! equal Lamport values.
//!
//! This module intentionally does NOT define the operation payload shape yet;
//! that lands with the flagship (ticket `b209a5`). It provides only the hashing
//! and canonical-encoding primitives that step consumes.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
