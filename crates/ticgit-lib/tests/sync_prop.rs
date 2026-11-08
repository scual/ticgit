//! Two clones of one repository syncing through a shared bare remote, driven
//! through the library seam (`TicketStore::pull` then `TicketStore::push`,
//! which is what `ti sync` runs).
//!
//! The property test generates random edits and Syncs on either clone, with
//! Race windows, and checks the Sync invariants of `docs/spec/Sync.tla`
//! (ADR 0003) against an in-memory record of what each clone did.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use tempfile::TempDir;
use ticgit_lib::{NewTicketOpts, OpKind, Session, TicketStore};
use uuid::Uuid;

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
        Clone {
            store,
            dir,
            remote: self.dir.path().to_owned(),
        }
    }
}

/// One clone: its store, the tempdir that keeps the repository alive, and
/// the path of its remote.
struct Clone {
    store: TicketStore,
    dir: TempDir,
    remote: PathBuf,
}

impl Clone {
    /// `ti sync`: pull, then push. Returns how many times the push merged
    /// and retried because the remote advanced after the pull.
    fn sync(&self) -> usize {
        self.pull();
        self.push()
    }

    /// The first half of a Sync. A remote that has never received tickets
    /// has no meta ref to pull yet; the push creates it.
    fn pull(&self) {
        if let Err(err) = self.store.pull(None) {
            assert!(err.is_missing_remote_ref(), "pull failed: {err}");
        }
    }

    /// The second half of a Sync, with the push guards on (no implicit
    /// deletion, verify gate).
    fn push(&self) -> usize {
        self.store.push(None, false, false).expect("push")
    }

    /// After a Sync, what this clone serialized is in the remote's history:
    /// the push landed, or had nothing new to land. A push that reported
    /// success while git rejected it fails here, even though a later Sync
    /// would make up for it.
    fn assert_pushed(&self, name: &str) {
        let local = git_out(self.dir.path(), &["rev-parse", "refs/meta/local/main"]);
        let landed = Command::new("git")
            .args(["merge-base", "--is-ancestor", &local, "refs/meta/main"])
            .current_dir(&self.remote)
            .output()
            .expect("spawn git")
            .status
            .success();
        assert!(
            landed,
            "{name} synced, but its tip {local} is not on the remote"
        );
    }
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
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

// ---------------------------------------------------------------------------
// The property: random edits and Syncs on two clones keep the Sync invariants
// ---------------------------------------------------------------------------

/// Which clone a step runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    A,
    B,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// A local edit. Tickets are picked from the tickets the clone holds, so a
/// pick means "some ticket this clone can see", whatever its id turned out to
/// be.
#[derive(Debug, Clone)]
enum Edit {
    Create,
    Comment(Index),
    Tag(Index, u8),
    /// Make the first pick a Sub-issue of the second.
    Subissue(Index, Index),
    /// The first pick depends on the second.
    Depends(Index, Index),
    Delete(Index),
    Undelete(Index),
}

#[derive(Debug, Clone)]
enum Step {
    Edit(Side, Edit),
    /// A Sync. With `Some(edit)`, the Race window: after this clone's pull
    /// and before its push, the other clone makes the edit and syncs, so the
    /// remote advances underneath the push.
    Sync(Side, Option<Edit>),
}

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::A), Just(Side::B)]
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        3 => Just(Edit::Create),
        3 => any::<Index>().prop_map(Edit::Comment),
        2 => (any::<Index>(), 0..3u8).prop_map(|(t, g)| Edit::Tag(t, g)),
        2 => (any::<Index>(), any::<Index>()).prop_map(|(c, p)| Edit::Subissue(c, p)),
        2 => (any::<Index>(), any::<Index>()).prop_map(|(t, d)| Edit::Depends(t, d)),
        2 => any::<Index>().prop_map(Edit::Delete),
        2 => any::<Index>().prop_map(Edit::Undelete),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        5 => (side(), edit()).prop_map(|(s, e)| Step::Edit(s, e)),
        2 => side().prop_map(|s| Step::Sync(s, None)),
        2 => (side(), edit()).prop_map(|(s, e)| Step::Sync(s, Some(e))),
    ]
}

