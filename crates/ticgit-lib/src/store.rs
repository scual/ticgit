//! `TicketStore` - the bridge between the [`Ticket`] domain model and a
//! git-meta [`Session`].
//!
//! Every read and write goes through a [`SessionTargetHandle`] scoped to
//! the `project` target. There is no separate index; tickets are
//! discovered by prefix-scanning `ticgit:tickets`.

use std::collections::{BTreeMap, BTreeSet};

use git_meta_lib::{ListEntry, MetaValue, Session, Target};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::keys;
use crate::oplog::{Op, OpKind};
use crate::ticket::{Comment, CommentBody, NewTicketOpts, Ticket, TicketState, TicketStatus};
use crate::writeup::{NewWriteupOpts, Writeup, WriteupStatus, WriteupVersion};

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

        // Scalar fields are born as a single `create` op — the synced source of
        // truth. The format-version marker stays a plain key (same value on
        // every clone, never a conflict) and gates old binaries off op-based
        // tickets via the FormatTooNew guard.
        p.set(
            &keys::ticket_field(&id, keys::FORMAT_VERSION_FIELD),
            CURRENT_TICKET_FORMAT.to_string().as_str(),
        )?;

        let mut fields = BTreeMap::new();
        fields.insert("title".to_string(), title.to_string());
        fields.insert(
            "status".to_string(),
            TicketStatus::Open.as_str().to_string(),
        );
        fields.insert("state".to_string(), TicketState::New.as_str().to_string());
        fields.insert("created-at".to_string(), now_rfc.clone());
        fields.insert("created-by".to_string(), self.session.email().to_string());

        if let Some(ref a) = opts.assigned {
            if !a.is_empty() {
                let resolved = self.resolve_user(a)?;
                validate_email(&resolved)?;
                fields.insert("assigned".to_string(), resolved);
            }
        }

        if let Some(parent_id) = opts.parent {
            // Validate parent exists.
            self.load(&parent_id)?;
            fields.insert("parent".to_string(), parent_id.to_string());
        }

        self.append_op(&id, OpKind::Create { fields })?;

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
        let p = self.session.target(&Target::project());
        let pairs = p.get_all_values(Some(&keys::tickets_prefix()))?;
        let mut fields_by_id: BTreeMap<Uuid, Vec<(String, MetaValue)>> = BTreeMap::new();
        let mut ops_by_id: BTreeMap<Uuid, Vec<Op>> = BTreeMap::new();
        for (key, value) in pairs {
            if let Some((id, _lamport, _hash)) = keys::parse_ticket_op(&key) {
                if let MetaValue::String(s) = value {
                    ops_by_id
                        .entry(id)
                        .or_default()
                        .push(serde_json::from_str::<Op>(&s)?);
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
        for id in ids {
            let fields = fields_by_id.remove(&id).unwrap_or_default();
            let ops = ops_by_id.remove(&id).unwrap_or_default();
            read_format_version(id, &fields)?;
            let projected = project_scalar_fields(fields, &ops);
            if let Some(t) = build_ticket(id, projected) {
                out.push(t);
            }
        }
        Ok(out)
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
        let projected = project_scalar_fields(fields, &ops);
        build_ticket(*id, projected).ok_or(Error::NotFound(*id))
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

    /// Direct children of `id` that are still open (`status == Open`).
    ///
    /// Dangling child references that fail to load are skipped rather than
    /// propagated, mirroring the `.ok()` handling in `set_parent`'s ancestor
    /// walk, so a corrupt reference can never wedge a close.
    pub fn open_children(&self, id: &Uuid) -> Result<Vec<Ticket>> {
        let ticket = self.load(id)?;
        let mut open = Vec::new();
        for child_id in &ticket.children {
            if let Ok(child) = self.load(child_id) {
                if child.status == TicketStatus::Open {
                    open.push(child);
                }
            }
        }
        Ok(open)
    }

    /// Direct dependencies of `id` (its `depends_on` targets) that are still
    /// open. Dangling references that fail to load are skipped.
    pub fn open_dependencies(&self, id: &Uuid) -> Result<Vec<Ticket>> {
        let ticket = self.load(id)?;
        let mut open = Vec::new();
        for dep_id in &ticket.depends_on {
            if let Ok(dep) = self.load(dep_id) {
                if dep.status == TicketStatus::Open {
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
            p.set_remove(
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
            p.set_remove(
                &keys::ticket_field(&old_parent, "children"),
                &child_id.to_string(),
            )?;
        }

        self.apply_scalars(child_id, BTreeMap::new(), vec!["parent".to_string()])
    }

    /// Delete a ticket and remove parent/child relationship references.
    pub fn delete_ticket(&self, id: &Uuid) -> Result<()> {
        let ticket = self.load(id)?;
        let p = self.project_handle();

        if let Some(parent_id) = ticket.parent {
            p.set_remove(&keys::ticket_field(&parent_id, "children"), &id.to_string())?;
        }

        for child_id in &ticket.children {
            // `parent` is op-managed, so orphan the child with a clear op
            // (removing the scalar key would be a no-op on op-based tickets).
            self.apply_scalars(child_id, BTreeMap::new(), vec!["parent".to_string()])?;
        }

        // Dependencies are denormalized on both sides, so clean up the reverse
        // references too, or other tickets keep dangling depends_on/blocks UUIDs
        // that never resolve (e.g. silently un-workable in `ti next`).
        for dep_id in &ticket.depends_on {
            p.set_remove(&keys::ticket_field(dep_id, "blocks"), &id.to_string())?;
        }
        for blocked_id in &ticket.blocks {
            p.set_remove(
                &keys::ticket_field(blocked_id, "depends_on"),
                &id.to_string(),
            )?;
        }

        for (key, _) in p.get_all_values(Some(&keys::ticket_prefix(id)))? {
            p.remove(&key)?;
        }

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
        p.set_remove(
            &keys::ticket_field(id, "depends_on"),
            &dependency_id.to_string(),
        )?;
        p.set_remove(
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
        self.project_handle()
            .set_remove(&keys::ticket_field(id, "tags"), tag)?;
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
        self.project_handle()
            .set_remove(&keys::writeup_field(id, "tags"), tag)?;
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
        p.set_remove(
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
                p.set_remove(&key, &member)?;
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
        self.project_handle()
            .set_remove(&keys::system_key("owners"), who.trim())?;
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
        p.set_remove(&keys::user_key(nick), email)?;
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

    /// Fold a ticket read from a fork into this store. An unknown ticket is
    /// imported under its original id as a `create` op carrying the fork's
    /// scalars; a known one gets a single op for the scalars where the fork
    /// differs (the fork wins, except that a missing parent never clears a
    /// local one), plus the union of tags and children, the fork's `meta:*`
    /// values, and any comments not already present.
    ///
    /// The fork is authoritative here, so the open sub-issue and open
    /// dependency close guards are not applied: mirroring a close never fails.
    pub fn merge_fork_ticket(&self, remote: &Ticket) -> Result<ForkMerge> {
        match self.load(&remote.id) {
            Ok(local) => self.merge_fork_into(&local, remote),
            Err(Error::NotFound(_)) => {
                self.import_fork_ticket(remote)?;
                Ok(ForkMerge::Imported)
            }
            Err(e) => Err(e),
        }
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
                p.set_remove(
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
            if build_ticket(id, projected).is_none() {
                issues.push("does not project to a valid ticket state".to_string());
            }

            out.push(VerifyOutcome {
                id,
                short_id: id.to_string().chars().take(6).collect(),
                ok: issues.is_empty(),
                issues,
                warnings,
            });
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

    pub fn pull(&self, remote: Option<&str>) -> Result<()> {
        let _ = self.session.pull(remote)?;
        Ok(())
    }

    pub fn push(&self, remote: Option<&str>) -> Result<()> {
        let _ = self.session.push_once(remote)?;
        Ok(())
    }

    /// Rebuild the local store from the git-meta refs, returning the ticket
    /// count afterwards.
    ///
    /// Recovers tickets that live in `refs/{ns}/*` but are missing from the
    /// local `git-meta.sqlite` — e.g. ops that were serialized and pushed from
    /// a since-removed worktree yet never materialized into this checkout's
    /// store, or a store file that was lost entirely.
    ///
    /// It drops the local serialization ref (`refs/{ns}/local/main`) so
    /// git-meta cannot treat the store as already up-to-date, then
    /// materializes every remote tracking ref, which re-applies the full
    /// remote tree into the store. This is non-destructive: existing rows are
    /// re-applied and nothing local is deleted.
    ///
    /// Requires at least one fetched remote tracking ref
    /// (`refs/{ns}/remotes/*`). If none exists it returns an error without
    /// touching any ref, so the only serialized copy is never dropped when it
    /// cannot be rebuilt — run a sync first in that case.
    pub fn reindex(&self) -> Result<usize> {
        let ns = self.session.namespace().to_string();
        let repo = gix::discover(".")
            .map_err(|e| Error::InvalidValue(format!("not inside a git repository: {e}")))?;

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

        // Re-project every remote tracking ref into the store.
        let _ = self.session.materialize(None)?;

        Ok(self.list()?.len())
    }
}

// ---------------------------------------------------------------------------
// Op-log projection
// ---------------------------------------------------------------------------

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
    })
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
    fn delete_ticket_removes_parent_child_references() {
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

        store.delete_ticket(&child.id).unwrap();

        assert!(store.load(&child.id).is_err());
        let parent = store.load(&parent.id).unwrap();
        assert!(!parent.children.contains(&child.id));
    }

    #[test]
    fn delete_parent_unparents_children() {
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

        store.delete_ticket(&parent.id).unwrap();

        assert!(store.load(&parent.id).is_err());
        let child = store.load(&child.id).unwrap();
        assert_eq!(child.parent, None);
    }

    #[test]
    fn delete_ticket_removes_dangling_dependency_references() {
        let (store, _td) = test_store();
        let a = store.create("a", NewTicketOpts::default()).unwrap();
        let b = store.create("b", NewTicketOpts::default()).unwrap();
        let c = store.create("c", NewTicketOpts::default()).unwrap();

        // a depends on b; c depends on a. Deleting a must clean both sides.
        store.add_dependency(&a.id, &b.id).unwrap();
        store.add_dependency(&c.id, &a.id).unwrap();

        store.delete_ticket(&a.id).unwrap();

        // b no longer lists a in `blocks`.
        assert!(!store.load(&b.id).unwrap().blocks.contains(&a.id));
        // c no longer lists a in `depends_on`.
        assert!(!store.load(&c.id).unwrap().depends_on.contains(&a.id));
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
            local.merge_fork_ticket(&parent).unwrap(),
            ForkMerge::Imported
        );
        assert_eq!(
            local.merge_fork_ticket(&child).unwrap(),
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
            local.merge_fork_ticket(&fork.load(&t.id).unwrap()).unwrap();
        }
        // A local edit so the ticket has ops of its own.
        local.set_priority(&child.id, Some(3)).unwrap();

        fork.set_parent(&child.id, &second.id).unwrap();
        let merged = local
            .merge_fork_ticket(&fork.load(&child.id).unwrap())
            .unwrap();

        assert_eq!(merged, ForkMerge::Updated);
        let after = local.load(&child.id).unwrap();
        assert_eq!(after.parent, Some(second.id));
        assert_eq!(after.priority, Some(3));
        assert!(!local.load(&first.id).unwrap().children.contains(&child.id));
        assert!(local.load(&second.id).unwrap().children.contains(&child.id));
        assert_verifies(&local);

        // Merging the same fork state again is a no-op.
        assert_eq!(
            local
                .merge_fork_ticket(&fork.load(&child.id).unwrap())
                .unwrap(),
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
            local.merge_fork_ticket(&fork.load(&t.id).unwrap()).unwrap();
        }
        fork.clear_parent(&child.id).unwrap();
        local
            .merge_fork_ticket(&fork.load(&child.id).unwrap())
            .unwrap();
        assert_eq!(local.load(&child.id).unwrap().parent, Some(parent.id));
    }

    #[test]
    fn fork_merge_mirrors_a_close_despite_open_subissue_and_unions_the_rest() {
        let (fork, _fork_td) = test_store();
        let (local, _local_td) = test_store();
        let parent = fork.create("parent", NewTicketOpts::default()).unwrap();
        local.merge_fork_ticket(&parent).unwrap();
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
            local
                .merge_fork_ticket(&fork.load(&parent.id).unwrap())
                .unwrap(),
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
}
