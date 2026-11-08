//! How the local store stands against the meta remote, for `ti reindex`.
//!
//! [`TicketStore::fetch_remote_tip`] brings the remote tracking ref up to date
//! without merging, so a Reindex reads what the remote holds now rather than
//! what the last Sync fetched.

use crate::error::Result;
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
        let ns = self.session().namespace();
        let refspec = format!("+refs/{ns}/main:refs/{ns}/remotes/main");
        git_utils::run_git(repo, &["fetch", "--quiet", &remote, &refspec])?;
        // A blobless clone fetches trees only; read the tip's blobs now so the
        // reindex does not have to backfill them one ticket at a time.
        git_utils::hydrate_tip_blobs(repo, &remote, &format!("{ns}/remotes/main"))?;
        Ok(true)
    }
}