/// What one clone did: everything it must never lose, and that both clones
/// must hold once they have synced (NoLostLocalOp).
#[derive(Debug, Default, Clone)]
struct Did {
    tickets: BTreeSet<Uuid>,
    comments: BTreeSet<(Uuid, String)>,
    tags: BTreeSet<(Uuid, String)>,
    /// child -> parent
    parents: BTreeMap<Uuid, Uuid>,
    /// (dependent, dependency)
    depends: BTreeSet<(Uuid, Uuid)>,
    /// Operation ids per ticket: create, parent, del and undel operations.
    ops: BTreeMap<Uuid, BTreeSet<String>>,
}

impl Did {
    fn union(&self, other: &Did) -> Did {
        let mut out = self.clone();
        out.tickets.extend(&other.tickets);
        out.comments.extend(other.comments.iter().cloned());
        out.tags.extend(other.tags.iter().cloned());
        out.parents.extend(&other.parents);
        out.depends.extend(&other.depends);
        for (id, ops) in &other.ops {
            out.ops.entry(*id).or_default().extend(ops.iter().cloned());
        }
        out
    }
}

/// The in-memory model of a run.
#[derive(Debug, Default)]
struct Model {
    did: [Did; 2],
    /// Creation order of every ticket, the order Sub-issue and Dependency
    /// picks respect (see [`apply_edit`]).
    order: Vec<Uuid>,
    /// Tickets some clone's index has held deleted (NoResurrection).
    seen_deleted: [BTreeSet<Uuid>; 2],
    /// Tickets an Undeletion was made for, on either clone.
    undeleted: BTreeSet<Uuid>,
    /// Children that have been given a parent, on either clone.
    parented: BTreeSet<Uuid>,
    comments_made: usize,
    /// Pushes that had to merge and retry because the remote moved.
    raced_pushes: usize,
}

impl Model {
    fn rank(&self, id: &Uuid) -> usize {
        self.order
            .iter()
            .position(|t| t == id)
            .expect("ticket in model")
    }
}

/// The tickets `clone` holds, deleted ones included, in creation order.
fn held(clone: &Clone, model: &Model) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = clone.store.list().unwrap().iter().map(|t| t.id).collect();
    ids.sort_by_key(|id| model.rank(id));
    ids
}

fn record_ops(clone: &Clone, side: Side, id: Uuid, model: &mut Model) {
    let ops = clone.store.load_ops(&id).unwrap();
    model.did[side.ix()]
        .ops
        .entry(id)
        .or_default()
        .extend(ops.into_iter().map(|op| op.id));
}

/// Apply `edit` on `clone` and record it in the model. Returns false when the
/// edit does not apply in the clone's current state (no tickets to pick, a
/// delete of a deleted ticket, ...) and nothing was done.
///
/// Sub-issues and Dependencies only point from a later ticket to an earlier
/// one, and a ticket gets a parent at most once across both clones: two
/// clones re-parenting the same ticket, or closing a cycle, concurrently is
/// a conflict the merge does not resolve today (`ti verify --repair`,
/// ticket dab3f8), not a lost operation.
fn apply_edit(clone: &Clone, side: Side, edit: &Edit, model: &mut Model) -> bool {
    let tickets = held(clone, model);
    let pick = |ix: &Index| tickets.get(ix.index(tickets.len().max(1))).copied();
    let store = &clone.store;
    let did = side.ix();
    match edit {
        Edit::Create => {
            let title = format!("T{}", model.order.len());
            let t = store.create(&title, NewTicketOpts::default()).unwrap();
            model.order.push(t.id);
            model.did[did].tickets.insert(t.id);
            record_ops(clone, side, t.id, model);
        }
        Edit::Comment(ix) => {
            let Some(id) = pick(ix) else { return false };
            model.comments_made += 1;
            let body = format!("comment {} from {side:?}", model.comments_made);
            store.add_comment(&id, &body).unwrap();
            model.did[did].comments.insert((id, body));
        }
        Edit::Tag(ix, g) => {
            let Some(id) = pick(ix) else { return false };
            let tag = format!("tag{g}");
            store.add_tag(&id, &tag).unwrap();
            model.did[did].tags.insert((id, tag));
        }
        Edit::Subissue(c, p) => {
            let (Some(child), Some(parent)) = (pick(c), pick(p)) else {
                return false;
            };
            if model.rank(&parent) >= model.rank(&child) || model.parented.contains(&child) {
                return false;
            }
            store.set_parent(&child, &parent).unwrap();
            model.parented.insert(child);
            model.did[did].parents.insert(child, parent);
            record_ops(clone, side, child, model);
        }
        Edit::Depends(t, d) => {
            let (Some(id), Some(dep)) = (pick(t), pick(d)) else {
                return false;
            };
            if model.rank(&dep) >= model.rank(&id) {
                return false;
            }
            store.add_dependency(&id, &dep).unwrap();
            model.did[did].depends.insert((id, dep));
        }
        Edit::Delete(ix) => {
            let Some(id) = pick(ix) else { return false };
            if store.load(&id).unwrap().is_deleted() {
                return false;
            }
            store.delete_ticket(&id, None).unwrap();
            record_ops(clone, side, id, model);
        }
        Edit::Undelete(ix) => {
            let Some(id) = pick(ix) else { return false };
            if !store.load(&id).unwrap().is_deleted() {
                return false;
            }
            store.undelete_ticket(&id).unwrap();
            model.undeleted.insert(id);
            record_ops(clone, side, id, model);
        }
    }
    true
}

