//! `TicketStore` - the bridge between the [`Ticket`] domain model and a
//! git-meta [`Session`].
//!
//! Every read and write goes through a [`SessionTargetHandle`] scoped to
//! the `project` target. There is no separate index; tickets are
//! discovered by prefix-scanning `ticgit:tickets`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use git_meta_lib::{ListEntry, MetaValue, Session, Target};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::keys;
use crate::oplog::{project_deletion, Op, OpKind};
use crate::review::{
    format_review_revision, parse_review_revision_change, CommitReviewStatus, NewReviewOpts,
    Review, ReviewMessage, ReviewProgress, ReviewRevisionChange, ReviewVcs,
};
use crate::ticket::{
    Comment, CommentBody, Deletion, NewTicketOpts, Ticket, TicketState, TicketStatus,
};
use crate::writeup::{NewWriteupOpts, Writeup, WriteupStatus, WriteupVersion};

/// Remove `value` from the set at `key`, treating an already-absent member (or
/// an absent set) as success. The two sides of a Sub-issue or Dependency are
/// denormalized sets that can disagree after a sync, so every removal in the
/// store must be idempotent or the CLI could never repair a half-present
/// relation.
fn remove_member(p: &git_meta_lib::SessionTargetHandle<'_>, key: &str, value: &str) -> Result<()> {
    match p.set_remove(key, value) {
        Ok(())
        | Err(git_meta_lib::Error::ValueNotFound(_))
        | Err(git_meta_lib::Error::KeyNotFound { .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Basic email format check: must contain exactly one `@` with non-empty
/// local and domain parts.
fn validate_email(email: &str) -> Result<()> {
    let email = email.trim();
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.')
    {
        Ok(())
    } else {
        Err(Error::InvalidValue(format!(
            "`{email}` is not a valid email address (expected user@domain)"
        )))
    }
}

/// Format a list of open sub-issues for the [`Error::OpenSubissues`] message,
/// e.g. `2 open sub-issue(s): a1b2c3 "title", d4e5f6 "other"`.
/// Join tickets as `shortid "title"` for guard error messages.
fn format_ticket_refs(tickets: &[Ticket]) -> String {
    tickets
        .iter()
        .map(|t| format!("{} \"{}\"", t.short_id(), t.title))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Highest per-ticket format version this binary understands. A ticket whose
/// `format-version` field exceeds this is refused with [`Error::FormatTooNew`];
/// absent or lower versions are migrated forward (lazily on read, persisted by
/// `ti migrate`).
pub const CURRENT_TICKET_FORMAT: u32 = 2;

/// Per-ticket outcome of a [`TicketStore::migrate`] run.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationOutcome {
    pub id: Uuid,
    pub short_id: String,
    /// The version marker found, or `None` for an unversioned (legacy) ticket.
    pub from: Option<u32>,
    /// The version the ticket is (or would be) migrated to.
    pub to: u32,
    /// Whether this ticket needs (dry-run) or received (write) a change.
    pub changed: bool,
}

/// Result of folding one fork ticket into the local store with
/// [`TicketStore::merge_fork_ticket`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkMerge {
    /// The ticket was unknown locally and has been imported.
    Imported,
    /// The ticket existed locally and the fork changed it.
    Updated,
    /// The ticket existed locally and already matched the fork.
    Unchanged,
}

/// Scalars a fork overrides when it differs from the local ticket. The local
/// `created-at`/`created-by` and `priority` are deliberately not mirrored.
const FORK_MERGED_SCALARS: [&str; 10] = [
    "title",
    "description",
    "spec",
    "status",
    "state",
    "assigned",
    "closed-by",
    "points",
    "milestone",
    "code",
];

/// Per-ticket outcome of a [`TicketStore::verify`] run.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyOutcome {
    pub id: Uuid,
    pub short_id: String,
    pub ok: bool,
    pub issues: Vec<String>,
    /// Non-fatal notes, e.g. unsigned ops (trusted by default, step 5).
    pub warnings: Vec<String>,
}

/// Read a ticket's declared format version from its raw fields, enforcing the
/// [`Error::FormatTooNew`] guard. Absent marker = current baseline (legacy
/// tickets stay readable); a malformed marker is [`Error::InvalidFormatVersion`].
fn read_format_version(id: Uuid, fields: &[(String, MetaValue)]) -> Result<u32> {
    for (field, value) in fields {
        if field == keys::FORMAT_VERSION_FIELD {
            if let MetaValue::String(s) = value {
                let ver: u32 = s
                    .trim()
                    .parse()
                    .map_err(|_| Error::InvalidFormatVersion(s.clone()))?;
                if ver > CURRENT_TICKET_FORMAT {
                    return Err(Error::FormatTooNew {
                        id,
                        version: ver,
                        supported: CURRENT_TICKET_FORMAT,
                    });
                }
                return Ok(ver);
            }
        }
    }
    Ok(CURRENT_TICKET_FORMAT)
}

/// SSH private key path used to sign ops, from `TICGIT_SIGNING_KEY`. Absent =
/// unsigned ops (trusted by default; `ti verify` flags them).
fn signing_key_from_env() -> Option<std::path::PathBuf> {
    std::env::var_os("TICGIT_SIGNING_KEY")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// How many pre-push remote-tip backup snapshots to retain per namespace;
/// older ones are pruned so the backup refs cannot grow without bound.
const MAX_BACKUP_SNAPSHOTS: usize = 10;

/// How many times [`TicketStore::push`] retries after a non-fast-forward
/// rejection (merging the remote tip in between) before giving up loudly.
const MAX_PUSH_ATTEMPTS: usize = 5;

/// What one additive reindex pass gained.
#[derive(Debug, Default)]
struct GainOutcome {
    /// Keys applied to the index.
    keys: usize,
    /// Tickets skipped because a blob of a key to gain could not be read.
    unreadable: BTreeSet<Uuid>,
}

/// Current time in milliseconds, git-meta's timestamp unit.
fn now_millis() -> i64 {
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// Collect every key directory under a ticket's serialized tree as
/// `(key segments below the ticket, directory oid)`. A directory is a key when
/// it holds a value marker (`__value`, `__list`, `__set`); any other
/// subdirectory, except git-meta's own `__`-prefixed ones, is a further key
/// segment. Reads trees only, never blobs.
fn collect_key_dirs(
    repo: &gix::Repository,
    dir: gix::ObjectId,
    segments: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, gix::ObjectId)>,
) -> Result<()> {
    use gix::bstr::ByteSlice;

    let tree = repo
        .find_tree(dir)
        .map_err(|e| Error::InvalidValue(format!("reading ticket tree: {e}")))?;
    let mut is_key = false;
    let mut children = Vec::new();
    for entry in tree.iter() {
        let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
        let name = entry.filename().to_str_lossy().to_string();
        match name.as_str() {
            "__value" | "__list" | "__set" => is_key = true,
            _ if name.starts_with("__") => {}
            _ if entry.mode().is_tree() => children.push((name, entry.object_id())),
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

/// One ticket's stored keys as read by [`TicketStore::ticket_records`]: its
/// non-operation fields (`(field, value)`) and its operations, unordered.
#[derive(Debug, Default)]
pub(crate) struct TicketRecord {
    pub fields: Vec<(String, MetaValue)>,
    pub ops: Vec<Op>,
}

/// Wraps a [`Session`] and exposes a ticket-shaped API on top of it.
pub struct TicketStore {
    session: Session,
    signing_key: Option<std::path::PathBuf>,
}

impl TicketStore {
    /// Open a store for the git repo discovered from the current working
    /// directory.
    ///
    /// The store is rooted at the repo's **common** git directory, not the
    /// per-worktree git directory. git-meta keeps `git-meta.sqlite` inside the
    /// git-dir it is handed, so opening the per-worktree git-dir (what plain
    /// `gix::discover` yields inside a linked worktree) would give every
    /// worktree its own isolated ticket store. Resolving the common dir keeps
    /// all worktrees of a repo on one shared store.
    pub fn discover() -> Result<Self> {
        let repo = gix::discover(".")
            .map_err(|e| Error::InvalidValue(format!("not inside a git repository: {e}")))?;
        let session = Session::open(repo.common_dir().to_owned())?;
        Self::ensure_schema(&session)?;
        Ok(Self {
            session,
            signing_key: signing_key_from_env(),
        })
    }

    /// Open a store for an already-loaded `gix::Repository` (used in tests
    /// and by host applications that own the repo handle).
    pub fn open(repo: gix::Repository) -> Result<Self> {
        let session = Session::open(repo.path())?;
        Self::ensure_schema(&session)?;
        Ok(Self {
            session,
            signing_key: signing_key_from_env(),
        })
    }

    /// Open a store from an already-built session (lets callers preconfigure
    /// e.g. `with_timestamp` for deterministic tests).
    pub fn from_session(session: Session) -> Result<Self> {
        Self::ensure_schema(&session)?;
        Ok(Self {
            session,
            signing_key: signing_key_from_env(),
        })
    }

    /// Set (or clear) the SSH key used to sign ops. Primarily for tests and
    /// hosts that resolve the key themselves; otherwise `TICGIT_SIGNING_KEY`
    /// is read at construction.
    pub fn set_signing_key(&mut self, key: Option<std::path::PathBuf>) {
        self.signing_key = key;
    }

    /// Borrow the underlying git-meta session.
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The user email this store will record on writes.
    #[must_use]
    pub fn email(&self) -> &str {
        self.session.email()
    }

    fn ensure_schema(session: &Session) -> Result<()> {
        let p = session.target(&Target::project());
        if p.get_value(keys::SCHEMA_VERSION_KEY)?.is_none() {
            p.set(keys::SCHEMA_VERSION_KEY, keys::SCHEMA_VERSION)?;
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // Ticket creation & loading
    // -------------------------------------------------------------------

    /// Create a new ticket. Returns the freshly-loaded ticket.
    pub fn create(&self, title: &str, opts: NewTicketOpts) -> Result<Ticket> {
        let id = Uuid::new_v4();
        let p = self.session.target(&Target::project());
        let now = opts.created_at.unwrap_or_else(OffsetDateTime::now_utc);
        let now_rfc = now
            .format(&Rfc3339)
            .map_err(|e| Error::Time(e.to_string()))?;

        // Validate the parent up front so a rejected sub-issue never leaves a
        // half-written ticket behind. A new ticket is always open, so parenting
        // it under a closed ticket would violate the no-open-children invariant.
        if let Some(parent_id) = opts.parent {
            let parent = self.load(&parent_id)?;
            if parent.status == TicketStatus::Closed {
                return Err(Error::InvalidValue(format!(
                    "cannot add a sub-issue to closed ticket {} (reopen it first)",
                    parent.short_id()
                )));
            }
        }

        // Resolve and validate the assignee up front too, so a rejected nick or
        // malformed email fails before anything is written and never leaves a
        // half-created ticket behind (e.g. an op-less `format-version` shell).
        let resolved_assigned = match opts.assigned.as_deref() {
            Some(a) if !a.is_empty() => {
                let resolved = self.resolve_user(a)?;
                validate_email(&resolved)?;
                Some(resolved)
            }
            _ => None,
        };

        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), title.to_string());
        fields.insert(
            "status".to_string(),
            TicketStatus::Open.as_str().to_string(),
        );
        fields.insert("state".to_string(), TicketState::New.as_str().to_string());
        fields.insert("created-at".to_string(), now_rfc.clone());
        fields.insert("created-by".to_string(), self.session.email().to_string());

        if let Some(resolved) = resolved_assigned {
            fields.insert("assigned".to_string(), resolved);
        }

        if let Some(parent_id) = opts.parent {
            fields.insert("parent".to_string(), parent_id.to_string());
        }

        // The create op is the synced source of truth and is written first:
        // all fallible validation is already done, so this is the first write.
        self.append_op(&id, OpKind::Create { fields })?;

        // Only now stamp the format-version marker. Writing it *after* the
        // create op means no failure path can leave a ticket dir that has a
        // marker but no create op. It stays a plain key (same value on every
        // clone, never a conflict) and gates old binaries off op-based tickets
        // via the FormatTooNew guard.
        p.set(
            &keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD),
            CURRENT_TICKET_FORMAT.to_string().as_str(),
        )?;

        if let Some(parent_id) = opts.parent {
            // Denormalize: add child to parent's children set (stays a set key).
            p.set_add(&keys::ticket_field(&parent_id, "children"), &id.to_string())?;
        }

        if let Some(body) = opts.comment {
            self.push_comment(&p, &id, &body)?;
        }

        for tag in opts.tags {
            let tag = tag.trim();
            if !tag.is_empty() {
                p.set_add(&keys::ticket_field(&id, "tags"), tag)?;
            }
        }

        self.load(&id)
    }

    /// Load every ticket in the project in a single round-trip.
    pub fn list(&self) -> Result<Vec<Ticket>> {
        let mut out = Vec::new();
        for (id, record) in self.ticket_records()? {
            read_format_version(id, &record.fields)?;
            if let Some(t) = project_ticket(id, record.fields, &record.ops) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Every ticket record's raw fields and parsed operations, from one
    /// prefix scan: the loader behind [`Self::list`] and the Sync warnings.
    pub(crate) fn ticket_records(&self) -> Result<BTreeMap<Uuid, TicketRecord>> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut records: BTreeMap<Uuid, TicketRecord> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, _lamport, _hash)) = keys::parse_ticket_op(&key) {
                if let MetaValue::String(s) = value {
                    let op = serde_json::from_str::<Op>(&s)?;
                    records.entry(id).or_default().ops.push(op);
                }
            } else if let Some((id, field)) = keys::parse_ticket_field(&key) {
                records
                    .entry(id)
                    .or_default()
                    .fields
                    .push((field.to_string(), value));
            }
        }
        Ok(records)
    }

    /// Load a single ticket by exact UUID.
    pub fn load(&self, id: &Uuid) -> Result<Ticket> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::ticket_prefix(id)))?;
        let mut fields = Vec::new();
        let mut ops = Vec::new();
        for (key, value) in pairs {
            if keys::parse_ticket_op(&key).is_some() {
                if let MetaValue::String(s) = value {
                    ops.push(serde_json::from_str::<Op>(&s)?);
                }
            } else if let Some((parsed_id, field)) = keys::parse_ticket_field(&key) {
                if parsed_id == *id {
                    fields.push((field.to_string(), value));
                }
            }
        }
        read_format_version(*id, &fields)?;
        project_ticket(*id, fields, &ops).ok_or(Error::NotFound(*id))
    }

    /// Resolve a user-supplied ticket reference (full UUID or unique
    /// prefix, hyphens optional, case-insensitive) into a real UUID.
    pub fn resolve_id(&self, reference: &str) -> Result<Uuid> {
        let needle = reference.trim().to_ascii_lowercase().replace('-', "");
        if needle.is_empty() {
            return Err(Error::NoMatch(reference.to_string()));
        }
        let tickets = self.list()?;
        let matches: Vec<&Ticket> = tickets
            .iter()
            .filter(|t| {
                let hex = t.id.to_string().replace('-', "");
                hex.starts_with(&needle)
            })
            .collect();
        match matches.len() {
            0 => Err(Error::NoMatch(reference.to_string())),
            1 => Ok(matches[0].id),
            n => {
                let open_matches: Vec<&Ticket> = matches
                    .iter()
                    .copied()
                    .filter(|t| t.status == TicketStatus::Open)
                    .collect();
                if open_matches.len() == 1 {
                    Ok(open_matches[0].id)
                } else {
                    Err(Error::Ambiguous(reference.to_string(), n))
                }
            }
        }
    }

    // -------------------------------------------------------------------
    // Field mutators
    // -------------------------------------------------------------------

    /// Persist scalar field mutations as operations — the synced, conflict-free
    /// source of truth. A `set` becomes a compound `SetField` op and a `clear`
    /// a `ClearField` op; the derived projection is rebuilt from the log on
    /// read ([`project_scalar_fields`]). Scalar fields are never written as
    /// synced git-meta keys anymore (clean cutover).
    fn apply_scalars(
        &self,
        id: &Uuid,
        set: BTreeMap<String, String>,
        clear: Vec<String>,
    ) -> Result<()> {
        if !set.is_empty() {
            self.append_op(id, OpKind::SetField { fields: set })?;
        }
        if !clear.is_empty() {
            self.append_op(id, OpKind::ClearField { fields: clear })?;
        }
        Ok(())
    }

    pub fn set_title(&self, id: &Uuid, title: &str) -> Result<()> {
        let mut set = BTreeMap::new();
        set.insert("title".to_string(), title.to_string());
        self.apply_scalars(id, set, Vec::new())
    }

    pub fn set_description(&self, id: &Uuid, description: Option<&str>) -> Result<()> {
        match description {
            Some(d) if !d.is_empty() => {
                let mut set = BTreeMap::new();
                set.insert("description".to_string(), d.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["description".to_string()]),
        }
    }

    pub fn set_spec(&self, id: &Uuid, spec: Option<&str>) -> Result<()> {
        match spec {
            Some(s) if !s.is_empty() => {
                let mut set = BTreeMap::new();
                set.insert("spec".to_string(), s.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["spec".to_string()]),
        }
    }

    pub fn set_state(&self, id: &Uuid, state: TicketState) -> Result<()> {
        self.set_lifecycle(id, state.status(), state)
    }

    /// Direct children of `id` that are still open (`status == Open`) and not
    /// deleted: a deleted sub-issue is not live work, so it never holds up
    /// its parent's close.
    ///
    /// Dangling child references that fail to load are skipped rather than
    /// propagated, mirroring the `.ok()` handling in `set_parent`'s ancestor
    /// walk, so a corrupt reference can never wedge a close.
    pub fn open_children(&self, id: &Uuid) -> Result<Vec<Ticket>> {
        let ticket = self.load(id)?;
        let mut open = Vec::new();
        for child_id in &ticket.children {
            if let Ok(child) = self.load(child_id) {
                if child.status == TicketStatus::Open && !child.is_deleted() {
                    open.push(child);
                }
            }
        }
        Ok(open)
    }

    /// Direct dependencies of `id` (its `depends_on` targets) that are still
    /// open and not deleted (an edge to a deleted ticket counts as absent).
    /// Dangling references that fail to load are skipped.
    pub fn open_dependencies(&self, id: &Uuid) -> Result<Vec<Ticket>> {
        let ticket = self.load(id)?;
        let mut open = Vec::new();
        for dep_id in &ticket.depends_on {
            if let Ok(dep) = self.load(dep_id) {
                if dep.status == TicketStatus::Open && !dep.is_deleted() {
                    open.push(dep);
                }
            }
        }
        Ok(open)
    }

    /// Change a ticket's lifecycle, rejecting a close while it still has open
    /// sub-issues (children) or open dependencies (open `depends_on`
    /// targets). Closing a ticket that still *blocks* others is allowed — that
    /// is the normal "finished the blocker" case. Only a genuine open→closed
    /// transition is guarded; reclassifying an already-closed ticket among
    /// closed states is allowed.
    pub fn set_lifecycle(&self, id: &Uuid, status: TicketStatus, state: TicketState) -> Result<()> {
        if status == TicketStatus::Closed {
            let current = self.load(id)?;
            if current.status == TicketStatus::Open {
                let open_children = self.open_children(id)?;
                if !open_children.is_empty() {
                    return Err(Error::OpenSubissues(format!(
                        "{} open sub-issue(s): {}",
                        open_children.len(),
                        format_ticket_refs(&open_children)
                    )));
                }
                let open_deps = self.open_dependencies(id)?;
                if !open_deps.is_empty() {
                    return Err(Error::OpenDependencies(format!(
                        "{} open dependenc{}: {}",
                        open_deps.len(),
                        if open_deps.len() == 1 { "y" } else { "ies" },
                        format_ticket_refs(&open_deps)
                    )));
                }
            }
        }
        self.write_lifecycle(id, status, state)
    }

    /// Change a ticket's lifecycle without the open sub-issue and open
    /// dependency guards. Used by the `--force` CLI path; fork merges bypass the
    /// guards on their own in [`Self::merge_fork_ticket`].
    pub fn set_lifecycle_forced(
        &self,
        id: &Uuid,
        status: TicketStatus,
        state: TicketState,
    ) -> Result<()> {
        self.write_lifecycle(id, status, state)
    }

    /// Raw lifecycle write: validates the status/state pair is coherent, then
    /// persists `status`/`state` and stamps or clears `closed-by`.
    fn write_lifecycle(&self, id: &Uuid, status: TicketStatus, state: TicketState) -> Result<()> {
        if state.status() != status {
            return Err(Error::InvalidState(format!(
                "{}:{}",
                status.as_str(),
                state.as_str()
            )));
        }
        let mut set = BTreeMap::new();
        set.insert("status".to_string(), status.as_str().to_string());
        set.insert("state".to_string(), state.as_str().to_string());
        if status == TicketStatus::Closed {
            set.insert("closed-by".to_string(), self.session.email().to_string());
            self.apply_scalars(id, set, Vec::new())
        } else {
            self.apply_scalars(id, set, vec!["closed-by".to_string()])
        }
    }

    pub fn set_closed_by(&self, id: &Uuid, who: Option<&str>) -> Result<()> {
        match who {
            Some(w) if !w.is_empty() => {
                let resolved = self.resolve_user(w)?;
                validate_email(&resolved)?;
                let mut set = BTreeMap::new();
                set.insert("closed-by".to_string(), resolved);
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["closed-by".to_string()]),
        }
    }

    pub fn set_assigned(&self, id: &Uuid, who: Option<&str>) -> Result<()> {
        match who {
            Some(w) if !w.is_empty() => {
                let resolved = self.resolve_user(w)?;
                validate_email(&resolved)?;
                let mut set = BTreeMap::new();
                set.insert("assigned".to_string(), resolved);
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["assigned".to_string()]),
        }
    }

    pub fn set_priority(&self, id: &Uuid, priority: Option<i64>) -> Result<()> {
        match priority {
            Some(n) => {
                let mut set = BTreeMap::new();
                set.insert("priority".to_string(), n.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            None => self.apply_scalars(id, BTreeMap::new(), vec!["priority".to_string()]),
        }
    }

    pub fn set_points(&self, id: &Uuid, points: Option<i64>) -> Result<()> {
        match points {
            Some(n) => {
                let mut set = BTreeMap::new();
                set.insert("points".to_string(), n.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            None => self.apply_scalars(id, BTreeMap::new(), vec!["points".to_string()]),
        }
    }

    pub fn set_milestone(&self, id: &Uuid, milestone: Option<&str>) -> Result<()> {
        match milestone {
            Some(m) if !m.is_empty() => {
                let mut set = BTreeMap::new();
                set.insert("milestone".to_string(), m.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["milestone".to_string()]),
        }
    }

    pub fn set_code(&self, id: &Uuid, code: Option<&str>) -> Result<()> {
        match code {
            Some(c) if !c.is_empty() => {
                crate::ticket::validate_code_uri(c)?;
                let mut set = BTreeMap::new();
                set.insert("code".to_string(), c.to_string());
                self.apply_scalars(id, set, Vec::new())
            }
            _ => self.apply_scalars(id, BTreeMap::new(), vec!["code".to_string()]),
        }
    }

    /// Set the parent of a ticket. Validates that the parent exists and
    /// prevents self-reference and circular chains.
    pub fn set_parent(&self, child_id: &Uuid, parent_id: &Uuid) -> Result<()> {
        if child_id == parent_id {
            return Err(Error::InvalidValue(
                "a ticket cannot be its own parent".to_string(),
            ));
        }

        // Validate parent exists
        let parent = self.load(parent_id)?;

        // Prevent circular chains: walk ancestors up to depth 20
        let mut ancestor_id = parent.parent;
        let mut depth = 0;
        while let Some(aid) = ancestor_id {
            if aid == *child_id {
                return Err(Error::InvalidValue(
                    "circular parent chain detected".to_string(),
                ));
            }
            depth += 1;
            if depth > 20 {
                break;
            }
            ancestor_id = self.load(&aid).ok().and_then(|t| t.parent);
        }

        let p = self.project_handle();

        // Remove from old parent's children set if any
        let child = self.load(child_id)?;

        // Keep the "a closed ticket has no open children" invariant airtight at
        // the other entry point: don't let an open ticket become a sub-issue of
        // a closed parent.
        if child.status == TicketStatus::Open && parent.status == TicketStatus::Closed {
            return Err(Error::InvalidValue(format!(
                "cannot make an open ticket a sub-issue of closed ticket {} (reopen it first)",
                parent.short_id()
            )));
        }

        if let Some(old_parent) = child.parent {
            remove_member(
                &p,
                &keys::ticket_field(&old_parent, "children"),
                &child_id.to_string(),
            )?;
        }

        // Set the parent field on the child (op-managed scalar).
        let mut set = BTreeMap::new();
        set.insert("parent".to_string(), parent_id.to_string());
        self.apply_scalars(child_id, set, Vec::new())?;

        // Add to new parent's children set (denormalized; stays a set key).
        p.set_add(
            &keys::ticket_field(parent_id, "children"),
            &child_id.to_string(),
        )?;

        Ok(())
    }

    /// Remove the parent of a ticket.
    pub fn clear_parent(&self, child_id: &Uuid) -> Result<()> {
        let child = self.load(child_id)?;
        let p = self.project_handle();

        if let Some(old_parent) = child.parent {
            remove_member(
                &p,
                &keys::ticket_field(&old_parent, "children"),
                &child_id.to_string(),
            )?;
        }

        self.apply_scalars(child_id, BTreeMap::new(), vec!["parent".to_string()])
    }

    /// Delete a ticket by appending a `del` operation (ADR 0003). Soft: the
    /// ticket keeps every key, its history, comments, Sub-issues and
    /// Dependencies, and no other ticket is touched, so [`Self::undelete_ticket`]
    /// brings it back exactly. Fails with [`Error::AlreadyDeleted`] when it is
    /// already deleted (no op is written).
    pub fn delete_ticket(&self, id: &Uuid, reason: Option<&str>) -> Result<()> {
        if self.load(id)?.is_deleted() {
            return Err(Error::AlreadyDeleted(*id));
        }
        self.append_op(
            id,
            OpKind::Delete {
                reason: reason.map(str::to_string),
            },
        )?;
        Ok(())
    }

    /// Undo a Deletion by appending an `undel` operation. Fails with
    /// [`Error::NotDeleted`] when the ticket is not deleted: a stray `undel`
    /// would otherwise outrank a concurrent `del` from another clone.
    pub fn undelete_ticket(&self, id: &Uuid) -> Result<()> {
        if !self.load(id)?.is_deleted() {
            return Err(Error::NotDeleted(*id));
        }
        self.append_op(id, OpKind::Undelete)?;
        Ok(())
    }

    /// Add a dependency: `id` depends on `dependency_id`.
    /// The dependency ticket must exist. Circular dependencies are rejected.
    pub fn add_dependency(&self, id: &Uuid, dependency_id: &Uuid) -> Result<()> {
        if id == dependency_id {
            return Err(Error::InvalidValue(
                "a ticket cannot depend on itself".to_string(),
            ));
        }
        // Validate dependency exists
        self.load(dependency_id)?;

        // Check for circular deps: walk the dependency chain from dependency_id
        let mut visited = std::collections::HashSet::new();
        visited.insert(*id);
        let mut stack = vec![*dependency_id];
        while let Some(current) = stack.pop() {
            if !visited.insert(current) {
                continue;
            }
            let t = self.load(&current)?;
            for dep in &t.depends_on {
                if dep == id {
                    return Err(Error::InvalidValue(
                        "circular dependency detected".to_string(),
                    ));
                }
                stack.push(*dep);
            }
        }

        let p = self.project_handle();
        // id depends_on dependency_id
        p.set_add(
            &keys::ticket_field(id, "depends_on"),
            &dependency_id.to_string(),
        )?;
        // dependency_id blocks id (denormalized reverse)
        p.set_add(
            &keys::ticket_field(dependency_id, "blocks"),
            &id.to_string(),
        )?;
        Ok(())
    }

    /// Remove a dependency: `id` no longer depends on `dependency_id`.
    pub fn remove_dependency(&self, id: &Uuid, dependency_id: &Uuid) -> Result<()> {
        let p = self.project_handle();
        remove_member(
            &p,
            &keys::ticket_field(id, "depends_on"),
            &dependency_id.to_string(),
        )?;
        remove_member(
            &p,
            &keys::ticket_field(dependency_id, "blocks"),
            &id.to_string(),
        )?;
        Ok(())
    }

    pub fn set_meta(&self, id: &Uuid, field: &str, value: &str) -> Result<()> {
        let field = field.trim();
        if field.is_empty() {
            return Err(Error::InvalidValue(
                "metadata field cannot be empty".to_string(),
            ));
        }
        if field.contains(':') {
            return Err(Error::InvalidValue(
                "metadata field cannot contain `:`".to_string(),
            ));
        }

        self.project_handle()
            .set(&keys::ticket_meta_field(id, field), value)?;
        Ok(())
    }

    pub fn add_tag(&self, id: &Uuid, tag: &str) -> Result<()> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Ok(());
        }
        self.project_handle()
            .set_add(&keys::ticket_field(id, "tags"), tag)?;
        Ok(())
    }

    pub fn remove_tag(&self, id: &Uuid, tag: &str) -> Result<()> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Ok(());
        }
        remove_member(&self.project_handle(), &keys::ticket_field(id, "tags"), tag)?;
        Ok(())
    }

    pub fn add_writeup_tag(&self, id: &Uuid, tag: &str) -> Result<()> {
        self.load_writeup(id)?;
        let tag = tag.trim();
        if tag.is_empty() {
            return Ok(());
        }
        self.project_handle()
            .set_add(&keys::writeup_field(id, "tags"), tag)?;
        Ok(())
    }

    pub fn remove_writeup_tag(&self, id: &Uuid, tag: &str) -> Result<()> {
        self.load_writeup(id)?;
        let tag = tag.trim();
        if tag.is_empty() {
            return Ok(());
        }
        remove_member(
            &self.project_handle(),
            &keys::writeup_field(id, "tags"),
            tag,
        )?;
        Ok(())
    }

    pub fn add_comment(&self, id: &Uuid, body: &str) -> Result<()> {
        let p = self.project_handle();
        self.push_comment(&p, id, body)?;
        Ok(())
    }

    fn push_comment(
        &self,
        handle: &git_meta_lib::SessionTargetHandle<'_>,
        id: &Uuid,
        body: &str,
    ) -> Result<()> {
        let email = self.session.email().to_string();
        validate_email(&email)?;
        push_comment_as(handle, id, &email, body)
    }

    // -------------------------------------------------------------------
    // Writeups
    // -------------------------------------------------------------------

    pub fn create_writeup(&self, title: &str, opts: NewWriteupOpts) -> Result<Writeup> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::InvalidValue(
                "writeup title cannot be empty".to_string(),
            ));
        }

        let id = Uuid::new_v4();
        let p = self.project_handle();
        let now = opts.created_at.unwrap_or_else(OffsetDateTime::now_utc);
        let now_rfc = now
            .format(&Rfc3339)
            .map_err(|e| Error::Time(e.to_string()))?;
        let author = self.session.email();
        validate_email(author)?;

        p.set(&keys::writeup_field(&id, "title"), title)?;
        p.set(
            &keys::writeup_field(&id, "status"),
            WriteupStatus::Open.as_str(),
        )?;
        p.set(&keys::writeup_field(&id, "created-at"), now_rfc.as_str())?;
        p.set(&keys::writeup_field(&id, "created-by"), author)?;
        p.set_add(&keys::writeup_field(&id, "authors"), author)?;

        for tag in opts.tags {
            let tag = tag.trim();
            if !tag.is_empty() {
                p.set_add(&keys::writeup_field(&id, "tags"), tag)?;
            }
        }

        if let Some(body) = opts.body {
            self.push_writeup_version(&p, &id, &body)?;
        }

        self.load_writeup(&id)
    }

    pub fn list_writeups(&self) -> Result<Vec<Writeup>> {
        let p = self.project_handle();
        let pairs = p.get_all_values(Some(&keys::writeups_prefix()))?;
        let mut by_id: BTreeMap<Uuid, Vec<(String, MetaValue)>> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, field)) = keys::parse_writeup_field(&key) {
                by_id
                    .entry(id)
                    .or_default()
                    .push((field.to_string(), value));
            }
        }

        let mut out = Vec::with_capacity(by_id.len());
        for (id, fields) in by_id {
            if let Some(writeup) = build_writeup(id, fields) {
                out.push(writeup);
            }
        }
        out.sort_by(|a, b| {
            metadata_priority_sort_key(a.priority)
                .cmp(&metadata_priority_sort_key(b.priority))
                .then_with(|| {
                    b.created_at
                        .cmp(&a.created_at)
                        .then_with(|| a.title.cmp(&b.title))
                })
        });
        Ok(out)
    }

    pub fn load_writeup(&self, id: &Uuid) -> Result<Writeup> {
        let p = self.project_handle();
        let pairs = p.get_all_values(Some(&keys::writeup_prefix(id)))?;
        let mut fields = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if let Some((parsed_id, field)) = keys::parse_writeup_field(&key) {
                if parsed_id == *id {
                    fields.push((field.to_string(), value));
                }
            }
        }
        build_writeup(*id, fields).ok_or(Error::NotFound(*id))
    }

    pub fn resolve_writeup_id(&self, reference: &str) -> Result<Uuid> {
        let needle = reference.trim().to_ascii_lowercase().replace('-', "");
        if needle.is_empty() {
            return Err(Error::NoMatch(reference.to_string()));
        }
        let writeups = self.list_writeups()?;
        let matches: Vec<&Writeup> = writeups
            .iter()
            .filter(|writeup| {
                let hex = writeup.id.to_string().replace('-', "");
                hex.starts_with(&needle)
            })
            .collect();
        match matches.len() {
            0 => Err(Error::NoMatch(reference.to_string())),
            1 => Ok(matches[0].id),
            n => {
                let open_matches: Vec<&Writeup> = matches
                    .iter()
                    .copied()
                    .filter(|writeup| writeup.status == WriteupStatus::Open)
                    .collect();
                if open_matches.len() == 1 {
                    Ok(open_matches[0].id)
                } else {
                    Err(Error::Ambiguous(reference.to_string(), n))
                }
            }
        }
    }

    pub fn append_writeup_version(&self, id: &Uuid, body: &str) -> Result<()> {
        self.load_writeup(id)?;
        let p = self.project_handle();
        self.push_writeup_version(&p, id, body)?;
        Ok(())
    }

    pub fn set_writeup_title(&self, id: &Uuid, title: &str) -> Result<()> {
        self.load_writeup(id)?;
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::InvalidValue(
                "writeup title cannot be empty".to_string(),
            ));
        }
        self.project_handle()
            .set(&keys::writeup_field(id, "title"), title)?;
        Ok(())
    }

    fn push_writeup_version(
        &self,
        handle: &git_meta_lib::SessionTargetHandle<'_>,
        id: &Uuid,
        body: &str,
    ) -> Result<()> {
        let body = body.trim();
        if body.is_empty() {
            return Err(Error::InvalidValue(
                "writeup version body cannot be empty".to_string(),
            ));
        }
        let author = self.session.email();
        validate_email(author)?;
        let now = OffsetDateTime::now_utc();
        let now_rfc = now
            .format(&Rfc3339)
            .map_err(|e| Error::Time(e.to_string()))?;
        let doc = format!("---\nauthor: {author}\ndate: {now_rfc}\n---\n\n{body}");
        handle.list_push(&keys::writeup_field(id, "versions"), &doc)?;
        handle.set_add(&keys::writeup_field(id, "authors"), author)?;
        Ok(())
    }

    pub fn set_writeup_status(&self, id: &Uuid, status: WriteupStatus) -> Result<()> {
        self.load_writeup(id)?;
        self.project_handle()
            .set(&keys::writeup_field(id, "status"), status.as_str())?;
        Ok(())
    }

    pub fn set_writeup_priority(&self, id: &Uuid, priority: Option<i64>) -> Result<()> {
        self.load_writeup(id)?;
        let p = self.project_handle();
        let key = keys::writeup_field(id, "priority");
        match priority {
            Some(n) => {
                p.set(&key, n.to_string().as_str())?;
            }
            None => {
                p.remove(&key)?;
            }
        }
        Ok(())
    }

    // The writeup<->ticket link is stored once, on the writeup
    // (`writeups:<id>:tickets`). A ticket-side `tickets:<id>:writeups` reverse
    // index used to be written here too, but nothing ever read it back into the
    // `Ticket` model, so it was only dead writes and a dangling-ref liability
    // (deletes never cleaned it). If a reverse lookup is needed, derive it from
    // the writeups or re-add it with full read/serialize/cleanup plumbing.
    pub fn link_writeup_ticket(&self, writeup_id: &Uuid, ticket_id: &Uuid) -> Result<()> {
        self.load_writeup(writeup_id)?;
        self.load(ticket_id)?;
        let p = self.project_handle();
        p.set_add(
            &keys::writeup_field(writeup_id, "tickets"),
            &ticket_id.to_string(),
        )?;
        Ok(())
    }

    pub fn unlink_writeup_ticket(&self, writeup_id: &Uuid, ticket_id: &Uuid) -> Result<()> {
        self.load_writeup(writeup_id)?;
        self.load(ticket_id)?;
        let p = self.project_handle();
        remove_member(
            &p,
            &keys::writeup_field(writeup_id, "tickets"),
            &ticket_id.to_string(),
        )?;
        Ok(())
    }

    pub fn promote_writeup(&self, writeup_id: &Uuid) -> Result<Ticket> {
        let writeup = self.load_writeup(writeup_id)?;
        let body = writeup.latest_body().unwrap_or("").trim().to_string();
        let ticket = self.create(
            &writeup.title,
            NewTicketOpts {
                tags: writeup.tags.iter().cloned().collect(),
                ..Default::default()
            },
        )?;
        if !body.is_empty() {
            self.set_description(&ticket.id, Some(&body))?;
        }
        if writeup.priority.is_some() {
            self.set_priority(&ticket.id, writeup.priority)?;
        }
        self.link_writeup_ticket(writeup_id, &ticket.id)?;
        self.load(&ticket.id)
    }

    fn project_handle(&self) -> git_meta_lib::SessionTargetHandle<'_> {
        self.session.target(&Target::project())
    }

    // -------------------------------------------------------------------
    // Saved views (named, frozen sets of ticket UUIDs)
    // -------------------------------------------------------------------

    /// Save a snapshot of `ids` under the name `name`.
    /// Replaces any existing membership of that view.
    pub fn save_view(&self, name: &str, ids: &BTreeSet<Uuid>) -> Result<()> {
        let p = self.project_handle();
        let key = keys::view(name);
        if let Some(MetaValue::Set(existing)) = p.get_value(&key)? {
            for member in existing {
                remove_member(&p, &key, &member)?;
            }
        }
        for id in ids {
            p.set_add(&key, &id.to_string())?;
        }
        Ok(())
    }

    /// Load the UUID set stored under view `name`.
    pub fn load_view(&self, name: &str) -> Result<BTreeSet<Uuid>> {
        let p = self.project_handle();
        match p.get_value(&keys::view(name))? {
            Some(MetaValue::Set(members)) => Ok(members
                .iter()
                .filter_map(|s| Uuid::parse_str(s).ok())
                .collect()),
            _ => Ok(BTreeSet::new()),
        }
    }

    /// List all view names defined on this project, alphabetised.
    pub fn list_views(&self) -> Result<Vec<String>> {
        let p = self.project_handle();
        let pairs = p.get_all_values(Some(&keys::views_prefix()))?;
        let mut names: Vec<String> = pairs
            .into_iter()
            .filter_map(|(k, _)| keys::parse_view_name(&k).map(String::from))
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    // -------------------------------------------------------------------
    // System-wide ticgit metadata
    // -------------------------------------------------------------------

    pub fn add_owner(&self, who: &str) -> Result<()> {
        validate_email(who)?;
        self.project_handle()
            .set_add(&keys::system_key("owners"), who.trim())?;
        Ok(())
    }

    pub fn remove_owner(&self, who: &str) -> Result<()> {
        remove_member(
            &self.project_handle(),
            &keys::system_key("owners"),
            who.trim(),
        )?;
        Ok(())
    }

    pub fn list_owners(&self) -> Result<BTreeSet<String>> {
        let p = self.project_handle();
        match p.get_value(&keys::system_key("owners"))? {
            Some(MetaValue::Set(members)) => Ok(members),
            _ => Ok(BTreeSet::new()),
        }
    }

    // -------------------------------------------------------------------
    // User nick → email map (shared mailmap)
    // -------------------------------------------------------------------

    /// List all user nicks and their email sets.
    pub fn list_users(&self) -> Result<BTreeMap<String, BTreeSet<String>>> {
        let p = self.project_handle();
        let pairs = p.get_all_values(Some(&keys::users_prefix()))?;
        let mut users = BTreeMap::new();
        for (key, value) in pairs {
            if let Some(nick) = keys::parse_user_nick(&key) {
                if let MetaValue::Set(emails) = value {
                    users.insert(nick.to_string(), emails);
                }
            }
        }
        Ok(users)
    }

    /// Get the email set for a nick.
    pub fn get_user(&self, nick: &str) -> Result<BTreeSet<String>> {
        let p = self.project_handle();
        match p.get_value(&keys::user_key(nick))? {
            Some(MetaValue::Set(emails)) => Ok(emails),
            _ => Ok(BTreeSet::new()),
        }
    }

    /// Add an email to a nick's set.
    pub fn add_user_email(&self, nick: &str, email: &str) -> Result<()> {
        validate_email(email)?;
        self.project_handle()
            .set_add(&keys::user_key(nick), email)?;
        Ok(())
    }

    /// Remove an email from a nick's set. If the set becomes empty, remove the key.
    pub fn remove_user_email(&self, nick: &str, email: &str) -> Result<()> {
        let p = self.project_handle();
        remove_member(&p, &keys::user_key(nick), email)?;
        // Check if empty and clean up.
        if let Ok(emails) = self.get_user(nick) {
            if emails.is_empty() {
                p.remove(&keys::user_key(nick))?;
            }
        }
        Ok(())
    }

    /// Remove a user nick entirely (all emails).
    pub fn remove_user(&self, nick: &str) -> Result<()> {
        self.project_handle().remove(&keys::user_key(nick))?;
        Ok(())
    }

    /// Resolve a nick or email to an email address.
    /// If `input` contains `@`, treat it as an email and return as-is.
    /// Otherwise, look up as a nick and return the first email.
    pub fn resolve_user(&self, input: &str) -> Result<String> {
        if input.contains('@') {
            return Ok(input.to_string());
        }
        let emails = self.get_user(input)?;
        emails
            .into_iter()
            .next()
            .ok_or_else(|| Error::InvalidValue(format!("unknown user nick `{input}`")))
    }

    /// Reverse-lookup: given an email, find the nick (if any).
    pub fn nick_for_email(&self, email: &str) -> Result<Option<String>> {
        let users = self.list_users()?;
        for (nick, emails) in &users {
            if emails.contains(email) {
                return Ok(Some(nick.clone()));
            }
        }
        Ok(None)
    }

    pub fn schema_version(&self) -> Result<Option<String>> {
        let p = self.project_handle();
        match p.get_value(keys::SCHEMA_VERSION_KEY)? {
            Some(MetaValue::String(s)) => Ok(Some(s)),
            _ => Ok(None),
        }
    }

    // -------------------------------------------------------------------
    // Code Reviews
    // -------------------------------------------------------------------

    /// Create review metadata for a branch, optionally linking it to a ticket.
    pub fn create_review(&self, opts: &NewReviewOpts, vcs: &impl ReviewVcs) -> Result<Review> {
        let branch_name = match opts.branch.as_deref() {
            Some(b) => b.to_string(),
            None => vcs.current_branch()?,
        };
        let branch_id = self.ensure_review_branch_id(&branch_name)?;
        let base_ref = opts.base.clone().unwrap_or_else(|| vcs.default_base_ref());
        let base_sha = vcs
            .resolve_ref(&base_ref)
            .or_else(|_| vcs.merge_base(&base_ref, &branch_name))
            .unwrap_or_else(|_| vcs.resolve_ref("HEAD").unwrap_or_default());
        let head_sha = vcs.resolve_branch_head(&branch_name)?;
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|e| Error::Time(e.to_string()))?;
        let target = self.session.target(&Target::branch(&branch_id));
        let mut title = opts.title.clone().unwrap_or_else(|| branch_name.clone());
        let mut description = opts.description.clone().unwrap_or_default();
        let mut linked_ticket_id = None;

        if let Some(ticket_ref) = opts.ticket.as_deref() {
            let ticket_id = self.resolve_id(ticket_ref)?;
            let ticket = self.load(&ticket_id)?;
            if title == branch_name {
                title = ticket.title.clone();
            }
            if description.is_empty() {
                description = ticket.description.clone().unwrap_or_default();
            }
            target.set_add("issue:id", &ticket_id.to_string())?;
            self.session.target(&Target::project()).set(
                &keys::ticket_field(&ticket_id, "branch-id"),
                branch_id.as_str(),
            )?;
            linked_ticket_id = Some(ticket_id);
        }

        target.set("title", title.as_str())?;
        target.set("description", description.as_str())?;
        target.set("status", "open")?;
        target.set("base:sha", base_sha.as_str())?;
        target.set("head:sha", head_sha.as_str())?;
        target.set(keys::REVIEW_CREATED_AT_KEY, now.as_str())?;
        target.set(keys::REVIEW_CREATED_BY_KEY, self.email())?;
        for reviewer in &opts.reviewers {
            target.set_add(keys::REVIEW_REVIEWERS_KEY, reviewer)?;
        }
        if let Some(ref commits) = opts.commits {
            self.refresh_review_revisions_from_commits(&branch_id, commits, vcs)?;
        } else {
            self.refresh_review_revisions_from_range(&branch_id, &base_sha, &head_sha, vcs)?;
        }
        self.session
            .target(&Target::project())
            .set_add(keys::REVIEW_INDEX_KEY, branch_id.as_str())?;

        let branch_target = self.session.target(&Target::branch(&branch_id));
        branch_target.set("code:branch", branch_name.as_str())?;
        if let Ok(Some(url)) = vcs.remote_url() {
            branch_target.set("code:url", url.as_str())?;
            if let Some(ticket_id) = linked_ticket_id {
                if url.starts_with("http://") || url.starts_with("https://") {
                    let code = format!("{url}:{branch_name}");
                    let _ = self.set_code(&ticket_id, Some(&code));
                }
            }
        }

        self.load_review(&branch_id)
    }

    /// Return the existing branch-id or generate a new timestamped ID e.g. `feature@1770000000`.
    pub fn ensure_review_branch_id(&self, branch_name: &str) -> Result<String> {
        let branch_target = self.session.target(&Target::branch(branch_name));
        if let Some(MetaValue::String(id)) = branch_target.get_value("branch-id")? {
            if !id.is_empty() {
                return Ok(id);
            }
        }
        let timestamp = OffsetDateTime::now_utc().unix_timestamp();
        let branch_id = format!("{branch_name}@{timestamp}");
        branch_target.set("branch-id", branch_id.as_str())?;
        Ok(branch_id)
    }

    /// Load the review record for `branch_id`.
    pub fn load_review(&self, branch_id: &str) -> Result<Review> {
        let target = self.session.target(&Target::branch(branch_id));
        let branch_name = meta_string(target.get_value("code:branch")?);
        let title = meta_string(target.get_value("title")?)
            .or_else(|| branch_name.clone())
            .unwrap_or_else(|| branch_id.to_string());
        let description = meta_string(target.get_value("description")?).unwrap_or_default();
        let status = meta_string(target.get_value("status")?).unwrap_or_else(|| "unknown".into());
        let head_sha = meta_string(target.get_value("head:sha")?);
        let base_sha = meta_string(target.get_value("base:sha")?);
        let integration_sha = meta_string(target.get_value("integration:sha")?);
        let created_at = meta_string(target.get_value(keys::REVIEW_CREATED_AT_KEY)?);
        let created_by = meta_string(target.get_value(keys::REVIEW_CREATED_BY_KEY)?);
        let reviewers = meta_set(target.get_value(keys::REVIEW_REVIEWERS_KEY)?)
            .into_iter()
            .collect();
        let ticket_id = meta_set(target.get_value("issue:id")?)
            .into_iter()
            .filter_map(|s| Uuid::parse_str(&s).ok())
            .next();
        let revisions = target
            .list_entries(keys::REVIEW_REVISIONS_KEY)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| parse_review_revision_change(&e.value))
            .collect();
        let mut revision_history = target
            .list_entries(keys::REVIEW_REVISION_HISTORY_KEY)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| parse_review_revision_change(&e.value))
            .collect::<Vec<_>>();
        if revision_history.is_empty() {
            let rev_copy: &Vec<ReviewRevisionChange> = &revisions;
            revision_history = rev_copy.iter().rev().cloned().collect();
        }
        let messages = target
            .list_entries(keys::REVIEW_MESSAGES_KEY)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str::<ReviewMessage>(&e.value).ok())
            .collect();

        Ok(Review {
            branch_id: branch_id.to_string(),
            branch_name,
            ticket_id,
            title,
            description,
            status,
            base_sha,
            head_sha,
            integration_sha,
            created_at,
            created_by,
            reviewers,
            revisions,
            revision_history,
            messages,
        })
    }

    /// List all reviews registered in `review:branches`.
    pub fn list_reviews(&self) -> Result<Vec<Review>> {
        let project = self.session.target(&Target::project());
        let branch_ids = meta_set(project.get_value(keys::REVIEW_INDEX_KEY)?);
        let mut reviews = Vec::with_capacity(branch_ids.len());
        for id in branch_ids {
            if let Ok(review) = self.load_review(&id) {
                reviews.push(review);
            }
        }
        Ok(reviews)
    }

    /// Resolve an explicit ticket-id, branch-id, branch name, or default to current branch.
    pub fn resolve_review_branch_id(
        &self,
        explicit: Option<&str>,
        vcs: Option<&impl ReviewVcs>,
    ) -> Result<String> {
        let name = match explicit {
            Some(n) => n.to_string(),
            None => match vcs {
                Some(v) => v.current_branch()?,
                None => return Err(Error::ReviewNotFound("no branch specified".into())),
            },
        };
        if let Ok(ticket_id) = self.resolve_id(&name) {
            if let Some(MetaValue::String(branch_id)) = self
                .session
                .target(&Target::project())
                .get_value(&keys::ticket_field(&ticket_id, "branch-id"))?
            {
                if !branch_id.is_empty() {
                    return Ok(branch_id);
                }
            }
        }
        let branch_target = self.session.target(&Target::branch(&name));
        if let Some(MetaValue::String(branch_id)) = branch_target.get_value("branch-id")? {
            if !branch_id.is_empty() {
                return Ok(branch_id);
            }
        }
        if branch_target.get_value("status")?.is_some()
            || branch_target.get_value("title")?.is_some()
        {
            return Ok(name);
        }
        Err(Error::ReviewNotFound(name))
    }

    /// Load mapped reviews for tickets (used by TUI review table/detail).
    pub fn load_ticket_reviews(&self, tickets: &[Ticket]) -> Result<HashMap<Uuid, Review>> {
        let project = self.session.target(&Target::project());
        let mut reviews = HashMap::new();

        for ticket in tickets {
            if let Some(branch_id) =
                meta_string(project.get_value(&keys::ticket_field(&ticket.id, "branch-id"))?)
            {
                if let Ok(review) = self.load_review(&branch_id) {
                    reviews.insert(ticket.id, review);
                }
            } else if let Some(branch_name) = ticket
                .code
                .as_deref()
                .and_then(crate::review::code_branch_name)
            {
                if let Ok(review) = self.load_review(branch_name) {
                    reviews.insert(ticket.id, review);
                } else {
                    reviews.insert(
                        ticket.id,
                        Review {
                            branch_id: branch_name.to_string(),
                            branch_name: Some(branch_name.to_string()),
                            title: branch_name.to_string(),
                            status: "open".to_string(),
                            ..Default::default()
                        },
                    );
                }
            }
        }

        for branch_id in meta_set(project.get_value(keys::REVIEW_INDEX_KEY)?) {
            if let Ok(review) = self.load_review(&branch_id) {
                let target = self.session.target(&Target::branch(&branch_id));
                for ticket_id in meta_set(target.get_value("issue:id")?) {
                    if let Ok(ticket_id) = Uuid::parse_str(&ticket_id) {
                        reviews.entry(ticket_id).or_insert_with(|| review.clone());
                    }
                }
            }
        }

        Ok(reviews)
    }

    /// Add a reviewer email to a review branch.
    pub fn add_reviewer(&self, branch_id: &str, email: &str) -> Result<()> {
        self.session
            .target(&Target::branch(branch_id))
            .set_add(keys::REVIEW_REVIEWERS_KEY, email)?;
        Ok(())
    }

    /// Append a comment message to a review.
    pub fn add_review_comment(&self, branch_id: &str, message: ReviewMessage) -> Result<()> {
        self.append_review_message(branch_id, message)
    }

    /// Request changes on a review, setting its status to `changes-requested`.
    pub fn request_changes(&self, branch_id: &str, message: ReviewMessage) -> Result<()> {
        self.session
            .target(&Target::branch(branch_id))
            .set("status", "changes-requested")?;
        self.append_review_message(branch_id, message)
    }

    /// Approve a specific commit. If `branch_id` is passed, appends an approval message
    /// and checks if all commits in the review are approved (setting status to `approved` if so).
    /// Returns `true` if the review is now all approved.
    pub fn approve_review_commit(
        &self,
        branch_id: Option<&str>,
        sha: &str,
        body: &str,
    ) -> Result<bool> {
        let email = self.email().to_string();
        let commit_target = self.session.target(&Target::commit(sha)?);
        commit_target.set_add(keys::REVIEW_APPROVALS_KEY, &email)?;
        commit_target.set_add(keys::REVIEW_REVIEWED_KEY, &email)?;

        let mut all_approved = false;
        if let Some(branch_id) = branch_id {
            self.append_review_message(
                branch_id,
                ReviewMessage {
                    author: email,
                    body: body.to_string(),
                    message_type: "approval".into(),
                    commit: Some(sha.to_string()),
                    path: None,
                    lines: None,
                    at: None,
                },
            )?;
            let review = self.load_review(branch_id)?;
            all_approved = self.review_commits_all_approved(&review, Some(sha));
            if all_approved {
                self.session
                    .target(&Target::branch(branch_id))
                    .set("status", "approved")?;
            }
        }
        Ok(all_approved)
    }

    /// Mark a review approved directly.
    pub fn approve_review(
        &self,
        branch_id: &str,
        body: &str,
        commit: Option<String>,
    ) -> Result<()> {
        let target = self.session.target(&Target::branch(branch_id));
        target.set("status", "approved")?;
        self.append_review_message(
            branch_id,
            ReviewMessage {
                author: self.email().to_string(),
                body: body.to_string(),
                message_type: "approval".into(),
                commit,
                path: None,
                lines: None,
                at: None,
            },
        )
    }

    /// Refresh a review's head commit and revision change list.
    pub fn update_review(
        &self,
        branch_id: &str,
        head: Option<&str>,
        vcs: &impl ReviewVcs,
    ) -> Result<(Review, String)> {
        let target = self.session.target(&Target::branch(branch_id));
        let branch =
            meta_string(target.get_value("code:branch")?).unwrap_or_else(|| branch_id.to_string());
        let head_sha = match head {
            Some(h) => vcs.resolve_ref(h).unwrap_or_else(|_| h.to_string()),
            None => vcs.resolve_branch_head(&branch)?,
        };
        let base_sha = meta_string(target.get_value("base:sha")?).unwrap_or_default();
        target.set("head:sha", head_sha.as_str())?;
        self.refresh_review_revisions_from_range(branch_id, &base_sha, &head_sha, vcs)?;
        let review = self.load_review(branch_id)?;
        Ok((review, head_sha))
    }

    /// Update a review's head and refresh revisions from an existing branch snapshot.
    pub fn update_review_from_snapshot(
        &self,
        branch_id: &str,
        snapshot: &crate::ReviewBranchSnapshot,
        vcs: &impl ReviewVcs,
    ) -> Result<()> {
        let target = self.session.target(&Target::branch(branch_id));
        let base_is_empty =
            meta_string(target.get_value("base:sha")?).is_none_or(|base| base.is_empty());
        if base_is_empty && !snapshot.base_sha.is_empty() {
            target.set("base:sha", snapshot.base_sha.as_str())?;
        }
        target.set("head:sha", snapshot.head_sha.as_str())?;
        self.refresh_review_revisions_from_commits(branch_id, &snapshot.commits, vcs)
    }

    /// Record merge / integration commit for a review.
    pub fn integrate_review(&self, branch_id: &str, integration_sha: &str) -> Result<()> {
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|e| Error::Time(e.to_string()))?;
        let target = self.session.target(&Target::branch(branch_id));
        target.set("integration:sha", integration_sha)?;
        target.set("status", "merged")?;
        target.set(keys::REVIEW_MERGED_AT_KEY, now.as_str())?;
        target.set(keys::REVIEW_MERGED_BY_KEY, self.email())?;
        Ok(())
    }

    /// Close a review branch.
    pub fn close_review(&self, branch_id: &str) -> Result<()> {
        self.session
            .target(&Target::branch(branch_id))
            .set("status", "closed")?;
        Ok(())
    }

    /// Reopen a closed review branch.
    pub fn reopen_review(&self, branch_id: &str) -> Result<()> {
        self.session
            .target(&Target::branch(branch_id))
            .set("status", "open")?;
        Ok(())
    }

    /// Set review title and description.
    pub fn set_review_details(
        &self,
        branch_id: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<()> {
        let target = self.session.target(&Target::branch(branch_id));
        target.set("title", title)?;
        target.set("description", description.unwrap_or(""))?;
        Ok(())
    }

    /// Return approval and reviewed marks for a commit.
    pub fn commit_review_status(&self, sha: &str) -> CommitReviewStatus {
        let Ok(target) = Target::commit(sha) else {
            return CommitReviewStatus::default();
        };
        let handle = self.session.target(&target);
        CommitReviewStatus {
            reviewed: meta_set(handle.get_value(keys::REVIEW_REVIEWED_KEY).ok().flatten()),
            approvals: meta_set(handle.get_value(keys::REVIEW_APPROVALS_KEY).ok().flatten()),
            signed_off: meta_set(handle.get_value("signed-off").ok().flatten()),
        }
    }

    /// Return true if every commit in `review` is approved.
    pub fn review_commits_all_approved(&self, review: &Review, approved_sha: Option<&str>) -> bool {
        let commits = review.commits();
        !commits.is_empty()
            && commits.iter().all(|sha| {
                approved_sha == Some(sha.as_str())
                    || !self.commit_review_status(sha).approvals.is_empty()
            })
    }

    /// Aggregate review progress across commits.
    pub fn review_progress(
        &self,
        commits: &[String],
        messages: &[ReviewMessage],
    ) -> ReviewProgress {
        let mut progress = ReviewProgress {
            total: commits.len(),
            ..Default::default()
        };
        for sha in commits {
            let status = self.commit_review_status(sha);
            let mut reviewed = status.reviewed;
            let mut approvals = status.approvals;
            for msg in messages.iter().filter(|m| {
                m.commit
                    .as_deref()
                    .is_some_and(|c| c == sha || sha.starts_with(c) || c.starts_with(sha))
            }) {
                match msg.message_type.as_str() {
                    "approval" => {
                        reviewed.insert(msg.author.clone());
                        approvals.insert(msg.author.clone());
                    }
                    "changes-requested" | "comment" => {
                        reviewed.insert(msg.author.clone());
                    }
                    _ => {}
                }
            }
            if !reviewed.is_empty() {
                progress.reviewed += 1;
            }
            if !approvals.is_empty() {
                progress.approved += 1;
            }
        }
        progress
    }

    /// Ensure commit patch-id is stored in git-meta.
    pub fn ensure_commit_patch_id(
        &self,
        sha: &str,
        vcs: &impl ReviewVcs,
    ) -> Result<Option<String>> {
        let target = self.session.target(&Target::commit(sha)?);
        if let Some(patch_id) = meta_string(target.get_value("patch-id")?) {
            return Ok(Some(patch_id));
        }
        let Some(patch_id) = vcs.commit_patch_id(sha) else {
            return Ok(None);
        };
        target.set("patch-id", patch_id.as_str())?;
        Ok(Some(patch_id))
    }

    /// Refresh review revisions from a base..head range.
    pub fn refresh_review_revisions_from_range(
        &self,
        branch_id: &str,
        base_sha: &str,
        head_sha: &str,
        vcs: &impl ReviewVcs,
    ) -> Result<()> {
        let commits = vcs.revision_list(base_sha, head_sha)?;
        self.refresh_review_revisions_from_commits(branch_id, &commits, vcs)
    }

    /// Refresh review revisions from an explicit list of commit SHAs.
    pub fn refresh_review_revisions_from_commits(
        &self,
        branch_id: &str,
        commits: &[String],
        vcs: &impl ReviewVcs,
    ) -> Result<()> {
        let target = self.session.target(&Target::branch(branch_id));
        let previous = target
            .list_entries(keys::REVIEW_REVISIONS_KEY)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| parse_review_revision_change(&e.value))
            .collect::<Vec<_>>();
        let entries = commits
            .iter()
            .map(|sha| {
                Ok(ReviewRevisionChange {
                    sha: sha.clone(),
                    change_id: vcs.commit_change_id(sha),
                    patch_id: self.ensure_commit_patch_id(sha, vcs)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        target.remove(keys::REVIEW_REVISIONS_KEY)?;
        for entry in &entries {
            target.list_push(keys::REVIEW_REVISIONS_KEY, &format_review_revision(entry))?;
        }
        self.append_revision_change_history(branch_id, &previous, vcs)?;
        self.append_revision_change_history(branch_id, &entries, vcs)?;
        Ok(())
    }

    fn append_revision_change_history(
        &self,
        branch_id: &str,
        commits: &[ReviewRevisionChange],
        vcs: &impl ReviewVcs,
    ) -> Result<()> {
        let target = self.session.target(&Target::branch(branch_id));
        let mut history = target
            .list_entries(keys::REVIEW_REVISION_HISTORY_KEY)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| parse_review_revision_change(&e.value))
            .collect::<Vec<_>>();
        let mut changed = false;
        for entry in &mut history {
            if entry.patch_id.is_none() {
                entry.patch_id = self.ensure_commit_patch_id(&entry.sha, vcs)?;
                changed |= entry.patch_id.is_some();
            }
        }
        let mut seen = history
            .iter()
            .map(|e| e.sha.clone())
            .collect::<BTreeSet<_>>();
        for entry in commits.iter().rev() {
            if seen.insert(entry.sha.clone()) {
                history.push(entry.clone());
                changed = true;
            }
        }
        if changed {
            target.remove(keys::REVIEW_REVISION_HISTORY_KEY)?;
            for entry in history {
                target.list_push(
                    keys::REVIEW_REVISION_HISTORY_KEY,
                    &serde_json::to_string(&entry)?,
                )?;
            }
        }
        Ok(())
    }

    fn append_review_message(&self, branch_id: &str, mut message: ReviewMessage) -> Result<()> {
        if message.at.is_none() {
            let now = OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| Error::Time(e.to_string()))?;
            message.at = Some(now);
        }
        let json = serde_json::to_string(&message)?;
        self.session
            .target(&Target::branch(branch_id))
            .list_push(keys::REVIEW_MESSAGES_KEY, &json)?;
        Ok(())
    }

    /// Roll every ticket forward to [`CURRENT_TICKET_FORMAT`] (the op-log
    /// format), returning a per-ticket plan. With `write == false` this is a
    /// dry run. With `write == true` each legacy (scalar-key, no-op) ticket is
    /// converted: its current scalar fields are snapshotted into a single
    /// `create` op at Lamport 0, the legacy synced scalar keys are removed, and
    /// the `format-version` marker is stamped. Idempotent — a ticket that
    /// already has ops is left untouched (`changed == false`). A ticket whose
    /// version exceeds this binary surfaces [`Error::FormatTooNew`].
    pub fn migrate(&self, write: bool) -> Result<Vec<MigrationOutcome>> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut fields_by_id: BTreeMap<Uuid, Vec<(String, MetaValue)>> = BTreeMap::new();
        let mut has_ops: BTreeSet<Uuid> = BTreeSet::new();
        for (key, value) in pairs {
            if let Some((id, _lamport, _hash)) = keys::parse_ticket_op(&key) {
                has_ops.insert(id);
            } else if let Some((id, field)) = keys::parse_ticket_field(&key) {
                fields_by_id
                    .entry(id)
                    .or_default()
                    .push((field.to_string(), value));
            }
        }

        let mut out = Vec::with_capacity(fields_by_id.len());
        for (id, fields) in fields_by_id {
            // Enforce the too-new / malformed guards before touching anything.
            read_format_version(id, &fields)?;
            let present = fields.iter().find_map(|(field, value)| {
                if field == keys::FORMAT_VERSION_FIELD {
                    if let MetaValue::String(s) = value {
                        return s.trim().parse::<u32>().ok();
                    }
                }
                None
            });

            // A ticket is already migrated iff it has ops. Legacy tickets are
            // scalar-key only and need conversion.
            let changed = !has_ops.contains(&id);
            if changed && write {
                self.convert_legacy(&p, id, &fields)?;
            }

            out.push(MigrationOutcome {
                id,
                short_id: id.to_string().chars().take(6).collect(),
                from: present,
                to: CURRENT_TICKET_FORMAT,
                changed,
            });
        }
        Ok(out)
    }

    /// Convert one legacy (scalar-key, no-op) ticket: snapshot its scalar
    /// fields into a `create` op at Lamport 0, drop the legacy scalar keys and
    /// stamp the format-version marker.
    fn convert_legacy(
        &self,
        p: &git_meta_lib::SessionTargetHandle<'_>,
        id: Uuid,
        fields: &[(String, MetaValue)],
    ) -> Result<()> {
        let ticket = build_ticket(id, fields.to_vec())
            .ok_or_else(|| Error::InvalidValue(format!("cannot migrate ticket {id}")))?;
        let snapshot = snapshot_scalar_fields(&ticket);
        let author = self.session.email().to_string();
        let op = Op::new(
            0,
            &author,
            ticket.created_at,
            OpKind::Create { fields: snapshot },
        )?;
        p.set(
            &keys::ticket_op(&id, op.lamport, &op.id),
            serde_json::to_string(&op)?.as_str(),
        )?;
        // Remove the now-redundant legacy scalar keys (clean cutover).
        for (field, _) in fields {
            if is_op_managed_scalar(field) {
                p.remove(&keys::ticket_field(&id, field))?;
            }
        }
        p.set(
            &keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD),
            CURRENT_TICKET_FORMAT.to_string().as_str(),
        )?;
        Ok(())
    }

    // -------------------------------------------------------------------
    // Fork merge (`ti pull`)
    // -------------------------------------------------------------------

    /// Fold a ticket read from a fork into this store. `remote_ops` is the
    /// fork's operation log for the ticket. An unknown ticket is imported
    /// under its original id as a `create` op carrying the fork's scalars; a
    /// known one gets a single op for the scalars where the fork differs (the
    /// fork wins, except that a missing parent never clears a local one), plus
    /// the union of tags and children, the fork's `meta:*` values, and any
    /// comments not already present.
    ///
    /// Deletion is not a scalar the fork overrides: every `del` and `undel`
    /// operation in `remote_ops` this store lacks is copied as is (same id and
    /// Lamport clock, without the fork's signature, whose key this store may
    /// not hold), so the Deletion projects from the union of both logs like
    /// any merge. A ticket deleted on the fork imports deleted, and a local
    /// Deletion the fork never saw stays.
    ///
    /// The fork is authoritative here, so the open sub-issue and open
    /// dependency close guards are not applied: mirroring a close never fails.
    pub fn merge_fork_ticket(&self, remote: &Ticket, remote_ops: &[Op]) -> Result<ForkMerge> {
        match self.load(&remote.id) {
            Ok(local) => {
                let merged = self.merge_fork_into(&local, remote)?;
                let copied = self.copy_fork_lifecycle_ops(&remote.id, remote_ops)?;
                Ok(if copied { ForkMerge::Updated } else { merged })
            }
            Err(Error::NotFound(_)) => {
                self.import_fork_ticket(remote)?;
                self.copy_fork_lifecycle_ops(&remote.id, remote_ops)?;
                Ok(ForkMerge::Imported)
            }
            Err(e) => Err(e),
        }
    }

    /// Copy the `del`/`undel` operations of `remote_ops` that ticket `id`
    /// lacks (see [`Self::merge_fork_ticket`]). Returns whether any was copied.
    fn copy_fork_lifecycle_ops(&self, id: &Uuid, remote_ops: &[Op]) -> Result<bool> {
        let have: BTreeSet<String> = self.load_ops(id)?.into_iter().map(|op| op.id).collect();
        let p = self.project_handle();
        let mut copied = false;
        for op in remote_ops {
            let lifecycle = matches!(op.decoded(), Some(OpKind::Delete { .. } | OpKind::Undelete));
            if !lifecycle || have.contains(&op.id) {
                continue;
            }
            let op = Op {
                signature: None,
                signer: None,
                ..op.clone()
            };
            p.set(
                &keys::ticket_op(id, op.lamport, &op.id),
                serde_json::to_string(&op)?.as_str(),
            )?;
            copied = true;
        }
        Ok(copied)
    }

    fn import_fork_ticket(&self, remote: &Ticket) -> Result<()> {
        let p = self.project_handle();
        let id = remote.id;

        p.set(
            &keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD),
            CURRENT_TICKET_FORMAT.to_string().as_str(),
        )?;
        self.append_op(
            &id,
            OpKind::Create {
                fields: snapshot_scalar_fields(remote),
            },
        )?;

        for tag in &remote.tags {
            p.set_add(&keys::ticket_field(&id, "tags"), tag)?;
        }
        for child_id in &remote.children {
            p.set_add(&keys::ticket_field(&id, "children"), &child_id.to_string())?;
        }
        for (key, value) in &remote.meta {
            p.set(&keys::ticket_meta_field(&id, key), value.as_str())?;
        }
        for comment in &remote.comments {
            push_comment_as(&p, &id, &comment.author, &comment.body)?;
        }
        Ok(())
    }

    fn merge_fork_into(&self, local: &Ticket, remote: &Ticket) -> Result<ForkMerge> {
        let id = &local.id;
        let p = self.project_handle();
        let mut changed = false;

        let want = snapshot_scalar_fields(remote);
        let have = snapshot_scalar_fields(local);
        let mut set = BTreeMap::new();
        let mut clear = Vec::new();
        for field in FORK_MERGED_SCALARS.iter().copied().chain(["parent"]) {
            match (want.get(field), have.get(field)) {
                (Some(w), h) if h != Some(w) => {
                    set.insert(field.to_string(), w.clone());
                }
                (None, Some(_)) if field != "parent" => clear.push(field.to_string()),
                _ => {}
            }
        }
        let new_parent = set.get("parent").and_then(|v| Uuid::parse_str(v).ok());
        if !set.is_empty() || !clear.is_empty() {
            self.apply_scalars(id, set, clear)?;
            changed = true;
        }
        if let Some(new_parent) = new_parent {
            // Keep the denormalized children sets in step with the parent op.
            if let Some(old_parent) = local.parent {
                remove_member(
                    &p,
                    &keys::ticket_field(&old_parent, "children"),
                    &id.to_string(),
                )?;
            }
            p.set_add(
                &keys::ticket_field(&new_parent, "children"),
                &id.to_string(),
            )?;
        }

        for tag in remote.tags.difference(&local.tags) {
            p.set_add(&keys::ticket_field(id, "tags"), tag)?;
            changed = true;
        }
        for (key, value) in &remote.meta {
            if local.meta.get(key) != Some(value) {
                p.set(&keys::ticket_meta_field(id, key), value.as_str())?;
                changed = true;
            }
        }
        for child_id in remote.children.difference(&local.children) {
            p.set_add(&keys::ticket_field(id, "children"), &child_id.to_string())?;
            changed = true;
        }

        let known: BTreeSet<(&str, &str)> = local
            .comments
            .iter()
            .map(|c| (c.author.as_str(), c.body.as_str()))
            .collect();
        for comment in &remote.comments {
            if !known.contains(&(comment.author.as_str(), comment.body.as_str())) {
                push_comment_as(&p, id, &comment.author, &comment.body)?;
                changed = true;
            }
        }

        Ok(if changed {
            ForkMerge::Updated
        } else {
            ForkMerge::Unchanged
        })
    }

    // -------------------------------------------------------------------
    // Operation log (append-only, synced source of truth for scalars)
    // -------------------------------------------------------------------

    /// Next Lamport value for a ticket: one past the max found by scanning its
    /// op keys (0 if none). No separate counter — the scan that reads the log
    /// yields the max for free, consistent with the no-index model.
    pub fn next_lamport(&self, id: &Uuid) -> Result<u64> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::ticket_ops_prefix(id)))?;
        let mut max: Option<u64> = None;
        for (key, _) in pairs {
            if let Some((_, lamport, _)) = keys::parse_ticket_op(&key) {
                max = Some(max.map_or(lamport, |m| m.max(lamport)));
            }
        }
        Ok(max.map_or(0, |m| m + 1))
    }

    /// Append an operation to a ticket's log and return it. The op's Lamport is
    /// assigned from [`Self::next_lamport`] and its content-derived id becomes
    /// part of the key, so the write is to a fresh key (append-only, never an
    /// overwrite).
    pub fn append_op(&self, id: &Uuid, kind: OpKind) -> Result<Op> {
        let mut lamport = self.next_lamport(id)?;
        // A ticket with plain fields but no ops (legacy / never migrated) must
        // get its `create` op first, or this op would land at Lamport 0 and the
        // projection would discard the plain fields.
        if lamport == 0 && !matches!(kind, OpKind::Create { .. }) {
            let p = self.session.target(&Target::project());
            let prefix = keys::ticket_prefix(id);
            let fields: Vec<(String, MetaValue)> = p
                .get_all_values(Some(&prefix))?
                .into_iter()
                .filter_map(|(k, v)| keys::parse_ticket_field(&k).map(|(_, f)| (f.to_string(), v)))
                .collect();
            if !fields.is_empty() {
                self.convert_legacy(&p, *id, &fields)?;
                lamport = self.next_lamport(id)?;
            }
        }
        let author = self.session.email().to_string();
        let mut op = Op::new(lamport, &author, OffsetDateTime::now_utc(), kind)?;
        let p = self.session.target(&Target::project());

        // Sign the op's content id if a signing key is configured, and publish
        // the public key into the identity chain so other clones can verify.
        if let Some(ref key) = self.signing_key {
            let signature = crate::signing::sign(op.id.as_bytes(), key)?;
            let pubkey = crate::signing::public_key(key)?;
            p.set(&keys::identity(&author), pubkey.as_str())?;
            op.signature = Some(signature);
            op.signer = Some(author);
        }

        p.set(
            &keys::ticket_op(id, op.lamport, &op.id),
            serde_json::to_string(&op)?.as_str(),
        )?;
        Ok(op)
    }

    /// Consistency oracle: for every ticket, confirm each op's stored bytes
    /// still hash to its key and envelope id (catching corruption or tampering)
    /// and that the log projects to a valid ticket. Read-only; returns a
    /// per-ticket report. Unknown op kinds are not errors (forward-compat).
    pub fn verify(&self) -> Result<Vec<VerifyOutcome>> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut ops_by_id: BTreeMap<Uuid, Vec<(String, Op)>> = BTreeMap::new();
        let mut fields_by_id: BTreeMap<Uuid, Vec<(String, MetaValue)>> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, _lamport, hash)) = keys::parse_ticket_op(&key) {
                if let MetaValue::String(s) = value {
                    let op = serde_json::from_str::<Op>(&s)?;
                    ops_by_id
                        .entry(id)
                        .or_default()
                        .push((hash.to_string(), op));
                }
            } else if let Some((id, field)) = keys::parse_ticket_field(&key) {
                fields_by_id
                    .entry(id)
                    .or_default()
                    .push((field.to_string(), value));
            }
        }

        let ids: BTreeSet<Uuid> = fields_by_id
            .keys()
            .chain(ops_by_id.keys())
            .copied()
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        let mut live: BTreeMap<Uuid, Ticket> = BTreeMap::new();
        for id in ids {
            let mut issues = Vec::new();
            let mut warnings = Vec::new();
            let ops_with_hash = ops_by_id.remove(&id).unwrap_or_default();
            let fields = fields_by_id.remove(&id).unwrap_or_default();

            let mut ops = Vec::with_capacity(ops_with_hash.len());
            for (key_hash, op) in ops_with_hash {
                if op.id != key_hash {
                    issues.push(format!(
                        "op key hash {key_hash} does not match envelope id {}",
                        op.id
                    ));
                }
                match op.recompute_id() {
                    Ok(rid) if rid == op.id => {}
                    Ok(rid) => issues.push(format!(
                        "op {} content-id mismatch (recomputed {rid}) — tampered or corrupt",
                        op.id
                    )),
                    Err(e) => issues.push(format!("op {} failed to hash: {e}", op.id)),
                }

                // Signature check (step 5). Unsigned ops are trusted by default
                // but flagged; a present-but-invalid signature is a hard issue.
                match (&op.signature, &op.signer) {
                    (Some(sig), Some(signer)) => match p.get_value(&keys::identity(signer))? {
                        Some(MetaValue::String(pubkey)) => {
                            match crate::signing::verify(op.id.as_bytes(), sig, signer, &pubkey) {
                                Ok(true) => {}
                                Ok(false) => issues.push(format!(
                                    "op {} has an invalid signature from {signer}",
                                    op.id
                                )),
                                Err(e) => issues.push(format!(
                                    "op {} signature could not be checked: {e}",
                                    op.id
                                )),
                            }
                        }
                        _ => issues.push(format!(
                            "op {} is signed by {signer} but that identity has no published key",
                            op.id
                        )),
                    },
                    _ => warnings.push(format!("op {} is unsigned", op.id)),
                }
                ops.push(op);
            }

            let projected = project_scalar_fields(fields, &ops);
            if let Some(ticket) = build_ticket(id, projected) {
                live.insert(id, ticket);
            } else {
                // Distinguish a genuinely corrupt ticket from deletion/creation
                // residue. A live ticket is evidenced by a `create` op (v2
                // format); a legacy ticket is evidenced by a `title` field and
                // would have projected to `Some` above. A record with neither —
                // a lone `format-version` marker, or an empty relation set left
                // behind by reverse-reference cleanup — is not a ticket at all.
                // Skip it (matching `list()`, which drops such records) so
                // verify does not report already-dead residue as corruption and
                // its ticket count matches the projected store.
                let has_create = ops
                    .iter()
                    .any(|op| matches!(op.decoded(), Some(OpKind::Create { .. })));
                if has_create {
                    issues.push("does not project to a valid ticket state".to_string());
                } else if issues.is_empty() {
                    // No create op and no integrity problems: pure residue.
                    continue;
                }
                // else: no create op but op-integrity issues exist — fall
                // through and report those issues below.
            }

            out.push(VerifyOutcome {
                id,
                short_id: id.to_string().chars().take(6).collect(),
                ok: issues.is_empty(),
                issues,
                warnings,
            });
        }

        let index: HashMap<Uuid, usize> = out.iter().enumerate().map(|(i, o)| (o.id, i)).collect();
        for (id, issue) in relation_asymmetries(&live) {
            if let Some(&i) = index.get(&id) {
                out[i].issues.push(issue);
                out[i].ok = false;
            }
        }
        Ok(out)
    }

    /// Load every operation for a ticket (unordered; [`crate::oplog::replay`]
    /// sorts and folds them).
    pub fn load_ops(&self, id: &Uuid) -> Result<Vec<Op>> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::ticket_ops_prefix(id)))?;
        let mut ops = Vec::new();
        for (key, value) in pairs {
            if keys::parse_ticket_op(&key).is_some() {
                if let MetaValue::String(s) = value {
                    ops.push(serde_json::from_str::<Op>(&s)?);
                }
            }
        }
        Ok(ops)
    }

    #[cfg(test)]
    fn set_meta_scalar_for_test(&self, id: &Uuid, field: &str, value: &str) {
        let mut fields = BTreeMap::new();
        fields.insert(field.to_string(), value.to_string());
        self.append_op(id, OpKind::SetField { fields }).unwrap();
    }

    /// Readable change history for a ticket, newest first: one entry per field
    /// change in the op log, plus comments. A ticket with no ops (legacy
    /// format) gets a synthesized `Created` entry.
    pub fn history(&self, id: &Uuid) -> Result<Vec<crate::history::HistoryEntry>> {
        let ticket = self.load(id)?;
        let ops = self.load_ops(id)?;
        Ok(crate::history::project_history(
            &ops,
            &ticket.comments,
            Some((ticket.created_at, &ticket.created_by, &ticket.title)),
        ))
    }

    /// When a ticket was closed, from the op log. `None` if it is not closed
    /// or has no op recording the close (legacy format).
    pub fn closed_at(&self, id: &Uuid) -> Result<Option<OffsetDateTime>> {
        Ok(crate::history::project_closed_at(&self.load_ops(id)?))
    }

    /// Close times for every closed ticket that has one, in a single scan.
    pub fn closed_times(&self) -> Result<std::collections::HashMap<Uuid, OffsetDateTime>> {
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut ops_by_id: BTreeMap<Uuid, Vec<Op>> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, _, _)) = keys::parse_ticket_op(&key) {
                if let MetaValue::String(s) = value {
                    ops_by_id
                        .entry(id)
                        .or_default()
                        .push(serde_json::from_str::<Op>(&s)?);
                }
            }
        }
        Ok(ops_by_id
            .into_iter()
            .filter_map(|(id, ops)| crate::history::project_closed_at(&ops).map(|t| (id, t)))
            .collect())
    }

    // -------------------------------------------------------------------
    // Sync porcelain
    // -------------------------------------------------------------------

    pub fn serialize(&self) -> Result<()> {
        let _ = self.session.serialize()?;
        Ok(())
    }

    /// Fetch and merge the remote meta ref, then run the additive
    /// [`reindex`](Self::reindex) against the fetched tip.
    ///
    /// git-meta's three-way merge treats a key the index lacks as removed
    /// locally, and it can report the store "already up-to-date" while the
    /// index is missing tickets that live in the fetched tree (a Stranded
    /// index). The additive reindex puts those back: it only adds keys the
    /// index lacks and never removed, so it is safe to run after every merge
    /// and needs no guess about whether the index is stranded. Unsynced local
    /// comments, Sub-issues, Dependencies and Deletions are never touched, and
    /// the local serialization ref is never dropped (ADR 0003).
    pub fn pull(&self, remote: Option<&str>) -> Result<()> {
        let _ = self.session.pull(remote)?;
        let ns = self.session.namespace().to_string();
        self.gain_from_refs(&[format!("refs/{ns}/remotes/main")])?;
        Ok(())
    }

    /// Push the local store to `remote`, returning how many times the push
    /// had to be retried because the remote advanced underneath it.
    ///
    /// Fail-closed against *implicit* deletion: unless `allow_deletes` is set,
    /// the push is refused when it would remove tickets that are on the remote
    /// but absent from the local store (the stranded/incomplete-index clobber).
    /// A ticket is removed from the remote only when the operator opts in, never
    /// as a side effect of serializing a partial local tree.
    ///
    /// Integrity gate: unless `skip_verify` is set, a store that fails
    /// [`verify`](Self::verify) is refused with [`Error::VerifyFailed`]; the
    /// shared remote is never overwritten by a store that cannot replay every
    /// ticket to a valid state.
    ///
    /// Race window: the remote may advance between our last fetch and the
    /// push (another clone syncing in that window), in which case git rejects
    /// the push as non-fast-forward. That rejection is never swallowed: the
    /// remote tip is fetched and merged into the local store, the store is
    /// re-serialized on top of it, and the push is retried, up to
    /// [`MAX_PUSH_ATTEMPTS`] times. Both guards above re-run on every attempt,
    /// since the merge changes what will be pushed. If the remote keeps moving
    /// the push fails loudly with [`Error::PushRejected`] rather than reporting
    /// success for a change that never left this clone.
    pub fn push(
        &self,
        remote: Option<&str>,
        allow_deletes: bool,
        skip_verify: bool,
    ) -> Result<usize> {
        for attempt in 0..MAX_PUSH_ATTEMPTS {
            if !allow_deletes {
                let removed = self.tickets_removed_by_push()?;
                if !removed.is_empty() {
                    return Err(Error::ImplicitDeletion(removed.into_iter().collect()));
                }
            }
            if !skip_verify {
                self.verify_gate()?;
            }
            // Defense in depth: snapshot the current remote tip to a local
            // backup ref before overwriting it, so even a push that slips past
            // the guards stays reversible.
            self.snapshot_remote_ref()?;
            let out = self.session.push_once(remote)?;
            if out.success {
                return Ok(attempt);
            }
            if !out.non_fast_forward {
                return Err(Error::PushRejected {
                    remote: out.remote_name,
                    attempts: attempt + 1,
                });
            }
            // Somebody pushed since we fetched: merge their tip into the local
            // store, re-serialize onto it, and go around again.
            self.session.resolve_push_conflict(remote)?;
        }
        Err(Error::PushRejected {
            remote: remote.unwrap_or("<meta remote>").to_string(),
            attempts: MAX_PUSH_ATTEMPTS,
        })
    }

    /// The integrity gate behind [`push`](Self::push): [`Error::VerifyFailed`]
    /// listing every ticket that fails [`verify`](Self::verify), else `Ok`.
    pub fn verify_gate(&self) -> Result<()> {
        let report = self.verify()?;
        let failed: Vec<_> = report.iter().filter(|o| !o.ok).collect();
        if failed.is_empty() {
            return Ok(());
        }
        let mut details = String::new();
        for o in &failed {
            details.push_str(&format!("\n  {}", o.short_id));
            for issue in &o.issues {
                details.push_str(&format!("\n    - {issue}"));
            }
        }
        Err(Error::VerifyFailed {
            failed: failed.len(),
            total: report.len(),
            details,
        })
    }

    /// Root of the local pre-push backup refs for this namespace.
    ///
    /// Deliberately OUTSIDE `refs/{ns}/`: git-meta's `materialize(None)` (which
    /// [`reindex_reset`](Self::reindex_reset) drives) replays every non-local
    /// ref under `refs/{ns}/`, so a backup kept there would be replayed into
    /// the store; the additive [`reindex`](Self::reindex) reads only
    /// `refs/{ns}/remotes/*`.
    fn backup_root(&self) -> String {
        format!("refs/ticgit-backup/{}", self.session.namespace())
    }

    /// Snapshot the current remote meta tip to a local backup ref before an
    /// overwriting push, pruning to the newest [`MAX_BACKUP_SNAPSHOTS`]. A no-op
    /// when there is no remote tip yet (a first push has nothing to overwrite)
    /// or when the current tip is already the latest snapshot.
    ///
    /// Restore the remote from a snapshot with:
    /// `git push <remote> +<backup-ref>:refs/{ns}/main && ti reindex`.
    fn snapshot_remote_ref(&self) -> Result<()> {
        let ns = self.session.namespace().to_string();
        let repo = self.session.repo();

        let Some(oid) = repo
            .find_reference(&format!("refs/{ns}/remotes/main"))
            .ok()
            .and_then(|r| r.into_fully_peeled_id().ok())
            .map(|id| id.detach())
        else {
            return Ok(());
        };

        let backup_root = self.backup_root();
        // Don't let no-op pushes pile up identical snapshots.
        if self
            .backup_refs(repo, &backup_root)?
            .iter()
            .any(|(_, o)| *o == oid)
        {
            return Ok(());
        }

        let ts = OffsetDateTime::now_utc().unix_timestamp();
        let short: String = oid.to_string().chars().take(8).collect();
        let name = format!("{backup_root}/{ts}-{short}");
        repo.reference(
            name,
            oid,
            gix::refs::transaction::PreviousValue::Any,
            "ticgit: pre-push backup of remote meta tip",
        )
        .map_err(|e| Error::InvalidValue(format!("writing backup ref: {e}")))?;

        self.prune_backups(repo, &backup_root)?;
        Ok(())
    }

    /// All backup refs under `backup_root` as `(full_ref_name, tip_oid)`.
    fn backup_refs(
        &self,
        repo: &gix::Repository,
        backup_root: &str,
    ) -> Result<Vec<(String, gix::ObjectId)>> {
        let prefix = format!("{backup_root}/");
        let mut out = Vec::new();
        for r in repo
            .references()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .all()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .filter_map(std::result::Result::ok)
        {
            let name = r.name().as_bstr().to_string();
            if !name.starts_with(&prefix) {
                continue;
            }
            if let Ok(id) = r.into_fully_peeled_id() {
                out.push((name, id.detach()));
            }
        }
        Ok(out)
    }

    /// Keep only the newest [`MAX_BACKUP_SNAPSHOTS`] backup refs. Names are
    /// `<unix_ts>-<short>`, so a lexical sort on the full ref name is
    /// chronological for the foreseeable future.
    fn prune_backups(&self, repo: &gix::Repository, backup_root: &str) -> Result<()> {
        let mut refs = self.backup_refs(repo, backup_root)?;
        if refs.len() <= MAX_BACKUP_SNAPSHOTS {
            return Ok(());
        }
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        let drop_count = refs.len() - MAX_BACKUP_SNAPSHOTS;
        for (name, _) in refs.into_iter().take(drop_count) {
            if let Ok(reference) = repo.find_reference(&name) {
                let _ = reference.delete();
            }
        }
        Ok(())
    }

    /// Ticket ids that a push would remove from the remote: present on the
    /// fetched remote tracking ref but absent from the freshly-serialized local
    /// tree. A non-empty result means the push would implicitly delete those
    /// remote tickets.
    ///
    /// Serializes first so the comparison reflects exactly what [`push`] will
    /// write. Compares git trees by ticket-id directory, so it is faithful to
    /// the push regardless of why the local index is incomplete.
    pub fn tickets_removed_by_push(&self) -> Result<BTreeSet<Uuid>> {
        self.serialize()?;
        let ns = self.session.namespace();
        let local = self.ticket_ids_in_ref(&format!("refs/{ns}/local/main"))?;
        let remote = self.ticket_ids_in_ref(&format!("refs/{ns}/remotes/main"))?;
        Ok(remote.difference(&local).copied().collect())
    }

    /// Ticket ids present under [`keys::tickets_tree_path`] in the git tree at
    /// `ref_name`. Returns an empty set when the ref — or that subtree — does
    /// not exist yet (e.g. a remote that has never received tickets).
    fn ticket_ids_in_ref(&self, ref_name: &str) -> Result<BTreeSet<Uuid>> {
        use gix::bstr::ByteSlice;

        let repo = self.session.repo();
        let spec = format!("{ref_name}:{}", keys::tickets_tree_path());
        let tree = match repo.rev_parse_single(spec.as_bytes()) {
            Ok(id) => id
                .object()
                .map_err(|e| Error::InvalidValue(format!("reading {ref_name}: {e}")))?
                .into_tree(),
            // The ref or the tickets subtree is absent.
            Err(_) => return Ok(BTreeSet::new()),
        };

        let mut ids = BTreeSet::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
            if !entry.mode().is_tree() {
                continue;
            }
            if let Ok(id) = Uuid::parse_str(&entry.filename().to_str_lossy()) {
                ids.insert(id);
            }
        }
        Ok(ids)
    }

    /// Additive reindex: gain from the fetched remote tracking refs
    /// (`refs/{ns}/remotes/*`) every ticket key the index lacks, returning the
    /// ticket count afterwards. This is what `ti reindex` runs, and what every
    /// [`pull`](Self::pull) runs after its merge.
    ///
    /// Recovers tickets that live in the refs but are missing from the local
    /// `git-meta.sqlite` (a Stranded index: the store file was lost, or ops
    /// were serialized and pushed from a since-removed worktree yet never
    /// materialized here). It only adds; it never removes or rewrites a key,
    /// a set member or a list entry the index holds:
    ///
    /// - a ticket the index has no keys for gains all of its keys;
    /// - a ticket the index holds gains only the whole keys it lacks, e.g. the
    ///   earlier operations and comments of a ticket the index had lost when
    ///   the merge brought in only what the remote appended since. Keys it
    ///   holds are left exactly as they are (no member or entry is added to
    ///   them), and legacy scalar keys `ti migrate` removed stay removed.
    ///
    /// Nothing else in ticgit removes a ticket key, so a lost key is gained
    /// even when git-meta's merge recorded its loss as a local removal (a key
    /// tombstone). Deleted tickets keep their keys (Deletion is an
    /// Operation), so they never qualify. After gaining anything the store is serialized in
    /// full so the local ref carries the gained keys. The wholesale rebuild
    /// that replaces the index with the remote tree is
    /// [`reindex_reset`](Self::reindex_reset).
    ///
    /// Requires at least one fetched remote tracking ref; run a sync first.
    ///
    /// On a blobless partial clone a ticket's meta blobs may be absent. Those
    /// are backfilled from the meta remote and the gain runs again; if a
    /// ticket stays unreadable it fails with [`Error::IncompleteReindex`]
    /// (every readable ticket is still gained).
    pub fn reindex(&self) -> Result<usize> {
        let refs = self.tracking_refs()?;
        if refs.is_empty() {
            let ns = self.session.namespace();
            return Err(Error::InvalidValue(format!(
                "reindex needs a fetched meta ref (refs/{ns}/remotes/*); run a sync first"
            )));
        }
        self.gain_from_refs(&refs)?;
        Ok(self.list()?.len())
    }

    /// Every fetched remote tracking ref under `refs/{ns}/remotes/`.
    fn tracking_refs(&self) -> Result<Vec<String>> {
        let prefix = format!("refs/{}/remotes/", self.session.namespace());
        Ok(self
            .session
            .repo()
            .references()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .all()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .filter_map(std::result::Result::ok)
            .map(|r| r.name().as_bstr().to_string())
            .filter(|n| n.starts_with(&prefix))
            .collect())
    }

    /// Gain the keys the index lacks from each of `refs` (see
    /// [`reindex`](Self::reindex)), backfilling blobs once when a ticket's
    /// objects are missing, then serialize in full if anything was gained.
    fn gain_from_refs(&self, refs: &[String]) -> Result<()> {
        let gain_all = || -> Result<GainOutcome> {
            let mut total = GainOutcome::default();
            for r in refs {
                let g = self.gain_from_ref(r)?;
                total.keys += g.keys;
                total.unreadable.extend(g.unreadable);
            }
            Ok(total)
        };
        let mut outcome = gain_all()?;
        if !outcome.unreadable.is_empty() && self.backfill_missing_blobs().is_ok() {
            let retry = gain_all()?;
            outcome = GainOutcome {
                keys: outcome.keys + retry.keys,
                unreadable: retry.unreadable,
            };
        }
        if outcome.keys > 0 {
            let _ = self.session.serialize_full()?;
        }
        if !outcome.unreadable.is_empty() {
            return Err(Error::IncompleteReindex {
                missing: outcome.unreadable.len(),
                ns: self.session.namespace().to_string(),
            });
        }
        Ok(())
    }

    /// One additive pass over the tickets in `ref_name`'s tree: apply every
    /// ticket key the index lacks (see [`reindex`](Self::reindex) for the one
    /// exception). A ticket with an unreadable blob is skipped whole and
    /// reported in `unreadable`.
    fn gain_from_ref(&self, ref_name: &str) -> Result<GainOutcome> {
        use git_meta_lib::tree::format::parse_tree;
        use git_meta_lib::tree::model::ParsedTree;
        use gix::bstr::ByteSlice;

        let mut outcome = GainOutcome::default();
        let repo = self.session.repo();
        let tickets_path = keys::tickets_tree_path();
        let spec = format!("{ref_name}:{tickets_path}");
        let Ok(tickets_tree) = repo.rev_parse_single(spec.as_bytes()) else {
            return Ok(outcome); // no such ref, or no tickets in it
        };
        let tickets_tree = tickets_tree
            .object()
            .map_err(|e| Error::InvalidValue(format!("reading {ref_name}: {e}")))?
            .into_tree();

        let held: BTreeSet<String> = self
            .project_handle()
            .get_all_values(Some(&keys::tickets_prefix()))?
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let indexed: BTreeSet<Uuid> = held
            .iter()
            .filter_map(|k| keys::parse_ticket_field(k).map(|(id, _)| id))
            .collect();

        let mut ticket_dirs = Vec::new();
        for entry in tickets_tree.iter() {
            let entry = entry.map_err(|e| Error::InvalidValue(format!("iterating tree: {e}")))?;
            if !entry.mode().is_tree() {
                continue;
            }
            if let Ok(id) = Uuid::parse_str(&entry.filename().to_str_lossy()) {
                ticket_dirs.push((id, entry.object_id()));
            }
        }

        for (id, dir) in ticket_dirs {
            let mut key_dirs = Vec::new();
            collect_key_dirs(repo, dir, &mut Vec::new(), &mut key_dirs)?;

            // Read every wanted key before applying any, so a ticket with an
            // unreadable blob is skipped whole rather than half-gained.
            let mut gain = ParsedTree::default();
            let mut readable = true;
            for (segments, oid) in key_dirs {
                let key = format!("{}:{}", keys::ticket_prefix(&id), segments.join(":"));
                // `ti migrate` removes legacy scalar keys from a ticket it has
                // converted to operations; never put those back.
                let migrated_away = indexed.contains(&id)
                    && keys::parse_ticket_field(&key)
                        .is_some_and(|(_, field)| is_op_managed_scalar(field));
                if held.contains(&key) || migrated_away {
                    continue;
                }
                let path = format!("{tickets_path}/{id}/{}", segments.join("/"));
                let Ok(parsed) = parse_tree(repo, oid, &path) else {
                    readable = false;
                    break;
                };
                // A key directory can hold deeper keys (`meta` next to
                // `meta:branch`); take only this key, never its neighbours.
                gain.values
                    .extend(parsed.values.into_iter().filter(|(k, _)| k.key == key));
                gain.set_tombstones.extend(
                    parsed
                        .set_tombstones
                        .into_iter()
                        .filter(|((k, _), _)| k.key == key),
                );
                gain.list_tombstones.extend(
                    parsed
                        .list_tombstones
                        .into_iter()
                        .filter(|((k, _), _)| k.key == key),
                );
            }
            if !readable {
                outcome.unreadable.insert(id);
                continue;
            }
            if gain.values.is_empty() {
                continue;
            }

            // The remote's member and entry tombstones only filter which
            // members of a gained value show; no key tombstone is applied.
            self.session.store().apply_tree(
                &gain.values,
                &BTreeMap::new(),
                &gain.set_tombstones,
                &gain.list_tombstones,
                self.session.email(),
                now_millis(),
            )?;
            outcome.keys += gain.values.len();
        }
        Ok(outcome)
    }

    /// Replace the index with the fetched remote tree, returning the ticket
    /// count afterwards. **Destructive**: unlike the additive
    /// [`reindex`](Self::reindex) it drops unsynced local work and brings
    /// back keys removed locally, so it is never part of a sync; it is the
    /// wholesale rebuild meant for a guarded `ti reindex --reset`.
    ///
    /// It drops the local serialization ref (`refs/{ns}/local/main`) so
    /// git-meta materializes every remote tracking ref as a fast-forward,
    /// overwriting each key with the remote tree's value (lists and sets are
    /// rebuilt from the tree, so local-only entries and members are lost),
    /// then removes ghost residue with `prune_orphan_tickets`.
    ///
    /// Requires at least one fetched remote tracking ref
    /// (`refs/{ns}/remotes/*`). If none exists it returns an error without
    /// touching any ref, so the only serialized copy is never dropped when it
    /// cannot be rebuilt — run a sync first in that case.
    ///
    /// On a blobless partial clone some referenced meta blobs may be absent, so
    /// a ticket cannot be materialized. reindex first tries to self-heal by
    /// backfilling those blobs from the meta remote and re-materializing. Only
    /// if that still leaves tickets unmaterializable does it fail loudly with
    /// [`Error::IncompleteReindex`] — git-meta's materialize is all-or-nothing,
    /// so a partial store cannot be produced safely; failing is better than
    /// silently dropping tickets.
    pub fn reindex_reset(&self) -> Result<usize> {
        let ns = self.session.namespace().to_string();
        let repo = self.session.repo();

        // Guard: refuse to drop the local ref unless there is a tracking ref to
        // rebuild from, otherwise we would strand the only serialized copy.
        let tracking_prefix = format!("refs/{ns}/remotes/");
        let has_tracking = repo
            .references()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .all()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .filter_map(std::result::Result::ok)
            .any(|r| r.name().as_bstr().to_string().starts_with(&tracking_prefix));
        if !has_tracking {
            return Err(Error::InvalidValue(format!(
                "reindex needs a fetched meta ref (refs/{ns}/remotes/*); run a sync first"
            )));
        }

        // Drop the local serialization ref so materialize re-applies the remote
        // tree from scratch instead of short-circuiting as up-to-date.
        let local_ref = format!("refs/{ns}/local/main");
        if let Ok(reference) = repo.find_reference(&local_ref) {
            reference
                .delete()
                .map_err(|e| Error::InvalidValue(format!("dropping {local_ref}: {e}")))?;
        }

        // Re-project every remote tracking ref into the store. On a blobless
        // partial clone the referenced meta blobs may be absent, so materialize
        // can abort or skip the affected tickets. We hold the result and decide
        // after a completeness check rather than failing eagerly.
        let mut materialize_result = self.session.materialize(None);

        // Completeness: every ticket id present in the remote meta tree must
        // leave at least one key in the store. Ids that leave none are
        // unmaterializable — typically because their meta blobs are absent on a
        // blobless partial clone.
        let expected = self.ticket_ids_in_ref(&format!("refs/{ns}/remotes/main"))?;
        let mut missing = self.unmaterialized_ids(&expected)?;

        // Self-heal: backfill the missing blobs from the meta remote and
        // re-materialize once, instead of making the operator run the fetch by
        // hand. Best-effort — if there is no remote, or the fetch fails, we fall
        // through with whatever is still missing. The re-materialize result
        // supersedes the first one so a now-resolved failure is not surfaced.
        if !missing.is_empty() && self.backfill_missing_blobs().is_ok() {
            materialize_result = self.session.materialize(None);
            missing = self.unmaterialized_ids(&expected)?;
        }

        // Still missing after the backfill attempt: fail loudly. git-meta's
        // materialize is all-or-nothing, so we cannot keep the healthy tickets
        // and drop only the unrecoverable ones — the store would be empty.
        // Failing (and pointing at the manual backfill) beats a silent
        // under-count.
        if !missing.is_empty() {
            return Err(Error::IncompleteReindex {
                missing: missing.len(),
                ns,
            });
        }

        // Nothing missing; surface any other materialize failure.
        let _ = materialize_result?;

        // Materialize never deletes rows for tickets that have since vanished
        // from refs/{ns}/*. Drop that ghost residue now so the reset is a
        // faithful projection of the refs — otherwise a ticket
        // removed from the refs (e.g. by a history rewrite) survives in the
        // store forever and keeps failing `verify`.
        self.prune_orphan_tickets()?;

        Ok(self.list()?.len())
    }

    /// Ticket ids from `expected` that left no key in the store — i.e. the
    /// remote meta tree references them but they could not be materialized
    /// (their objects are absent locally).
    fn unmaterialized_ids(&self, expected: &BTreeSet<Uuid>) -> Result<Vec<Uuid>> {
        let p = self.project_handle();
        let mut missing = Vec::new();
        for id in expected {
            if p.get_all_values(Some(&keys::ticket_prefix(id)))?.is_empty() {
                missing.push(*id);
            }
        }
        Ok(missing)
    }

    /// Best-effort backfill of meta blobs absent on a blobless partial clone:
    /// re-fetch the remote meta ref with a permissive blob filter so the
    /// previously-filtered ticket objects are hydrated locally. Returns `Ok`
    /// only when the fetch ran successfully; the caller re-checks completeness.
    fn backfill_missing_blobs(&self) -> Result<()> {
        let ns = self.session.namespace().to_string();
        let remote = self
            .session
            .resolve_remote(None)
            .map_err(|e| Error::InvalidValue(format!("no meta remote to backfill from: {e}")))?;
        let repo = self.session.repo();
        let refspec = format!("+refs/{ns}/main:refs/{ns}/remotes/main");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.git_dir())
            .args([
                "fetch",
                "--refetch",
                &remote,
                &refspec,
                "--filter=blob:limit=5m",
            ])
            .status()
            .map_err(|e| Error::InvalidValue(format!("running git fetch for backfill: {e}")))?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::InvalidValue(format!(
                "backfill fetch from {remote} failed"
            )))
        }
    }

    /// Remove ghost residue that the [`reindex_reset`](Self::reindex_reset)
    /// materialize cannot:
    /// ticket records present in the store but absent from **every**
    /// `refs/{ns}/*` tree that also fail to project to a real ticket (no
    /// `create` op and no legacy `title`). These are rewritten-away or
    /// never-completed tickets — a lone `format-version` marker, or an empty
    /// relation set left by reverse-reference cleanup.
    ///
    /// A record is kept when it is backed by a ref (someone else's live
    /// ticket we just materialized) OR when it projects to a valid ticket (a
    /// local-only ticket created but not yet pushed). Only unbacked,
    /// non-projecting residue is removed.
    fn prune_orphan_tickets(&self) -> Result<()> {
        let ns = self.session.namespace().to_string();

        // Ids backed by any meta ref: the local serialization ref plus every
        // remote tracking ref under refs/{ns}/.
        let repo = self.session.repo();
        let prefix = format!("refs/{ns}/");
        let ref_names: Vec<String> = repo
            .references()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .all()
            .map_err(|e| Error::InvalidValue(format!("reading refs: {e}")))?
            .filter_map(std::result::Result::ok)
            .map(|r| r.name().as_bstr().to_string())
            .filter(|n| n.starts_with(&prefix))
            .collect();
        let mut ref_ids: BTreeSet<Uuid> = BTreeSet::new();
        for name in &ref_names {
            ref_ids.extend(self.ticket_ids_in_ref(name)?);
        }

        // Group the store's ticket records by id (ops and scalar fields),
        // mirroring `verify`'s enumeration so the same records are classified.
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut ops_by_id: BTreeMap<Uuid, Vec<Op>> = BTreeMap::new();
        let mut fields_by_id: BTreeMap<Uuid, Vec<(String, MetaValue)>> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, _lamport, _hash)) = keys::parse_ticket_op(&key) {
                if let MetaValue::String(s) = value {
                    if let Ok(op) = serde_json::from_str::<Op>(&s) {
                        ops_by_id.entry(id).or_default().push(op);
                    }
                }
            } else if let Some((id, field)) = keys::parse_ticket_field(&key) {
                fields_by_id
                    .entry(id)
                    .or_default()
                    .push((field.to_string(), value));
            }
        }

        let ids: BTreeSet<Uuid> = fields_by_id
            .keys()
            .chain(ops_by_id.keys())
            .copied()
            .collect();
        for id in ids {
            if ref_ids.contains(&id) {
                continue; // backed by a ref — keep.
            }
            let ops = ops_by_id.remove(&id).unwrap_or_default();
            let has_create = ops
                .iter()
                .any(|op| matches!(op.decoded(), Some(OpKind::Create { .. })));
            if has_create {
                continue; // a real (if unpushed) ticket — keep.
            }
            let fields = fields_by_id.remove(&id).unwrap_or_default();
            let projected = project_scalar_fields(fields, &ops);
            if build_ticket(id, projected).is_some() {
                continue; // legacy ticket with a title — keep.
            }
            // Unbacked, non-projecting residue: remove every key under this id.
            for (key, _) in p.get_all_values(Some(&keys::ticket_prefix(&id)))? {
                p.remove(&key)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Op-log projection
// ---------------------------------------------------------------------------

/// Every place where the two denormalized sides of a Sub-issue or Dependency
/// disagree among `live` tickets, as `(ticket to report it on, issue)`. Each
/// asymmetry yields exactly one issue, reported on the Sub-issue or
/// the dependent (Dependency), naming both tickets. References to tickets that
/// are not live are not asymmetries between two tickets and are skipped.
fn relation_asymmetries(live: &BTreeMap<Uuid, Ticket>) -> Vec<(Uuid, String)> {
    let mut out = Vec::new();
    for t in live.values() {
        if let Some(parent) = t.parent.as_ref().and_then(|p| live.get(p)) {
            if !parent.children.contains(&t.id) {
                out.push((
                    t.id,
                    format!(
                        "sub-issue asymmetry: {} has parent {p}, but {p}'s children lack it",
                        t.short_id(),
                        p = parent.short_id(),
                    ),
                ));
            }
        }
        for sub in t.children.iter().filter_map(|c| live.get(c)) {
            if sub.parent != Some(t.id) {
                let actual = sub.parent.map_or("none".to_string(), |p| {
                    p.to_string().chars().take(6).collect()
                });
                out.push((
                    sub.id,
                    format!(
                        "sub-issue asymmetry: {} lists {s} as a sub-issue, but {s}'s parent is {actual}",
                        t.short_id(),
                        s = sub.short_id(),
                    ),
                ));
            }
        }
        for dep in t.depends_on.iter().filter_map(|d| live.get(d)) {
            if !dep.blocks.contains(&t.id) {
                out.push((
                    t.id,
                    format!(
                        "dependency asymmetry: {} depends on {d}, but {d}'s blocks lack it",
                        t.short_id(),
                        d = dep.short_id(),
                    ),
                ));
            }
        }
        for dependent in t.blocks.iter().filter_map(|b| live.get(b)) {
            if !dependent.depends_on.contains(&t.id) {
                out.push((
                    dependent.id,
                    format!(
                        "dependency asymmetry: {} blocks {d}, but {d}'s depends_on lacks it",
                        t.short_id(),
                        d = dependent.short_id(),
                    ),
                ));
            }
        }
    }
    out
}

/// Append a comment attributed to `author` (the session user for new
/// comments, the original author for fork import/merge).
fn push_comment_as(
    handle: &git_meta_lib::SessionTargetHandle<'_>,
    id: &Uuid,
    author: &str,
    body: &str,
) -> Result<()> {
    let payload = CommentBody {
        author: author.to_string(),
        body: body.to_string(),
    };
    handle.list_push(
        &keys::ticket_field(id, "comments"),
        &serde_json::to_string(&payload)?,
    )?;
    Ok(())
}

/// Scalar singleton fields whose authority is the op-log once a ticket has any
/// ops. Sets (`tags`, `children`, `depends_on`, `blocks`), `comments`, `meta:*`
/// and `format-version` are NOT here — they keep their own git-meta keys (sets
/// and comments already merge conflict-free; meta is low-conflict).
fn is_op_managed_scalar(field: &str) -> bool {
    matches!(
        field,
        "title"
            | "description"
            | "spec"
            | "status"
            | "state"
            | "assigned"
            | "closed-by"
            | "priority"
            | "points"
            | "milestone"
            | "code"
            | "parent"
            | "created-at"
            | "created-by"
    )
}

/// Resolve the scalar fields fed to [`build_ticket`]. With no ops, the raw
/// scan is used unchanged (legacy / pre-migration tickets). With ops present,
/// op-managed scalars come entirely from replaying the log (the conflict-free
/// source of truth), while non-scalar keys (sets, comments, meta) pass through.
fn project_scalar_fields(fields: Vec<(String, MetaValue)>, ops: &[Op]) -> Vec<(String, MetaValue)> {
    if ops.is_empty() {
        return fields;
    }
    let mut out: Vec<(String, MetaValue)> = fields
        .into_iter()
        .filter(|(field, _)| !is_op_managed_scalar(field))
        .collect();
    for (field, value) in crate::oplog::replay(ops) {
        out.push((field, MetaValue::String(value)));
    }
    out
}

/// Snapshot a ticket's op-managed scalar fields into the `create`-op payload
/// used by `ti migrate` v1→v2. Only present (non-`None`) fields are included;
/// values are the canonical current forms (legacy state folding already applied
/// by [`build_ticket`]).
fn snapshot_scalar_fields(t: &Ticket) -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    f.insert("title".to_string(), t.title.clone());
    f.insert("status".to_string(), t.status.as_str().to_string());
    f.insert("state".to_string(), t.state.as_str().to_string());
    if let Ok(s) = t.created_at.format(&Rfc3339) {
        f.insert("created-at".to_string(), s);
    }
    f.insert("created-by".to_string(), t.created_by.clone());
    if let Some(ref a) = t.assigned {
        f.insert("assigned".to_string(), a.clone());
    }
    if let Some(ref c) = t.closed_by {
        f.insert("closed-by".to_string(), c.clone());
    }
    if let Some(p) = t.priority {
        f.insert("priority".to_string(), p.to_string());
    }
    if let Some(p) = t.points {
        f.insert("points".to_string(), p.to_string());
    }
    if let Some(ref m) = t.milestone {
        f.insert("milestone".to_string(), m.clone());
    }
    if let Some(ref c) = t.code {
        f.insert("code".to_string(), c.clone());
    }
    if let Some(parent) = t.parent {
        f.insert("parent".to_string(), parent.to_string());
    }
    if let Some(ref d) = t.description {
        f.insert("description".to_string(), d.clone());
    }
    if let Some(ref s) = t.spec {
        f.insert("spec".to_string(), s.clone());
    }
    f
}

// ---------------------------------------------------------------------------
// Field-level deserialisation
// ---------------------------------------------------------------------------

fn build_ticket(id: Uuid, fields: Vec<(String, MetaValue)>) -> Option<Ticket> {
    if fields.is_empty() {
        return None;
    }

    let mut title: Option<String> = None;
    let mut description: Option<String> = None;
    let mut spec: Option<String> = None;
    let mut status: Option<TicketStatus> = None;
    let mut state: Option<TicketState> = None;
    let mut legacy_status: Option<TicketStatus> = None;
    let mut legacy_state: Option<TicketState> = None;
    let mut assigned: Option<String> = None;
    let mut closed_by: Option<String> = None;
    let mut priority: Option<i64> = None;
    let mut points: Option<i64> = None;
    let mut milestone: Option<String> = None;
    let mut code: Option<String> = None;
    let mut parent: Option<Uuid> = None;
    let mut children: BTreeSet<Uuid> = BTreeSet::new();
    let mut depends_on: BTreeSet<Uuid> = BTreeSet::new();
    let mut blocks: BTreeSet<Uuid> = BTreeSet::new();
    let mut tags: BTreeSet<String> = BTreeSet::new();
    let mut meta: BTreeMap<String, String> = BTreeMap::new();
    let mut comments: Vec<Comment> = Vec::new();
    let mut created_at: Option<OffsetDateTime> = None;
    let mut created_by = String::new();

    for (field, value) in fields {
        match (field.as_str(), value) {
            ("title", MetaValue::String(s)) => title = Some(s),
            ("description", MetaValue::String(s)) => description = Some(s),
            ("spec", MetaValue::String(s)) => spec = Some(s),
            ("status", MetaValue::String(s)) => {
                status = TicketStatus::parse(&s).ok();
            }
            ("state", MetaValue::String(s)) => match s.as_str() {
                "open" => {
                    legacy_status = Some(TicketStatus::Open);
                    legacy_state = Some(TicketState::New);
                }
                "hold" => {
                    legacy_status = Some(TicketStatus::Open);
                    legacy_state = Some(TicketState::Blocked);
                }
                "resolved" => {
                    legacy_status = Some(TicketStatus::Closed);
                    legacy_state = Some(TicketState::Resolved);
                }
                "invalid" => {
                    legacy_status = Some(TicketStatus::Closed);
                    legacy_state = Some(TicketState::Invalid);
                }
                _ => {
                    state = TicketState::parse(&s).ok();
                }
            },
            ("assigned", MetaValue::String(s)) => assigned = Some(s),
            ("closed-by", MetaValue::String(s)) => closed_by = Some(s),
            ("priority", MetaValue::String(s)) => priority = s.parse().ok(),
            ("points", MetaValue::String(s)) => points = s.parse().ok(),
            ("milestone", MetaValue::String(s)) => milestone = Some(s),
            ("code", MetaValue::String(s)) => code = Some(s),
            ("parent", MetaValue::String(s)) => parent = Uuid::parse_str(&s).ok(),
            ("children", MetaValue::Set(members)) => {
                children = members
                    .iter()
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
            }
            ("depends_on", MetaValue::Set(members)) => {
                depends_on = members
                    .iter()
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
            }
            ("blocks", MetaValue::Set(members)) => {
                blocks = members
                    .iter()
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
            }
            ("tags", MetaValue::Set(members)) => tags = members,
            ("comments", MetaValue::List(entries)) => comments = decode_comments(entries),
            (field, MetaValue::String(s)) if field.starts_with("meta:") => {
                let key = field.trim_start_matches("meta:");
                if !key.is_empty() {
                    meta.insert(key.to_string(), s);
                }
            }
            ("created-at", MetaValue::String(s)) => {
                created_at = OffsetDateTime::parse(&s, &Rfc3339).ok();
            }
            ("created-by", MetaValue::String(s)) => created_by = s,
            _ => {}
        }
    }

    let title = title?;
    let created_at = created_at.unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let state = state.or(legacy_state).unwrap_or(TicketState::New);
    let status = status
        .or(legacy_status)
        .filter(|status| *status == state.status())
        .unwrap_or_else(|| state.status());

    Some(Ticket {
        id,
        title,
        description,
        spec,
        status,
        state,
        assigned,
        closed_by,
        priority,
        points,
        milestone,
        code,
        parent,
        children,
        depends_on,
        blocks,
        tags,
        meta,
        comments,
        created_at,
        created_by,
        deleted: None,
    })
}

/// Project a ticket's stored fields and op log into a [`Ticket`]: scalar
/// fields from [`replay`] (via [`project_scalar_fields`]) and the Deletion from
/// [`project_deletion`]. `None` when the fields don't form a ticket.
fn project_ticket(id: Uuid, fields: Vec<(String, MetaValue)>, ops: &[Op]) -> Option<Ticket> {
    let mut ticket = build_ticket(id, project_scalar_fields(fields, ops))?;
    ticket.deleted = project_deletion(ops).map(|op| Deletion {
        at: op.created_at,
        by: op.author.clone(),
    });
    Some(ticket)
}

fn meta_string(val: Option<MetaValue>) -> Option<String> {
    match val {
        Some(MetaValue::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn meta_set(val: Option<MetaValue>) -> BTreeSet<String> {
    match val {
        Some(MetaValue::Set(set)) => set,
        Some(MetaValue::String(s)) if !s.is_empty() => BTreeSet::from([s]),
        _ => BTreeSet::new(),
    }
}

fn build_writeup(id: Uuid, fields: Vec<(String, MetaValue)>) -> Option<Writeup> {
    if fields.is_empty() {
        return None;
    }

    let mut title: Option<String> = None;
    let mut status = WriteupStatus::Open;
    let mut priority: Option<i64> = None;
    let mut created_at: Option<OffsetDateTime> = None;
    let mut created_by = String::new();
    let mut authors = BTreeSet::new();
    let mut tags = BTreeSet::new();
    let mut tickets = BTreeSet::new();
    let mut versions = Vec::new();

    for (field, value) in fields {
        match (field.as_str(), value) {
            ("title", MetaValue::String(s)) => title = Some(s),
            ("status", MetaValue::String(s)) => {
                status = WriteupStatus::parse(&s).unwrap_or(WriteupStatus::Open);
            }
            ("priority", MetaValue::String(s)) => priority = s.parse().ok(),
            ("created-at", MetaValue::String(s)) => {
                created_at = OffsetDateTime::parse(&s, &Rfc3339).ok();
            }
            ("created-by", MetaValue::String(s)) => created_by = s,
            ("authors", MetaValue::Set(members)) => authors = members,
            ("tags", MetaValue::Set(members)) => tags = members,
            ("tickets", MetaValue::Set(members)) => {
                tickets = members
                    .iter()
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
            }
            ("versions", MetaValue::List(entries)) => versions = decode_writeup_versions(entries),
            _ => {}
        }
    }

    let title = title?;
    let created_at = created_at.unwrap_or(OffsetDateTime::UNIX_EPOCH);
    if created_by.is_empty() {
        created_by = authors.iter().next().cloned().unwrap_or_default();
    }

    Some(Writeup {
        id,
        title,
        status,
        priority,
        created_at,
        created_by,
        authors,
        tags,
        tickets,
        versions,
    })
}

fn decode_comments(entries: Vec<ListEntry>) -> Vec<Comment> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(entry.timestamp) * 1_000_000)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

        let (author, body) = match serde_json::from_str::<CommentBody>(&entry.value) {
            Ok(c) => (c.author, c.body),
            // Tolerate raw-string bodies (older or hand-pushed entries).
            Err(_) => (String::from("unknown"), entry.value),
        };

        out.push(Comment { author, at, body });
    }
    out
}

fn decode_writeup_versions(entries: Vec<ListEntry>) -> Vec<WriteupVersion> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let fallback_at =
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(entry.timestamp) * 1_000_000)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        out.push(decode_writeup_version(&entry.value, fallback_at));
    }
    out
}

