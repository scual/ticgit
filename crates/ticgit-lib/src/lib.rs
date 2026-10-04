//! # ticgit-lib
//!
//! Ticket-tracking on top of [git-meta](https://crates.io/crates/git-meta-lib).
//!
//! Tickets live as project-target metadata under the `ticgit:` namespace:
//!
//! Lifecycle is split into a broad `status` (`open` | `closed`) and a specific
//! `state` (open: `new`, `assigned`, `in-progress`, `blocked`, `review`;
//! closed: `resolved`, `wontfix`, `duplicate`, `invalid`).
//!
//! Scalar fields (title, description, status/state, assigned, closed-by,
//! priority, points, milestone, code, spec, parent, created-at/by) are the
//! projection of an append-only **operation log** — the conflict-free source
//! of truth, see [`oplog`]. Sets, comments, and `meta:*` keep their own keys
//! (they already merge). On-disk layout:
//!
//! ```text
//! ticgit:tickets:<uuid>:ops:<lamport>:<hash>   # one immutable operation (scalar fields)
//! ticgit:tickets:<uuid>:format-version         # string (on-disk format; current "2")
//! ticgit:tickets:<uuid>:tags           # set
//! ticgit:tickets:<uuid>:meta:<key>     # string (arbitrary custom fields)
//! ticgit:tickets:<uuid>:comments       # list of JSON-encoded {author, body}
//! ticgit:tickets:<uuid>:children       # set of child UUIDs (denormalized)
//! ticgit:tickets:<uuid>:depends_on     # set of UUIDs this ticket depends on
//! ticgit:tickets:<uuid>:blocks         # set of UUIDs this ticket blocks (reverse)
//! ticgit:identities:<email>            # SSH public key (op-signature verification)
//! ticgit:writeups:<uuid>:title         # string
//! ticgit:writeups:<uuid>:status        # string ("open" | "closed")
//! ticgit:writeups:<uuid>:priority      # string (optional integer)
//! ticgit:writeups:<uuid>:tags          # set
//! ticgit:writeups:<uuid>:authors       # set of emails
//! ticgit:writeups:<uuid>:versions      # list of JSON-encoded {author, at, body}
//! ticgit:writeups:<uuid>:tickets       # set of linked ticket UUIDs
//! ticgit:writeups:<uuid>:created-at    # RFC3339 string
//! ticgit:writeups:<uuid>:created-by    # string (email)
//! ticgit:views:<name>                  # set of UUIDs (saved selection)
//! ticgit:users:<nick>                  # set of emails (nick -> email mailmap)
//! ticgit:owners                        # set of emails
//! ticgit:schema-version                # string ("1")
//! ```
//!
//! The writeup<->ticket link is stored once, on the writeup
//! (`ticgit:writeups:<uuid>:tickets`); there is no ticket-side reverse index.
//!
//! See the top-level `README.md` and `docs/schema/v1.json` for higher-level
//! docs and the stable JSON machine-output schema.

pub mod error;
pub mod keys;
pub mod oplog;
pub mod query;
pub mod signing;
pub mod store;
pub mod ticket;
pub mod writeup;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use error::{Error, Result};
pub use oplog::{canonical_json, content_id, replay, Lamport, Op, OpId, OpKind, CURRENT_OP_FORMAT};
pub use query::{
    next_queue, Filter, NextOptions, SearchFilter, SearchScope, SortKey, SortOrder, DEFERRED_TAGS,
};
pub use store::{MigrationOutcome, TicketStore, VerifyOutcome, CURRENT_TICKET_FORMAT};
pub use ticket::{
    validate_code_uri, Comment, NewTicketOpts, Ticket, TicketLifecycle, TicketState, TicketStatus,
};
pub use writeup::{NewWriteupOpts, Writeup, WriteupStatus, WriteupVersion};

/// Re-exported for callers who want to talk to git-meta directly.
pub use git_meta_lib::{MetaValue, Session, Target};
