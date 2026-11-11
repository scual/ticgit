//! git-meta's serialized tree layout, read directly from git objects.
//!
//! The push guard, the additive reindex and the reset guard read meta refs
//! without materializing them into the store. They meet git-meta's on-disk
//! markers here and nowhere else: under a key's directory, `__value` holds a
//! string, `__list` a list and `__set` a set; `__tombstones` holds recorded
//! removals, and a key tombstone ends in `__deleted`. Every other
//! `__`-prefixed name is git-meta's own. Reading a tree maps git errors to
//! [`Error::InvalidValue`] in one place.

use gix::bstr::{BString, ByteSlice};
use gix::ObjectId;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::keys;

/// Directory entry holding a string value.
const VALUE: &str = "__value";
/// Directory entry holding a list value.
const LIST: &str = "__list";
/// Directory entry holding a set value.
const SET: &str = "__set";
/// Directory holding recorded removals.
const TOMBSTONES: &str = "__tombstones";
/// Last segment of a key tombstone.
const DELETED: &str = "__deleted";

/// One entry of a tree.
pub(crate) struct TreeEntry {
    pub name: BString,
    pub oid: ObjectId,
    pub is_tree: bool,
}

/// The entries of tree `oid`.
pub(crate) fn entries(repo: &gix::Repository, oid: ObjectId) -> Result<Vec<TreeEntry>> {
    let tree = repo
        .find_tree(oid)
        .map_err(|e| Error::InvalidValue(format!("reading tree {oid}: {e}")))?;
    tree_entries(&tree)
}

fn tree_entries(tree: &gix::Tree<'_>) -> Result<Vec<TreeEntry>> {
    tree.iter()
        .map(|entry| {
            let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
            Ok(TreeEntry {
                name: entry.filename().to_owned(),
                oid: entry.object_id(),
                is_tree: entry.mode().is_tree(),
            })
        })
        .collect()
}

/// Every ticket directory under [`keys::tickets_tree_path`] in the tree at
/// `ref_name`, as `(ticket id, directory oid)`. Empty when the ref, or that
/// subtree, does not exist (a remote that has never received tickets).
pub(crate) fn ticket_dirs(repo: &gix::Repository, ref_name: &str) -> Result<Vec<(Uuid, ObjectId)>> {
    let spec = format!("{ref_name}:{}", keys::tickets_tree_path());
    let Ok(id) = repo.rev_parse_single(spec.as_bytes()) else {
        return Ok(Vec::new());
    };
    let tree = id
        .object()
        .map_err(|e| Error::InvalidValue(format!("reading {ref_name}: {e}")))?
        .into_tree();
    Ok(tree_entries(&tree)?
        .into_iter()
        .filter(|e| e.is_tree)
        .filter_map(|e| Some((Uuid::parse_str(&e.name.to_str_lossy()).ok()?, e.oid)))
        .collect())
}

/// Like [`ticket_dirs`], but only the directories holding at least one key
/// (a `__value`, `__list` or `__set` somewhere below). A directory with
/// nothing but tombstones — another clone's record of removing a relation or
/// tag on a ticket its store never held — is no ticket: it materializes to
/// nothing and a serialize never reproduces it, so the guards that compare
/// ticket ids must not count it. Reads trees only, never blobs.
pub(crate) fn ticket_dirs_with_keys(
    repo: &gix::Repository,
    ref_name: &str,
) -> Result<Vec<(Uuid, ObjectId)>> {
    let mut out = Vec::new();
    for (id, dir) in ticket_dirs(repo, ref_name)? {
        let mut keys = Vec::new();
        collect_key_dirs(repo, dir, &mut Vec::new(), &mut keys)?;
        if !keys.is_empty() {
            out.push((id, dir));
        }
    }
    Ok(out)
}

/// Collect every key directory under a ticket's serialized tree as
/// `(key segments below the ticket, directory oid)`. A directory is a key when
/// it holds a value marker (`__value`, `__list`, `__set`); any other
/// subdirectory, except git-meta's own `__`-prefixed ones, is a further key
/// segment. Reads trees only, never blobs.
pub(crate) fn collect_key_dirs(
    repo: &gix::Repository,
    dir: ObjectId,
    segments: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, ObjectId)>,
) -> Result<()> {
    let mut is_key = false;
    let mut children = Vec::new();
    for entry in entries(repo, dir)? {
        let name = entry.name.to_str_lossy().to_string();
        match name.as_str() {
            VALUE | LIST | SET => is_key = true,
            _ if name.starts_with("__") => {}
            _ if entry.is_tree => children.push((name, entry.oid)),
            _ => {}
        }
    }
    if is_key && !segments.is_empty() {
        out.push((segments.clone(), dir));
    }
    for (name, oid) in children {
        segments.push(name);
        collect_key_dirs(repo, oid, segments, out)?;
        segments.pop();
    }
    Ok(())
}

/// For a tombstone blob path, the path of the value it removes: a key's
/// directory for a key tombstone (`<base>/__tombstones/<key>/__deleted`), a
/// list entry (`<key>/__list/__tombstones/<entry>`) or a set member
/// (`<key>/__tombstones/<member>`). `None` when `path` is no tombstone.
pub(crate) fn removed_value_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    let at = segments.iter().position(|s| *s == TOMBSTONES)?;
    let (before, after) = (&segments[..at], &segments[at + 1..]);
    let removed: Vec<&str> = if after.last() == Some(&DELETED) {
        before
            .iter()
            .chain(&after[..after.len() - 1])
            .copied()
            .collect()
    } else if before.last() == Some(&LIST) {
        before.iter().chain(after).copied().collect()
    } else {
        before
            .iter()
            .chain(std::iter::once(&SET))
            .chain(after)
            .copied()
            .collect()
    };
    Some(removed.join("/"))
}

#[cfg(test)]
mod tests {
    use super::removed_value_path;

    #[test]
    fn tombstone_paths_name_the_value_they_remove() {
        assert_eq!(
            removed_value_path("project/__tombstones/ticgit/tickets/x/title/__deleted").as_deref(),
            Some("project/ticgit/tickets/x/title")
        );
        assert_eq!(
            removed_value_path("t/comments/__list/__tombstones/e1").as_deref(),
            Some("t/comments/__list/e1")
        );
        assert_eq!(
            removed_value_path("t/tags/__tombstones/m1").as_deref(),
            Some("t/tags/__set/m1")
        );
        assert_eq!(removed_value_path("t/tags/__set/m1"), None);
    }
}