fn decode_writeup_version(raw: &str, fallback_at: OffsetDateTime) -> WriteupVersion {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return WriteupVersion {
            author: "unknown".to_string(),
            at: fallback_at,
            body: raw.to_string(),
        };
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        return WriteupVersion {
            author: "unknown".to_string(),
            at: fallback_at,
            body: raw.to_string(),
        };
    };

    let mut author = "unknown".to_string();
    let mut at = fallback_at;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("author:") {
            let value = value.trim();
            if !value.is_empty() {
                author = value.to_string();
            }
        } else if let Some(value) = line.strip_prefix("date:") {
            if let Ok(parsed) = OffsetDateTime::parse(value.trim(), &Rfc3339) {
                at = parsed;
            }
        }
    }

    WriteupVersion {
        author,
        at,
        body: body.trim_start_matches(['\r', '\n']).to_string(),
    }
}

fn metadata_priority_sort_key(priority: Option<i64>) -> (u8, i64) {
    match priority {
        Some(value) => (0, value),
        None => (1, 0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_store;
    use crate::FakeVcsAdapter;

    #[test]
    fn create_and_load_round_trips() {
        let (store, _td) = test_store();
        let opts = NewTicketOpts {
            comment: Some("first comment".into()),
            tags: vec!["bug".into(), "ui".into()],
            assigned: Some("scott@example.com".into()),
            ..Default::default()
        };
        let created = store.create("My new ticket", opts).unwrap();
        assert_eq!(created.title, "My new ticket");
        assert_eq!(created.status, TicketStatus::Open);
        assert_eq!(created.state, TicketState::New);
        assert_eq!(created.assigned.as_deref(), Some("scott@example.com"));
        assert!(created.tags.contains("bug"));
        assert!(created.tags.contains("ui"));
        assert_eq!(created.comments.len(), 1);
        assert_eq!(created.comments[0].body, "first comment");

        let again = store.load(&created.id).unwrap();
        assert_eq!(created, again);
    }

    #[test]
    fn failed_create_leaves_no_residue() {
        let (store, _td) = test_store();
        // An unknown assignee nick makes `create` fail during validation.
        let opts = NewTicketOpts {
            assigned: Some("zzz-no-such-nick".into()),
            ..Default::default()
        };
        assert!(store.create("ghosty", opts).is_err());

        // The failure must leave nothing behind — no op-less `format-version`
        // shell, no partial ticket keys at all.
        let p = store.session().target(&Target::project());
        let residue = p.get_all_values(Some(&keys::tickets_prefix())).unwrap();
        assert!(
            residue.is_empty(),
            "failed create left residue: {residue:?}"
        );
        assert_eq!(store.list().unwrap().len(), 0);

        // And the store is still healthy for a subsequent good create.
        store.create("real", NewTicketOpts::default()).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn list_returns_all_created_tickets() {
        let (store, _td) = test_store();
        store.create("first", NewTicketOpts::default()).unwrap();
        store.create("second", NewTicketOpts::default()).unwrap();
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
        let titles: BTreeSet<_> = all.iter().map(|t| t.title.clone()).collect();
        assert!(titles.contains("first"));
        assert!(titles.contains("second"));
    }

    #[test]
    fn list_is_empty_for_fresh_repo() {
        let (store, _td) = test_store();
        assert!(store.list().unwrap().is_empty());
    }

    fn one_field(field: &str, value: &str) -> OpKind {
        let mut fields = BTreeMap::new();
        fields.insert(field.to_string(), value.to_string());
        OpKind::SetField { fields }
    }

    #[test]
    fn append_and_load_ops_round_trips_and_replays() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store
            .append_op(&t.id, one_field("title", "renamed"))
            .unwrap();
        store.append_op(&t.id, one_field("priority", "2")).unwrap();

        let ops = store.load_ops(&t.id).unwrap();
        // create op (from `create`) + the two appended ops.
        assert_eq!(ops.len(), 3);
        let state = crate::oplog::replay(&ops);
        assert_eq!(state.get("title").map(String::as_str), Some("renamed"));
        assert_eq!(state.get("priority").map(String::as_str), Some("2"));
    }

    #[test]
    fn append_op_increments_lamport_from_scan() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        // `create` already wrote a create op at lamport 0.
        assert_eq!(store.next_lamport(&t.id).unwrap(), 1);
        let op0 = store.append_op(&t.id, one_field("a", "1")).unwrap();
        assert_eq!(op0.lamport, 1);
        assert_eq!(store.next_lamport(&t.id).unwrap(), 2);
        let op1 = store.append_op(&t.id, one_field("b", "2")).unwrap();
        assert_eq!(op1.lamport, 2);
    }

    #[test]
    fn append_op_migrates_plain_field_ticket_first() {
        let (store, _td) = test_store();
        let id = Uuid::new_v4();
        let p = store.session().target(&Target::project());
        p.set(&keys::ticket_field(&id, "title"), "plain").unwrap();
        p.set(&keys::ticket_field(&id, "status"), "open").unwrap();
        p.set(&keys::ticket_field(&id, "state"), "new").unwrap();
        p.set(&keys::ticket_field(&id, "created-by"), "old@example.com")
            .unwrap();
        p.set(
            &keys::ticket_field(&id, "created-at"),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();

        let op = store.append_op(&id, one_field("priority", "2")).unwrap();
        assert_eq!(op.lamport, 1);
        let t = store.load(&id).unwrap();
        assert_eq!(t.title, "plain");
        assert_eq!(t.priority, Some(2));
    }

    #[test]
    fn signed_ops_verify_clean_with_published_identity() {
        let (mut store, _td) = test_store();
        let keydir = tempfile::tempdir().unwrap();
        let key = keydir.path().join("id_ed25519");
        let status = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "signer", "-q", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen");
        assert!(status.success());
        store.set_signing_key(Some(key));

        let t = store.create("signed", NewTicketOpts::default()).unwrap();
        store.set_priority(&t.id, Some(1)).unwrap();

        // Every op is signed.
        let ops = store.load_ops(&t.id).unwrap();
        assert!(ops
            .iter()
            .all(|o| o.signature.is_some() && o.signer.is_some()));

        // The identity key was published.
        let p = store.session().target(&Target::project());
        assert!(p
            .get_value(&keys::identity(store.email()))
            .unwrap()
            .is_some());

        // verify is clean: valid signatures, no unsigned warnings.
        let report = store.verify().unwrap();
        let outcome = report.iter().find(|o| o.id == t.id).unwrap();
        assert!(outcome.ok, "signed ops must verify: {:?}", outcome.issues);
        assert!(
            outcome.warnings.is_empty(),
            "no unsigned warnings expected: {:?}",
            outcome.warnings
        );
    }

    #[test]
    fn unsigned_ops_verify_ok_but_warn() {
        let (store, _td) = test_store();
        let t = store.create("unsigned", NewTicketOpts::default()).unwrap();
        let report = store.verify().unwrap();
        let outcome = report.iter().find(|o| o.id == t.id).unwrap();
        assert!(outcome.ok, "unsigned ops are trusted by default");
        assert!(
            !outcome.warnings.is_empty(),
            "unsigned ops should be flagged as warnings"
        );
    }

    #[test]
    fn verify_passes_healthy_and_flags_tampered_ops() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.set_priority(&t.id, Some(2)).unwrap();

        let report = store.verify().unwrap();
        assert!(
            report.iter().all(|o| o.ok),
            "healthy repo must verify clean: {report:?}"
        );

        // Tamper: overwrite an op key's value so its envelope id no longer
        // matches the key hash nor its own recomputed content-id.
        let p = store.session().target(&Target::project());
        let pairs = p
            .get_all_values(Some(&keys::ticket_ops_prefix(&t.id)))
            .unwrap();
        let (key, _) = pairs
            .into_iter()
            .find(|(k, _)| keys::parse_ticket_op(k).is_some())
            .unwrap();
        p.set(
            &key,
            "{\"id\":\"deadbeef\",\"lamport\":0,\"author\":\"x@y.z\",\
             \"created_at\":\"1970-01-01T00:00:00Z\",\"format_version\":1,\
             \"kind\":\"set-field\",\"payload\":{\"fields\":{\"title\":\"evil\"}}}",
        )
        .unwrap();

        let report = store.verify().unwrap();
        let outcome = report.iter().find(|o| o.id == t.id).unwrap();
        assert!(!outcome.ok, "tampered op must be flagged");
        assert!(!outcome.issues.is_empty());
    }

    #[test]
    fn load_ignores_unknown_fields() {
        // Forward-compat contract (roadmap step 3): a reader must tolerate
        // fields a newer client added that it does not understand — it ignores
        // them and keeps the known fields intact, never erroring. The op-log
        // envelope extends this same guarantee to unknown op *kinds* in step 4.
        let (store, _td) = test_store();
        let t = store.create("keep me", NewTicketOpts::default()).unwrap();
        let p = store.session().target(&Target::project());
        p.set(&keys::ticket_field(&t.id, "future-widget"), "42")
            .unwrap();
        let loaded = store.load(&t.id).unwrap();
        assert_eq!(loaded.title, "keep me");
        assert_eq!(loaded.state, TicketState::New);
    }

    #[test]
    fn create_stamps_current_format_version() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        let p = store.session().target(&Target::project());
        match p
            .get_value(&keys::ticket_field(&t.id, keys::FORMAT_VERSION_FIELD))
            .unwrap()
        {
            Some(MetaValue::String(s)) => assert_eq!(s, "2"),
            other => panic!("expected format-version string, got {other:?}"),
        }
    }

    #[test]
    fn load_rejects_future_format_version() {
        let (store, _td) = test_store();
        let id = Uuid::new_v4();
        let p = store.session().target(&Target::project());
        p.set(&keys::ticket_field(&id, "title"), "future").unwrap();
        p.set(&keys::ticket_field(&id, "status"), "open").unwrap();
        p.set(&keys::ticket_field(&id, "state"), "new").unwrap();
        p.set(&keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD), "999")
            .unwrap();
        let err = store.load(&id).unwrap_err();
        assert!(
            matches!(err, Error::FormatTooNew { .. }),
            "expected FormatTooNew, got {err:?}"
        );
    }

    #[test]
    fn migrate_converts_legacy_ticket_to_op_and_is_idempotent() {
        let (store, _td) = test_store();
        // A legacy ticket written as raw scalar keys, no ops, no version marker.
        let id = Uuid::new_v4();
        let p = store.session().target(&Target::project());
        p.set(&keys::ticket_field(&id, "title"), "legacy").unwrap();
        p.set(&keys::ticket_field(&id, "status"), "open").unwrap();
        p.set(&keys::ticket_field(&id, "state"), "new").unwrap();
        p.set(&keys::ticket_field(&id, "priority"), "3").unwrap();
        p.set(&keys::ticket_field(&id, "created-by"), "old@example.com")
            .unwrap();
        p.set(
            &keys::ticket_field(&id, "created-at"),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();

        // Dry-run reports the conversion but writes nothing.
        let plan = store.migrate(false).unwrap();
        let outcome = plan.iter().find(|o| o.id == id).expect("ticket in plan");
        assert_eq!(outcome.from, None);
        assert_eq!(outcome.to, 2);
        assert!(outcome.changed);
        assert!(store.load_ops(&id).unwrap().is_empty());

        // Write converts: a create op appears, legacy scalar keys are gone,
        // format-version is stamped, and the ticket still reads identically.
        store.migrate(true).unwrap();
        assert_eq!(store.load_ops(&id).unwrap().len(), 1);
        assert!(p
            .get_value(&keys::ticket_field(&id, "title"))
            .unwrap()
            .is_none());
        let t = store.load(&id).unwrap();
        assert_eq!(t.title, "legacy");
        assert_eq!(t.priority, Some(3));
        assert_eq!(t.created_by, "old@example.com");
        match p
            .get_value(&keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD))
            .unwrap()
        {
            Some(MetaValue::String(s)) => assert_eq!(s, "2"),
            other => panic!("expected stamped version, got {other:?}"),
        }

        // Idempotent: the ticket now has ops, so a second run is a no-op.
        let plan = store.migrate(true).unwrap();
        assert!(!plan.iter().find(|o| o.id == id).unwrap().changed);
        assert_eq!(store.load_ops(&id).unwrap().len(), 1);
    }

    #[test]
    fn history_and_closed_at_come_from_the_op_log() {
        let (store, _td) = test_store();
        let t = store.create("hist", NewTicketOpts::default()).unwrap();
        assert_eq!(store.closed_at(&t.id).unwrap(), None);
        store.set_state(&t.id, TicketState::InProgress).unwrap();
        store.add_comment(&t.id, "working").unwrap();
        let before = OffsetDateTime::now_utc();
        store.set_state(&t.id, TicketState::Resolved).unwrap();

        let history = store.history(&t.id).unwrap();
        let has = |field: &str, value: &str| {
            history
                .iter()
                .any(|e| e.field == field && e.value.as_deref() == Some(value))
        };
        assert!(has("title", "hist"));
        assert!(has("state", "in-progress"));
        assert!(has("status", "closed"));
        assert!(has("state", "resolved"));
        assert!(has("closed-by", store.email()));
        assert!(has("comments", "working"));
        assert!(
            history.windows(2).all(|w| w[0].at >= w[1].at),
            "newest first"
        );

        let closed = store.closed_at(&t.id).unwrap().expect("closed time");
        assert!(closed >= before - time::Duration::seconds(1));
        assert_eq!(store.closed_times().unwrap().get(&t.id), Some(&closed));

        store.set_state(&t.id, TicketState::New).unwrap();
        assert_eq!(store.closed_at(&t.id).unwrap(), None);
    }

    #[test]
    fn closed_at_reflects_close_time_not_created_time() {
        let (store, _td) = test_store();
        let t = store.create("old", NewTicketOpts::default()).unwrap();
        let long_ago = (OffsetDateTime::now_utc() - time::Duration::days(60))
            .format(&Rfc3339)
            .unwrap();
        store.set_meta_scalar_for_test(&t.id, "created-at", &long_ago);
        store.set_state(&t.id, TicketState::Resolved).unwrap();

        let loaded = store.load(&t.id).unwrap();
        assert!(loaded.created_at < OffsetDateTime::now_utc() - time::Duration::days(30));
        let closed = store.closed_at(&t.id).unwrap().expect("closed time");
        assert!(closed > OffsetDateTime::now_utc() - time::Duration::days(7));
    }

    #[test]
    fn state_change_persists() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.set_state(&t.id, TicketState::Resolved).unwrap();
        let loaded = store.load(&t.id).unwrap();
        assert_eq!(loaded.status, TicketStatus::Closed);
        assert_eq!(loaded.state, TicketState::Resolved);
        assert_eq!(loaded.closed_by.as_deref(), Some(store.email()));
        store.set_state(&t.id, TicketState::New).unwrap();
        assert_eq!(store.load(&t.id).unwrap().closed_by, None);
    }

    #[test]
    fn lifecycle_change_persists_status_and_state() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store
            .set_lifecycle(&t.id, TicketStatus::Open, TicketState::Blocked)
            .unwrap();
        let loaded = store.load(&t.id).unwrap();
        assert_eq!(loaded.status, TicketStatus::Open);
        assert_eq!(loaded.state, TicketState::Blocked);
    }

    /// Create a child ticket parented to `parent`.
    fn child_of(store: &TicketStore, parent: &Uuid, title: &str) -> Ticket {
        store
            .create(
                title,
                NewTicketOpts {
                    parent: Some(*parent),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn close_rejected_while_a_subissue_is_open() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = child_of(&store, &parent.id, "child");

        let err = store
            .set_lifecycle(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap_err();
        assert!(matches!(err, Error::OpenSubissues(_)));

        let open = store.open_children(&parent.id).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, child.id);
        // Parent stayed open.
        assert_eq!(store.load(&parent.id).unwrap().status, TicketStatus::Open);
    }

    #[test]
    fn close_allowed_once_all_subissues_are_closed() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = child_of(&store, &parent.id, "child");

        store.set_state(&child.id, TicketState::Resolved).unwrap();
        store
            .set_lifecycle(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(store.load(&parent.id).unwrap().status, TicketStatus::Closed);
    }

    #[test]
    fn subissue_closed_as_wontfix_counts_as_solved() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = child_of(&store, &parent.id, "child");

        // A child dropped as wontfix is "dealt with" — it must not block.
        store.set_state(&child.id, TicketState::Wontfix).unwrap();
        assert!(store.open_children(&parent.id).unwrap().is_empty());
        store
            .set_lifecycle(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(store.load(&parent.id).unwrap().status, TicketStatus::Closed);
    }

    #[test]
    fn forced_close_bypasses_open_subissue_guard() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        child_of(&store, &parent.id, "child");

        store
            .set_lifecycle_forced(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(store.load(&parent.id).unwrap().status, TicketStatus::Closed);
    }

    #[test]
    fn close_with_no_children_is_unaffected() {
        let (store, _td) = test_store();
        let t = store.create("solo", NewTicketOpts::default()).unwrap();
        store
            .set_lifecycle(&t.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(store.load(&t.id).unwrap().status, TicketStatus::Closed);
    }

    #[test]
    fn close_rejected_while_a_dependency_is_open() {
        let (store, _td) = test_store();
        let dependent = store.create("dependent", NewTicketOpts::default()).unwrap();
        let blocker = store.create("blocker", NewTicketOpts::default()).unwrap();
        store.add_dependency(&dependent.id, &blocker.id).unwrap();

        // Can't resolve the dependent while the thing it waits on is still open.
        let err = store
            .set_lifecycle(&dependent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap_err();
        assert!(matches!(err, Error::OpenDependencies(_)));
        assert_eq!(store.open_dependencies(&dependent.id).unwrap().len(), 1);
    }

    #[test]
    fn closing_a_blocker_with_open_dependents_is_allowed() {
        let (store, _td) = test_store();
        let dependent = store.create("dependent", NewTicketOpts::default()).unwrap();
        let blocker = store.create("blocker", NewTicketOpts::default()).unwrap();
        store.add_dependency(&dependent.id, &blocker.id).unwrap();

        // Finishing the blocker is the normal case, even with an open dependent.
        store
            .set_lifecycle(&blocker.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(
            store.load(&blocker.id).unwrap().status,
            TicketStatus::Closed
        );
    }

    #[test]
    fn close_allowed_once_dependency_resolved_or_forced() {
        let (store, _td) = test_store();
        let dependent = store.create("dependent", NewTicketOpts::default()).unwrap();
        let blocker = store.create("blocker", NewTicketOpts::default()).unwrap();
        store.add_dependency(&dependent.id, &blocker.id).unwrap();

        // --force bypasses the guard.
        store
            .set_lifecycle_forced(&dependent.id, TicketStatus::Open, TicketState::New)
            .unwrap();
        store
            .set_lifecycle_forced(&dependent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(
            store.load(&dependent.id).unwrap().status,
            TicketStatus::Closed
        );

        // Reopen, resolve the blocker, then a plain close succeeds.
        store
            .set_lifecycle_forced(&dependent.id, TicketStatus::Open, TicketState::New)
            .unwrap();
        store.set_state(&blocker.id, TicketState::Resolved).unwrap();
        store
            .set_lifecycle(&dependent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert_eq!(
            store.load(&dependent.id).unwrap().status,
            TicketStatus::Closed
        );
    }

    #[test]
    fn create_subissue_under_closed_parent_is_rejected() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        store.set_state(&parent.id, TicketState::Resolved).unwrap();

        let err = store
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(parent.id),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::InvalidValue(_)));
        // No half-written ticket leaked: only the parent exists.
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn set_parent_rejects_open_child_under_closed_parent() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = store.create("child", NewTicketOpts::default()).unwrap();
        store.set_state(&parent.id, TicketState::Resolved).unwrap();

        assert!(store.set_parent(&child.id, &parent.id).is_err());
    }

    #[test]
    fn reclassifying_a_closed_parent_is_not_blocked_by_open_child() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = child_of(&store, &parent.id, "child");

        // Force the parent closed while a child is still open (legacy-style state).
        store
            .set_lifecycle_forced(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        assert!(!store.open_children(&parent.id).unwrap().is_empty());

        // Closed→closed reclassification must not re-trigger the guard.
        store
            .set_lifecycle(&parent.id, TicketStatus::Closed, TicketState::Wontfix)
            .unwrap();
        assert_eq!(store.load(&parent.id).unwrap().state, TicketState::Wontfix);
        // Child untouched.
        assert_eq!(store.load(&child.id).unwrap().status, TicketStatus::Open);
    }

    #[test]
    fn tag_add_and_remove() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.add_tag(&t.id, "feature").unwrap();
        store.add_tag(&t.id, "ui").unwrap();
        assert_eq!(
            store.load(&t.id).unwrap().tags,
            ["feature", "ui"].iter().map(|s| s.to_string()).collect()
        );
        store.remove_tag(&t.id, "ui").unwrap();
        assert_eq!(
            store.load(&t.id).unwrap().tags,
            ["feature"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn assigned_set_and_clear() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.set_assigned(&t.id, Some("a@b.co")).unwrap();
        assert_eq!(
            store.load(&t.id).unwrap().assigned.as_deref(),
            Some("a@b.co")
        );
        store.set_assigned(&t.id, None).unwrap();
        assert_eq!(store.load(&t.id).unwrap().assigned, None);
    }

    #[test]
    fn points_set_and_clear() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.set_points(&t.id, Some(5)).unwrap();
        assert_eq!(store.load(&t.id).unwrap().points, Some(5));
        store.set_points(&t.id, None).unwrap();
        assert_eq!(store.load(&t.id).unwrap().points, None);
    }

    #[test]
    fn comments_carry_author_and_arrive_in_order() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        store.add_comment(&t.id, "one").unwrap();
        store.add_comment(&t.id, "two").unwrap();
        store.add_comment(&t.id, "three").unwrap();
        let loaded = store.load(&t.id).unwrap();
        let bodies: Vec<_> = loaded.comments.iter().map(|c| c.body.clone()).collect();
        assert_eq!(bodies, vec!["one", "two", "three"]);
        for c in &loaded.comments {
            assert_eq!(c.author, store.email());
        }
    }

    #[test]
    fn resolve_id_accepts_unique_prefix() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        let prefix: String = t.id.to_string().chars().take(6).collect();
        assert_eq!(store.resolve_id(&prefix).unwrap(), t.id);
        // Hyphens optional & case-insensitive.
        let no_hyphen: String = t.id.to_string().replace('-', "").to_ascii_uppercase();
        assert_eq!(store.resolve_id(&no_hyphen).unwrap(), t.id);
    }

    #[test]
    fn resolve_id_accepts_prefix_unique_among_open_tickets() {
        let (store, _td) = test_store();
        let open = Uuid::parse_str("d7f2d8f6-d6ec-3da1-a180-0a33fb090d59").unwrap();
        let closed = Uuid::parse_str("d7f99999-d6ec-3da1-a180-0a33fb090d59").unwrap();
        insert_ticket(&store, open, "open", TicketStatus::Open, TicketState::New);
        insert_ticket(
            &store,
            closed,
            "closed",
            TicketStatus::Closed,
            TicketState::Resolved,
        );

        assert_eq!(store.resolve_id("d7f").unwrap(), open);
    }

    #[test]
    fn resolve_id_reports_no_match() {
        let (store, _td) = test_store();
        store.create("x", NewTicketOpts::default()).unwrap();
        let err = store.resolve_id("ffffffff").unwrap_err();
        assert!(matches!(err, Error::NoMatch(_)));
    }

    #[test]
    fn views_round_trip() {
        let (store, _td) = test_store();
        let a = store.create("a", NewTicketOpts::default()).unwrap();
        let b = store.create("b", NewTicketOpts::default()).unwrap();
        let mut snapshot = BTreeSet::new();
        snapshot.insert(a.id);
        snapshot.insert(b.id);
        store.save_view("everything", &snapshot).unwrap();
        assert_eq!(store.load_view("everything").unwrap(), snapshot);
        assert_eq!(store.list_views().unwrap(), vec!["everything".to_string()]);

        // Saving again with a smaller set replaces, not unions.
        let mut just_a = BTreeSet::new();
        just_a.insert(a.id);
        store.save_view("everything", &just_a).unwrap();
        assert_eq!(store.load_view("everything").unwrap(), just_a);
    }

    #[test]
    fn writeups_round_trip_versions_and_status() {
        let (store, _td) = test_store();
        let writeup = store
            .create_writeup(
                "Design note",
                NewWriteupOpts {
                    body: Some("first draft".to_string()),
                    tags: vec!["design".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .append_writeup_version(&writeup.id, "second draft")
            .unwrap();
        store
            .set_writeup_status(&writeup.id, WriteupStatus::Closed)
            .unwrap();
        store.set_writeup_priority(&writeup.id, Some(2)).unwrap();
        store.add_writeup_tag(&writeup.id, "review").unwrap();
        store.remove_writeup_tag(&writeup.id, "design").unwrap();

        let loaded = store.load_writeup(&writeup.id).unwrap();
        assert_eq!(loaded.title, "Design note");
        assert_eq!(loaded.status, WriteupStatus::Closed);
        assert_eq!(loaded.priority, Some(2));
        assert!(!loaded.tags.contains("design"));
        assert!(loaded.tags.contains("review"));
        assert!(loaded.authors.contains(store.email()));
        assert_eq!(loaded.versions.len(), 2);
        assert_eq!(loaded.versions[0].author, store.email());
        assert_eq!(loaded.versions[0].body, "first draft");
        assert_eq!(loaded.versions[1].body, "second draft");
        assert_eq!(
            store.resolve_writeup_id(&writeup.short_id()).unwrap(),
            writeup.id
        );
    }

    #[test]
    fn writeups_sort_by_priority_before_recency() {
        let (store, _td) = test_store();
        let old_high = store
            .create_writeup(
                "old high",
                NewWriteupOpts {
                    created_at: Some(
                        OffsetDateTime::from_unix_timestamp(1_000).expect("valid timestamp"),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        let recent_low = store
            .create_writeup(
                "recent low",
                NewWriteupOpts {
                    created_at: Some(
                        OffsetDateTime::from_unix_timestamp(2_000).expect("valid timestamp"),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        let no_priority = store
            .create_writeup(
                "no priority",
                NewWriteupOpts {
                    created_at: Some(
                        OffsetDateTime::from_unix_timestamp(3_000).expect("valid timestamp"),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        store.set_writeup_priority(&old_high.id, Some(1)).unwrap();
        store.set_writeup_priority(&recent_low.id, Some(5)).unwrap();

        let writeups = store.list_writeups().unwrap();

        assert_eq!(
            writeups
                .iter()
                .map(|writeup| writeup.id)
                .collect::<Vec<_>>(),
            vec![old_high.id, recent_low.id, no_priority.id]
        );
    }

    #[test]
    fn writeups_link_unlink_and_promote() {
        let (store, _td) = test_store();
        let ticket = store.create("existing", NewTicketOpts::default()).unwrap();
        let writeup = store
            .create_writeup(
                "Promotable",
                NewWriteupOpts {
                    body: Some("make this actionable".to_string()),
                    tags: vec!["feature".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
        store.set_writeup_priority(&writeup.id, Some(3)).unwrap();

        store.link_writeup_ticket(&writeup.id, &ticket.id).unwrap();
        assert!(store
            .load_writeup(&writeup.id)
            .unwrap()
            .tickets
            .contains(&ticket.id));
        // The link is stored only on the writeup; no ticket-side reverse index.
        assert!(store
            .project_handle()
            .get_value(&keys::ticket_field(&ticket.id, "writeups"))
            .unwrap()
            .is_none());
        store
            .unlink_writeup_ticket(&writeup.id, &ticket.id)
            .unwrap();
        assert!(!store
            .load_writeup(&writeup.id)
            .unwrap()
            .tickets
            .contains(&ticket.id));

        let promoted = store.promote_writeup(&writeup.id).unwrap();
        assert_eq!(promoted.title, "Promotable");
        assert_eq!(
            promoted.description.as_deref(),
            Some("make this actionable")
        );
        assert_eq!(promoted.priority, Some(3));
        assert!(promoted.tags.contains("feature"));
        assert!(store
            .load_writeup(&writeup.id)
            .unwrap()
            .tickets
            .contains(&promoted.id));
    }

    fn insert_ticket(
        store: &TicketStore,
        id: Uuid,
        title: &str,
        status: TicketStatus,
        state: TicketState,
    ) {
        let p = store.project_handle();
        let created = OffsetDateTime::UNIX_EPOCH.format(&Rfc3339).unwrap();
        p.set(&keys::ticket_field(&id, "title"), title).unwrap();
        p.set(&keys::ticket_field(&id, "status"), status.as_str())
            .unwrap();
        p.set(&keys::ticket_field(&id, "state"), state.as_str())
            .unwrap();
        p.set(&keys::ticket_field(&id, "created-at"), created.as_str())
            .unwrap();
        p.set(&keys::ticket_field(&id, "created-by"), store.email())
            .unwrap();
    }

    #[test]
    fn owners_round_trip() {
        let (store, _td) = test_store();
        store.add_owner("alice@example.com").unwrap();
        store.add_owner("bob@example.com").unwrap();
        let owners = store.list_owners().unwrap();
        assert!(owners.contains("alice@example.com"));
        assert!(owners.contains("bob@example.com"));
        store.remove_owner("alice@example.com").unwrap();
        let owners = store.list_owners().unwrap();
        assert!(!owners.contains("alice@example.com"));
        assert!(owners.contains("bob@example.com"));
    }

    #[test]
    fn schema_version_is_seeded_on_open() {
        let (store, _td) = test_store();
        assert_eq!(
            store.schema_version().unwrap().as_deref(),
            Some(keys::SCHEMA_VERSION),
        );
    }

    #[test]
    fn parent_child_round_trips() {
        let (store, _td) = test_store();
        let parent = store.create("epic", NewTicketOpts::default()).unwrap();
        let child = store
            .create(
                "sub-task",
                NewTicketOpts {
                    parent: Some(parent.id),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(child.parent, Some(parent.id));
        let parent = store.load(&parent.id).unwrap();
        assert!(parent.children.contains(&child.id));
    }

    #[test]
    fn set_parent_and_clear_parent() {
        let (store, _td) = test_store();
        let epic = store.create("epic", NewTicketOpts::default()).unwrap();
        let task = store.create("task", NewTicketOpts::default()).unwrap();

        // Set parent
        store.set_parent(&task.id, &epic.id).unwrap();
        let task = store.load(&task.id).unwrap();
        assert_eq!(task.parent, Some(epic.id));
        let epic = store.load(&epic.id).unwrap();
        assert!(epic.children.contains(&task.id));

        // Clear parent
        store.clear_parent(&task.id).unwrap();
        let task = store.load(&task.id).unwrap();
        assert_eq!(task.parent, None);
        let epic = store.load(&epic.id).unwrap();
        assert!(!epic.children.contains(&task.id));
    }

    #[test]
    fn set_parent_rejects_self_reference() {
        let (store, _td) = test_store();
        let t = store.create("x", NewTicketOpts::default()).unwrap();
        assert!(store.set_parent(&t.id, &t.id).is_err());
    }

    #[test]
    fn set_parent_rejects_circular_chain() {
        let (store, _td) = test_store();
        let a = store.create("a", NewTicketOpts::default()).unwrap();
        let b = store.create("b", NewTicketOpts::default()).unwrap();
        let c = store.create("c", NewTicketOpts::default()).unwrap();

        store.set_parent(&b.id, &a.id).unwrap();
        store.set_parent(&c.id, &b.id).unwrap();
        // a -> b -> c, now trying c -> a would be circular
        assert!(store.set_parent(&a.id, &c.id).is_err());
    }

    #[test]
    fn reparent_moves_child_between_parents() {
        let (store, _td) = test_store();
        let p1 = store.create("parent1", NewTicketOpts::default()).unwrap();
        let p2 = store.create("parent2", NewTicketOpts::default()).unwrap();
        let child = store
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(p1.id),
                    ..Default::default()
                },
            )
            .unwrap();

        // Move child from p1 to p2
        store.set_parent(&child.id, &p2.id).unwrap();
        let p1 = store.load(&p1.id).unwrap();
        let p2 = store.load(&p2.id).unwrap();
        assert!(!p1.children.contains(&child.id));
        assert!(p2.children.contains(&child.id));
    }

    #[test]
    fn delete_ticket_is_soft_and_leaves_relations_on_both_sides() {
        let (store, _td) = test_store();
        let parent = store.create("parent", NewTicketOpts::default()).unwrap();
        let child = store
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(parent.id),
                    ..Default::default()
                },
            )
            .unwrap();
        let dep = store.create("dep", NewTicketOpts::default()).unwrap();
        store.add_dependency(&child.id, &dep.id).unwrap();
        let parent_before = store.load(&parent.id).unwrap();
        let dep_before = store.load(&dep.id).unwrap();

        store.delete_ticket(&child.id, Some("dup")).unwrap();

        let deleted = store.load(&child.id).unwrap();
        assert!(deleted.is_deleted());
        assert_eq!(deleted.deleted.as_ref().unwrap().by, "tester@example.com");
        assert_eq!(deleted.parent, Some(parent.id));
        assert!(deleted.depends_on.contains(&dep.id));
        assert_eq!(store.load(&parent.id).unwrap(), parent_before);
        assert_eq!(store.load(&dep.id).unwrap(), dep_before);
        assert!(store.list().unwrap().iter().any(|t| t.id == child.id));
    }

    #[test]
    fn undelete_ticket_brings_it_back_and_both_refuse_a_no_op() {
        let (store, _td) = test_store();
        let t = store.create("t", NewTicketOpts::default()).unwrap();
        assert!(matches!(
            store.undelete_ticket(&t.id),
            Err(Error::NotDeleted(_))
        ));
        store.delete_ticket(&t.id, None).unwrap();
        assert!(matches!(
            store.delete_ticket(&t.id, None),
            Err(Error::AlreadyDeleted(_))
        ));
        store.undelete_ticket(&t.id).unwrap();
        assert!(!store.load(&t.id).unwrap().is_deleted());
        assert_verifies(&store);
    }

    #[test]
    fn add_dependency_rejects_self_dependency() {
        let (store, _td) = test_store();
        let t = store.create("t", NewTicketOpts::default()).unwrap();
        assert!(store.add_dependency(&t.id, &t.id).is_err());
    }

    #[test]
    fn add_dependency_rejects_cycle() {
        let (store, _td) = test_store();
        let a = store.create("a", NewTicketOpts::default()).unwrap();
        let b = store.create("b", NewTicketOpts::default()).unwrap();
        let c = store.create("c", NewTicketOpts::default()).unwrap();

        // a -> b -> c; closing the loop c -> a must be rejected.
        store.add_dependency(&a.id, &b.id).unwrap();
        store.add_dependency(&b.id, &c.id).unwrap();
        assert!(store.add_dependency(&c.id, &a.id).is_err());
    }

    #[test]
    fn remove_dependency_clears_both_sides() {
        let (store, _td) = test_store();
        let a = store.create("a", NewTicketOpts::default()).unwrap();
        let b = store.create("b", NewTicketOpts::default()).unwrap();

        store.add_dependency(&a.id, &b.id).unwrap();
        assert!(store.load(&a.id).unwrap().depends_on.contains(&b.id));
        assert!(store.load(&b.id).unwrap().blocks.contains(&a.id));

        store.remove_dependency(&a.id, &b.id).unwrap();
        assert!(store.load(&a.id).unwrap().depends_on.is_empty());
        assert!(store.load(&b.id).unwrap().blocks.is_empty());
    }

    /// Merge the fork's current state of ticket `id` into `local`, the way
    /// `ti pull <url>` does.
    fn fork_merge(local: &TicketStore, fork: &TicketStore, id: &Uuid) -> ForkMerge {
        local
            .merge_fork_ticket(&fork.load(id).unwrap(), &fork.load_ops(id).unwrap())
            .unwrap()
    }

    fn assert_verifies(store: &TicketStore) {
        for outcome in store.verify().unwrap() {
            assert!(outcome.ok, "{}: {:?}", outcome.short_id, outcome.issues);
        }
    }

    #[test]
    fn fork_import_emits_a_create_op_and_verifies() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let parent = fork.create("parent", NewTicketOpts::default()).unwrap();
        let child = fork
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(parent.id),
                    comment: Some("hello".into()),
                    tags: vec!["bug".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        fork.set_meta(&child.id, "env", "prod").unwrap();
        let child = fork.load(&child.id).unwrap();

        let parent = fork.load(&parent.id).unwrap();
        assert_eq!(
            fork_merge(&local, &fork, &parent.id),
            ForkMerge::Imported
        );
        assert_eq!(
            fork_merge(&local, &fork, &child.id),
            ForkMerge::Imported
        );

        let imported = local.load(&child.id).unwrap();
        assert_eq!(imported.title, "child");
        assert_eq!(imported.parent, Some(parent.id));
        assert_eq!(imported.created_by, child.created_by);
        assert_eq!(imported.created_at, child.created_at);
        assert!(imported.tags.contains("bug"));
        assert_eq!(imported.meta.get("env").map(String::as_str), Some("prod"));
        assert_eq!(imported.comments.len(), 1);
        assert_eq!(imported.comments[0].author, child.comments[0].author);
        assert_eq!(local.load_ops(&child.id).unwrap().len(), 1);
        assert_eq!(local.load(&parent.id).unwrap().children, parent.children);
        assert_verifies(&local);
    }

    #[test]
    fn fork_merge_carries_deletion_and_undeletion_operations() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let gone = fork.create("deleted on the fork", NewTicketOpts::default()).unwrap();
        let kept = fork.create("live on the fork", NewTicketOpts::default()).unwrap();
        fork.delete_ticket(&gone.id, Some("dup")).unwrap();

        // A ticket the fork deleted imports deleted, not live.
        assert_eq!(fork_merge(&local, &fork, &gone.id), ForkMerge::Imported);
        assert!(local.load(&gone.id).unwrap().is_deleted());

        // A Deletion the fork makes later reaches a ticket already imported.
        fork_merge(&local, &fork, &kept.id);
        fork.delete_ticket(&kept.id, None).unwrap();
        assert_eq!(fork_merge(&local, &fork, &kept.id), ForkMerge::Updated);
        assert!(local.load(&kept.id).unwrap().is_deleted());
        assert_eq!(fork_merge(&local, &fork, &kept.id), ForkMerge::Unchanged);

        // So does the fork's Undeletion.
        fork.undelete_ticket(&gone.id).unwrap();
        assert_eq!(fork_merge(&local, &fork, &gone.id), ForkMerge::Updated);
        assert!(!local.load(&gone.id).unwrap().is_deleted());
        assert_verifies(&local);
    }

    #[test]
    fn fork_merge_keeps_a_local_deletion_the_fork_never_saw() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let t = fork.create("shared", NewTicketOpts::default()).unwrap();
        fork_merge(&local, &fork, &t.id);
        local.delete_ticket(&t.id, None).unwrap();

        fork.add_comment(&t.id, "the fork still works on it").unwrap();
        assert_eq!(fork_merge(&local, &fork, &t.id), ForkMerge::Updated);
        let after = local.load(&t.id).unwrap();
        assert!(after.is_deleted());
        assert_eq!(after.comments.len(), 1);
    }

    #[test]
    fn fork_merge_applies_parent_change_to_op_based_ticket() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let first = fork.create("first", NewTicketOpts::default()).unwrap();
        let second = fork.create("second", NewTicketOpts::default()).unwrap();
        let child = fork
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(first.id),
                    ..Default::default()
                },
            )
            .unwrap();
        for t in [&first, &second, &child] {
            fork_merge(&local, &fork, &t.id);
        }
        // A local edit so the ticket has ops of its own.
        local.set_priority(&child.id, Some(3)).unwrap();

        fork.set_parent(&child.id, &second.id).unwrap();
        let merged = fork_merge(&local, &fork, &child.id);

        assert_eq!(merged, ForkMerge::Updated);
        let after = local.load(&child.id).unwrap();
        assert_eq!(after.parent, Some(second.id));
        assert_eq!(after.priority, Some(3));
        assert!(!local.load(&first.id).unwrap().children.contains(&child.id));
        assert!(local.load(&second.id).unwrap().children.contains(&child.id));
        assert_verifies(&local);

        // Merging the same fork state again is a no-op.
        assert_eq!(
            fork_merge(&local, &fork, &child.id),
            ForkMerge::Unchanged
        );
    }

    #[test]
    fn fork_merge_does_not_clear_a_local_parent() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let parent = fork.create("parent", NewTicketOpts::default()).unwrap();
        let child = fork
            .create(
                "child",
                NewTicketOpts {
                    parent: Some(parent.id),
                    ..Default::default()
                },
            )
            .unwrap();
        for t in [&parent, &child] {
            fork_merge(&local, &fork, &t.id);
        }
        fork.clear_parent(&child.id).unwrap();
        fork_merge(&local, &fork, &child.id);
        assert_eq!(local.load(&child.id).unwrap().parent, Some(parent.id));
    }

    #[test]
    fn fork_merge_mirrors_a_close_despite_open_subissue_and_unions_the_rest() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let parent = fork.create("parent", NewTicketOpts::default()).unwrap();
        fork_merge(&local, &fork, &parent.id);
        // Locally the parent has an open sub-issue the fork does not know about.
        let child = local
            .create(
                "local child",
                NewTicketOpts {
                    parent: Some(parent.id),
                    ..Default::default()
                },
            )
            .unwrap();
        local.add_tag(&parent.id, "local-tag").unwrap();

        fork.set_lifecycle(&parent.id, TicketStatus::Closed, TicketState::Resolved)
            .unwrap();
        fork.add_tag(&parent.id, "fork-tag").unwrap();
        fork.add_comment(&parent.id, "from the fork").unwrap();
        assert_eq!(
            fork_merge(&local, &fork, &parent.id),
            ForkMerge::Updated
        );

        let after = local.load(&parent.id).unwrap();
        assert_eq!(after.status, TicketStatus::Closed);
        assert_eq!(after.state, TicketState::Resolved);
        assert_eq!(after.closed_by.as_deref(), Some(fork.email()));
        assert!(after.tags.contains("local-tag") && after.tags.contains("fork-tag"));
        assert_eq!(after.comments.len(), 1);
        assert!(after.children.contains(&child.id));
        assert_verifies(&local);
    }

    #[test]
    fn review_create_and_load_round_trip() {
        let (store, _td) = test_store();
        let fake = FakeVcsAdapter::default();
        fake.set_current_branch("feat/review");
        fake.set_ref("HEAD", "1111111");
        fake.set_branch_head("feat/review", "2222222");
        fake.set_default_base("main");
        fake.set_merge_base("main", "feat/review", "0000000");
        fake.set_revisions(
            "0000000",
            "2222222",
            vec!["1111111".into(), "2222222".into()],
        );
        fake.set_change_id("1111111", "ch-1");
        fake.set_patch_id("1111111", "p-1");
        fake.set_change_id("2222222", "ch-2");
        fake.set_patch_id("2222222", "p-2");

        let ticket = store
            .create("Reviewable task", NewTicketOpts::default())
            .unwrap();

        let review = store
            .create_review(
                &NewReviewOpts {
                    branch: Some("feat/review".into()),
                    base: Some("main".into()),
                    ticket: Some(ticket.short_id()),
                    title: Some("Reviewable task".into()),
                    description: Some("Implement feature".into()),
                    reviewers: vec!["reviewer@example.com".into()],
                    commits: None,
                },
                &fake,
            )
            .unwrap();

        assert_eq!(review.title, "Reviewable task");
        assert_eq!(review.status, "open");
        assert_eq!(review.base_sha.as_deref(), Some("0000000"));
        assert_eq!(review.head_sha.as_deref(), Some("2222222"));
        assert_eq!(review.reviewers, vec!["reviewer@example.com".to_string()]);
        assert_eq!(review.revisions.len(), 2);
        assert_eq!(review.ticket_id, Some(ticket.id));

        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.branch_id, review.branch_id);
        assert_eq!(loaded.title, "Reviewable task");
        assert_eq!(loaded.ticket_id, Some(ticket.id));

        let list = store.list_reviews().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].branch_id, review.branch_id);

        let resolved_id = store
            .resolve_review_branch_id(Some(&ticket.short_id()), Some(&fake))
            .unwrap();
        assert_eq!(resolved_id, review.branch_id);

        let resolved_by_branch = store
            .resolve_review_branch_id(Some("feat/review"), Some(&fake))
            .unwrap();
        assert_eq!(resolved_by_branch, review.branch_id);
    }

    #[test]
    fn review_update_tracks_revision_change_history() {
        let (store, _td) = test_store();
        let fake = FakeVcsAdapter::default();
        fake.set_default_base("main");
        fake.set_branch_head("feat/history", "1111111");
        fake.set_merge_base("main", "feat/history", "0000000");
        fake.set_revisions("0000000", "1111111", vec!["1111111".into()]);
        fake.set_change_id("1111111", "ch-1");
        fake.set_patch_id("1111111", "p-1");

        let review = store
            .create_review(
                &NewReviewOpts {
                    branch: Some("feat/history".into()),
                    base: Some("main".into()),
                    ticket: None,
                    title: Some("History test".into()),
                    description: None,
                    reviewers: vec![],
                    commits: None,
                },
                &fake,
            )
            .unwrap();
        assert_eq!(review.revisions.len(), 1);

        // Advance branch head to 2222222
        fake.set_branch_head("feat/history", "2222222");
        fake.set_revisions(
            "0000000",
            "2222222",
            vec!["1111111".into(), "2222222".into()],
        );
        fake.set_change_id("2222222", "ch-2");
        fake.set_patch_id("2222222", "p-2");

        let (updated, new_head) = store.update_review(&review.branch_id, None, &fake).unwrap();
        assert_eq!(new_head, "2222222");
        assert_eq!(updated.head_sha.as_deref(), Some("2222222"));
        assert_eq!(updated.revisions.len(), 2);
        // History should retain previous commits
        assert!(updated.revision_history.iter().any(|r| r.sha == "1111111"));
        assert!(updated.revision_history.iter().any(|r| r.sha == "2222222"));
    }

    #[test]
    fn review_approvals_and_all_approved_rule() {
        let (store, _td) = test_store();
        let fake = FakeVcsAdapter::default();
        fake.set_default_base("main");
        fake.set_branch_head("feat/approvals", "2222222");
        fake.set_merge_base("main", "feat/approvals", "0000000");
        fake.set_revisions(
            "0000000",
            "2222222",
            vec!["1111111".into(), "2222222".into()],
        );

        let review = store
            .create_review(
                &NewReviewOpts {
                    branch: Some("feat/approvals".into()),
                    base: Some("main".into()),
                    ticket: None,
                    title: Some("Approvals test".into()),
                    description: None,
                    reviewers: vec![],
                    commits: None,
                },
                &fake,
            )
            .unwrap();
        assert_eq!(review.status, "open");

        // Approve commit 1 - review not yet all approved
        let all_approved = store
            .approve_review_commit(Some(&review.branch_id), "1111111", "looks good 1")
            .unwrap();
        assert!(!all_approved);
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "open");

        // Approve commit 2 - now all commits approved!
        let all_approved = store
            .approve_review_commit(Some(&review.branch_id), "2222222", "looks good 2")
            .unwrap();
        assert!(all_approved);
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "approved");
    }

    #[test]
    fn review_request_changes_and_lifecycle() {
        let (store, _td) = test_store();
        let fake = FakeVcsAdapter::default();
        fake.set_default_base("main");
        fake.set_branch_head("feat/lifecycle", "1111111");
        fake.set_merge_base("main", "feat/lifecycle", "0000000");
        fake.set_revisions("0000000", "1111111", vec!["1111111".into()]);

        let review = store
            .create_review(
                &NewReviewOpts {
                    branch: Some("feat/lifecycle".into()),
                    base: Some("main".into()),
                    ticket: None,
                    title: Some("Lifecycle test".into()),
                    description: None,
                    reviewers: vec![],
                    commits: None,
                },
                &fake,
            )
            .unwrap();

        // Request changes
        store
            .request_changes(
                &review.branch_id,
                ReviewMessage {
                    author: "alice@example.com".into(),
                    body: "needs tests".into(),
                    message_type: "changes-requested".into(),
                    commit: None,
                    path: None,
                    lines: None,
                    at: None,
                },
            )
            .unwrap();
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "changes-requested");
        assert_eq!(loaded.messages.len(), 1);

        // Close review
        store.close_review(&review.branch_id).unwrap();
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "closed");

        // Reopen review
        store.reopen_review(&review.branch_id).unwrap();
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "open");

        // Integrate review
        store
            .integrate_review(&review.branch_id, "9999999")
            .unwrap();
        let loaded = store.load_review(&review.branch_id).unwrap();
        assert_eq!(loaded.status, "merged");
        assert_eq!(loaded.integration_sha.as_deref(), Some("9999999"));
    }
}
