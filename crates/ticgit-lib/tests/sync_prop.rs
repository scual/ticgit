//! Two clones of one repository syncing through a shared bare remote, driven
//! through the library seam (`TicketStore::pull` then `TicketStore::push`,
//! which is what `ti sync` runs).

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use ticgit_lib::{NewTicketOpts, Session, TicketStore};

/// A bare remote shared by the clones of one test.
struct Remote {
    dir: TempDir,
}

impl Remote {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("remote tempdir");
        git(dir.path(), &["init", "--bare", "--quiet"]);
        Self { dir }
    }

    /// A fresh clone configured the way `ti init` configures one: `origin`
    /// points at the remote and is the git-meta remote.
    fn clone(&self, email: &str) -> Clone {
        let dir = tempfile::tempdir().expect("clone tempdir");
        let p = dir.path();
        git(p, &["init", "--quiet", "-b", "main"]);
        git(p, &["config", "user.email", email]);
        git(p, &["config", "user.name", email]);
        git(p, &["commit", "--allow-empty", "-m", "init", "--quiet"]);
        let url = self.dir.path().to_string_lossy().to_string();
        git(p, &["remote", "add", "origin", &url]);
        git(p, &["config", "meta.namespace", "meta"]);
        git(
            p,
            &[
                "config",
                "--add",
                "remote.origin.fetch",
                "+refs/meta/main:refs/meta/remotes/main",
            ],
        );
        git(p, &["config", "remote.origin.meta", "true"]);
        git(p, &["config", "remote.origin.promisor", "true"]);
        git(
            p,
            &["config", "remote.origin.partialclonefilter", "blob:none"],
        );
        let session = Session::open(p.join(".git")).expect("session");
        let store = TicketStore::from_session(session).expect("store");
        Clone { store, _dir: dir }
    }
}

/// One clone: its store, and the tempdir that keeps the repository alive.
struct Clone {
    store: TicketStore,
    _dir: TempDir,
}

impl Clone {
    /// `ti sync`: pull, then push. A remote that has never received tickets
    /// has no meta ref to pull yet; the push creates it.
    fn sync(&self) -> usize {
        self.sync_with(|| {})
    }

    /// A sync with `between` run in the race window after the pull and
    /// before the push. Returns how many times the push merged and retried.
    fn sync_with(&self, between: impl FnOnce()) -> usize {
        if let Err(err) = self.store.pull(None) {
            assert!(err.is_missing_remote_ref(), "pull failed: {err}");
        }
        between();
        self.store.push(None, false, false).expect("push")
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// The process cwd is never a clone: each store syncs the repository it was
/// opened on.
#[test]
fn two_clones_sync_through_a_bare_remote_regardless_of_cwd() {
    let remote = Remote::new();
    let a = remote.clone("a@example.com");
    let b = remote.clone("b@example.com");

    let t = a.store.create("shared", NewTicketOpts::default()).unwrap();
    a.store.add_comment(&t.id, "from a").unwrap();
    a.sync();
    b.sync();

    let seen = b.store.load(&t.id).expect("b has a's ticket");
    assert_eq!(seen.comments[0].body, "from a");
}