/// Remember which tickets `clone`'s index holds deleted right now.
fn observe_deletions(clone: &Clone, side: Side, model: &mut Model) {
    for t in clone.store.list().unwrap() {
        if t.is_deleted() {
            model.seen_deleted[side.ix()].insert(t.id);
        }
    }
}

/// NoLostLocalOp: every ticket, comment, tag, Sub-issue, Dependency and
/// operation in `did` is in `clone`'s index.
fn assert_holds(clone: &Clone, name: &str, did: &Did) {
    let store = &clone.store;
    for id in &did.tickets {
        store
            .load(id)
            .unwrap_or_else(|e| panic!("NoLostLocalOp: {name} lost ticket {id}: {e}"));
    }
    for (id, body) in &did.comments {
        let t = store.load(id).unwrap();
        assert!(
            t.comments.iter().any(|c| &c.body == body),
            "NoLostLocalOp: {name} lost comment {body:?} on {id}"
        );
    }
    for (id, tag) in &did.tags {
        let t = store.load(id).unwrap();
        assert!(
            t.tags.contains(tag),
            "NoLostLocalOp: {name} lost tag {tag} on {id}"
        );
    }
    for (child, parent) in &did.parents {
        let c = store.load(child).unwrap();
        let p = store.load(parent).unwrap();
        assert_eq!(
            c.parent,
            Some(*parent),
            "NoLostLocalOp: {name} lost the parent of {child}"
        );
        assert!(
            p.children.contains(child),
            "NoLostLocalOp: {name} lost Sub-issue {child} from {parent}'s children"
        );
    }
    for (id, dep) in &did.depends {
        let t = store.load(id).unwrap();
        let d = store.load(dep).unwrap();
        assert!(
            t.depends_on.contains(dep),
            "NoLostLocalOp: {name} lost Dependency {id} -> {dep}"
        );
        assert!(
            d.blocks.contains(id),
            "NoLostLocalOp: {name} lost {dep} blocks {id}"
        );
    }
    for (id, ops) in &did.ops {
        let have: BTreeSet<String> = store
            .load_ops(id)
            .unwrap()
            .into_iter()
            .map(|o| o.id)
            .collect();
        let lost: Vec<_> = ops.difference(&have).collect();
        assert!(
            lost.is_empty(),
            "NoLostLocalOp: {name} lost operations {lost:?} of {id}"
        );
    }
}

/// NoResurrection: a ticket `clone`'s index has held deleted is visible there
/// only if an Undeletion is in its log, and one nobody undeleted stays
/// deleted.
fn assert_no_resurrection(clone: &Clone, name: &str, seen_deleted: &BTreeSet<Uuid>, model: &Model) {
    for id in seen_deleted {
        let t = clone.store.load(id).unwrap();
        if t.is_deleted() {
            continue;
        }
        let undeleted = clone
            .store
            .load_ops(id)
            .unwrap()
            .iter()
            .any(|op| matches!(op.decoded(), Some(OpKind::Undelete)));
        assert!(
            undeleted && model.undeleted.contains(id),
            "NoResurrection: deleted ticket {id} is visible on {name} without an Undeletion"
        );
    }
}

