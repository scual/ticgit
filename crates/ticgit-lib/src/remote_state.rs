//! How the local store stands against the meta remote, for `ti reindex`.
//!
//! [`TicketStore::fetch_remote_tip`] brings the remote tracking ref up to date
//! without merging, so a Reindex reads what the remote holds now rather than
//! what the last Sync fetched. [`TicketStore::unsynced_changes`] counts what
//! the store holds that the remote lacks, which is what the wholesale
//! [`reindex_reset`](TicketStore::reindex_reset) would drop.

use std::collections::BTreeMap;

use gix::bstr::BString;

use crate::error::{Error, Result};
use crate::keys;
use crate::store::TicketStore;

impl TicketStore {
    /// Fetch the meta remote's tip into the tracking ref
    /// `refs/{ns}/remotes/main`, without merging it into the store.
    ///
    /// The fetch is forced, so a remote that was moved back (a restore from a
    /// `refs/ticgit-backup/` snapshot) is followed too. Returns `Ok(false)`
    /// when no meta remote is configured; an error means the remote could not
    /// be reached, and the existing tracking ref is left as it was.
    pub fn fetch_remote_tip(&self) -> Result<bool> {
        use git_meta_lib::git_utils;

        let repo = self.session().repo();
        if git_utils::list_meta_remotes(repo)?.is_empty() {
            return Ok(false);
        }
        let remote = self.session().resolve_remote(None)?;
        self.fetch_meta_tip(&remote, &["--quiet"])?;
        // A blobless clone fetches trees only; read the tip's blobs now so the
        // reindex does not have to backfill them one ticket at a time.
        let tracking = keys::meta_tracking_ref(self.session().namespace());
        git_utils::hydrate_tip_blobs(repo, &remote, &tracking)?;
        Ok(true)
    }

    /// Force-fetch `remote`'s meta tip into the tracking ref
    /// ([`keys::meta_fetch_refspec`]), passing `options` to `git fetch`.
    pub(crate) fn fetch_meta_tip(&self, remote: &str, options: &[&str]) -> Result<()> {
        let refspec = keys::meta_fetch_refspec(self.session().namespace());
        let mut args = vec!["fetch"];
        args.extend_from_slice(options);
        args.extend([remote, refspec.as_str()]);
        git_meta_lib::git_utils::run_git(self.session().repo(), &args)?;
        Ok(())
    }

    /// How many changes the local store holds that the remote tracking tree
    /// (`refs/{ns}/remotes/main`) lacks: what
    /// [`reindex_reset`](Self::reindex_reset) would drop.
    ///
    /// The store is serialized in full, the way a push would write it, into a
    /// tree that no ref points at (the store and every ref are left as they
    /// are). Each value in that tree is one change when the remote tree lacks
    /// it or holds something else at its path: an operation, a comment, a set
    /// member or a string value. A tombstone (a recorded removal) is one
    /// change when the remote tree lacks it yet still holds the value it
    /// removes. What only the remote holds is not counted, since the reset
    /// brings it in. With no tracking ref, every local value counts.
    pub fn unsynced_changes(&self) -> Result<usize> {
        use git_meta_lib::serialize::build_filtered_tree;
        use git_meta_lib::tree::filter::{classify_key, parse_filter_rules, MAIN_DEST};

        let store = self.session().store();
        let rules = parse_filter_rules(store)?;
        let pushed = |key: &str| {
            classify_key(key, &rules).is_some_and(|dests| dests.iter().any(|d| d == MAIN_DEST))
        };
        let mut metadata = store.get_all_metadata()?;
        metadata.retain(|e| pushed(&e.key));
        let mut tombstones = store.get_all_tombstones()?;
        tombstones.retain(|e| pushed(&e.key));
        let mut set_tombstones = store.get_all_set_tombstones()?;
        set_tombstones.retain(|e| pushed(&e.key));
        let mut list_tombstones = store.get_all_list_tombstones()?;
        list_tombstones.retain(|e| pushed(&e.key));

        let repo = self.session().repo();
        let local = build_filtered_tree(
            repo,
            &metadata,
            &tombstones,
            &set_tombstones,
            &list_tombstones,
        )?;
        let spec = format!(
            "{}^{{tree}}",
            keys::meta_tracking_ref(self.session().namespace())
        );
        let remote = repo
            .rev_parse_single(spec.as_str())
            .ok()
            .map(|id| id.detach());
        let mut missing = Vec::new();
        blobs_missing_from(repo, "", local, remote, &mut missing)?;
        // A tombstone the remote lacks is a change only while the remote
        // still holds the value it removes; otherwise the remote already
        // shows the removal and the reset loses nothing visible.
        Ok(missing
            .iter()
            .filter(|path| match removed_value_path(path) {
                None => true,
                Some(removed) => remote.is_some_and(|tree| {
                    repo.rev_parse_single(format!("{tree}:{removed}").as_str())
                        .is_ok()
                }),
            })
            .count())
    }
}

/// The paths of the blobs under tree `ours` (prefixed with `prefix`) that
/// tree `theirs` lacks or holds with other content at the same path. `None`
/// stands for an absent tree.
fn blobs_missing_from(
    repo: &gix::Repository,
    prefix: &str,
    ours: gix::ObjectId,
    theirs: Option<gix::ObjectId>,
    out: &mut Vec<String>,
) -> Result<()> {
    if theirs == Some(ours) {
        return Ok(());
    }
    let mut their_entries: BTreeMap<BString, (gix::ObjectId, bool)> = BTreeMap::new();
    if let Some(theirs) = theirs {
        for entry in read_tree(repo, theirs)?.iter() {
            let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
            their_entries.insert(
                entry.filename().to_owned(),
                (entry.object_id(), entry.mode().is_tree()),
            );
        }
    }
    for entry in read_tree(repo, ours)?.iter() {
        let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
        let path = format!("{prefix}{}", entry.filename());
        let theirs = their_entries.get(entry.filename());
        if entry.mode().is_tree() {
            let their_tree = theirs.filter(|(_, is_tree)| *is_tree).map(|(oid, _)| *oid);
            blobs_missing_from(
                repo,
                &format!("{path}/"),
                entry.object_id(),
                their_tree,
                out,
            )?;
        } else if theirs.map(|(oid, _)| *oid) != Some(entry.object_id()) {
            out.push(path);
        }
    }
    Ok(())
}

fn read_tree(repo: &gix::Repository, oid: gix::ObjectId) -> Result<gix::Tree<'_>> {
    repo.find_tree(oid)
        .map_err(|e| Error::InvalidValue(format!("reading tree {oid}: {e}")))
}

/// For a tombstone blob path, the path of the value it removes: a key's
/// directory for a key tombstone (`<base>/__tombstones/<key>/__deleted`), a
/// list entry (`<key>/__list/__tombstones/<entry>`) or a set member
/// (`<key>/__tombstones/<member>`). `None` when `path` is no tombstone.
fn removed_value_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    let at = segments.iter().position(|s| *s == "__tombstones")?;
    let (before, after) = (&segments[..at], &segments[at + 1..]);
    let removed: Vec<&str> = if after.last() == Some(&"__deleted") {
        before
            .iter()
            .chain(&after[..after.len() - 1])
            .copied()
            .collect()
    } else if before.last() == Some(&"__list") {
        before.iter().chain(after).copied().collect()
    } else {
        before
            .iter()
            .chain(std::iter::once(&"__set"))
            .chain(after)
            .copied()
            .collect()
    };
    Some(removed.join("/"))
}