/// Check the invariants that hold in every state for the clone that just
/// synced: it lost nothing it did, and resurrected nothing.
fn check_clone(clone: &Clone, side: Side, model: &mut Model) {
    let name = format!("{side:?}");
    assert_holds(clone, &name, &model.did[side.ix()]);
    assert_no_resurrection(clone, &name, &model.seen_deleted[side.ix()], model);
    observe_deletions(clone, side, model);
}

/// Run `steps` on two fresh clones of one remote, sync both until quiescent,
/// and check NoLostLocalOp, NoResurrection, NoAsymmetry and Convergence.
/// Returns the model, for callers that check what was exercised.
fn run(steps: &[Step]) -> Model {
    let remote = Remote::new();
    let clones = [remote.clone("a@example.com"), remote.clone("b@example.com")];
    let mut model = Model::default();

    for step in steps {
        match step {
            Step::Edit(side, edit) => {
                let clone = &clones[side.ix()];
                apply_edit(clone, *side, edit, &mut model);
                observe_deletions(clone, *side, &mut model);
            }
            Step::Sync(side, race) => {
                let clone = &clones[side.ix()];
                clone.pull();
                check_clone(clone, *side, &mut model);
                if let Some(edit) = race {
                    // The Race window: the other clone edits and syncs between
                    // this clone's pull and push. An edit that does not apply
                    // becomes a create, so the remote always advances.
                    let other = side.other();
                    let racer = &clones[other.ix()];
                    if !apply_edit(racer, other, edit, &mut model) {
                        apply_edit(racer, other, &Edit::Create, &mut model);
                    }
                    racer.sync();
                    racer.assert_pushed(&format!("{other:?}"));
                    check_clone(racer, other, &mut model);
                }
                if clone.push() > 0 {
                    model.raced_pushes += 1;
                }
                clone.assert_pushed(&format!("{side:?}"));
                check_clone(clone, *side, &mut model);
            }
        }
    }

    // Quiesce: after A, B, A sync, each holds everything and the remote has
    // not moved since either pushed.
    for side in [Side::A, Side::B, Side::A] {
        clones[side.ix()].sync();
        check_clone(&clones[side.ix()], side, &mut model);
    }

    let everything = model.did[0].union(&model.did[1]);
    let seen_deleted: BTreeSet<Uuid> = model.seen_deleted[0]
        .union(&model.seen_deleted[1])
        .copied()
        .collect();
    for side in [Side::A, Side::B] {
        let clone = &clones[side.ix()];
        let name = format!("{side:?}");
        // NoLostLocalOp, across clones: what either did is on both.
        assert_holds(clone, &name, &everything);
        // NoResurrection, across clones: a Deletion either clone held wins
        // on both unless an Undeletion followed.
        assert_no_resurrection(clone, &name, &seen_deleted, &model);
        // NoAsymmetry: verify reports parent/children and depends-on/blocks
        // asymmetry (and any ticket that does not replay).
        let failing: Vec<_> = clone
            .store
            .verify()
            .unwrap()
            .into_iter()
            .filter(|o| !o.ok)
            .collect();
        assert!(
            failing.is_empty(),
            "NoAsymmetry: {name} fails verify: {failing:?}"
        );
    }
    // Convergence: both clones project identical tickets.
    let a = clones[0].store.list().unwrap();
    let b = clones[1].store.list().unwrap();
    assert_eq!(a, b, "Convergence: the clones project different tickets");
    assert_eq!(
        a.iter().map(|t| t.id).collect::<BTreeSet<_>>(),
        model.order.iter().copied().collect(),
        "Convergence: the clones hold other tickets than were created"
    );

    model
}

/// 32 cases by default; `PROPTEST_CASES` overrides.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

#[test]
fn random_edits_and_syncs_on_two_clones_keep_the_sync_invariants() {
    let cases = cases();
    let mut runner = TestRunner::new(Config {
        cases,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..Config::default()
    });
    let raced = Cell::new(0usize);
    let result = runner.run(&prop::collection::vec(step(), 1..12), |steps| {
        let model = run(&steps);
        raced.set(raced.get() + model.raced_pushes);
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }
    // Race windows are generated often enough that a run of this size
    // always has pushes that merged a remote that moved after their pull.
    if cases >= 16 {
        assert!(
            raced.get() > 0,
            "no generated case exercised the Race window"
        );
    }
}
