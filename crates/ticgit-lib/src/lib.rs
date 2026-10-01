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
//! ```text
//! ticgit:tickets:<uuid>:title          # string
//! ticgit:tickets:<uuid>:description    # string (optional)
//! ticgit:tickets:<uuid>:status         # string ("open" | "closed")
//! ticgit:tickets:<uuid>:state          # string (see lifecycle above)
//! ticgit:tickets:<uuid>:assigned       # string, email (optional)
//! ticgit:tickets:<uuid>:closed-by      # string, email (set while closed)
//! ticgit:tickets:<uuid>:priority       # string (optional integer, lower = higher)
//! ticgit:tickets:<uuid>:points         # string (optional integer)
//! ticgit:tickets:<uuid>:milestone      # string (optional)
//! ticgit:tickets:<uuid>:code           # string, code URI (optional)
//! ticgit:tickets:<uuid>:spec           # string, markdown (optional)
//! ticgit:tickets:<uuid>:tags           # set
//! ticgit:tickets:<uuid>:meta:<key>     # string (arbitrary custom fields)
//! ticgit:tickets:<uuid>:comments       # list of JSON-encoded {author, body}
//! ticgit:tickets:<uuid>:parent         # UUID (this ticket's parent, if a sub-issue)
//! ticgit:tickets:<uuid>:children       # set of child UUIDs (denormalized)
//! ticgit:tickets:<uuid>:depends_on     # set of UUIDs this ticket depends on
//! ticgit:tickets:<uuid>:blocks         # set of UUIDs this ticket blocks (reverse)
//! ticgit:tickets:<uuid>:created-at     # RFC3339 string
//! ticgit:tickets:<uuid>:created-by     # string (email)
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
pub mod query;
pub mod store;
pub mod ticket;
pub mod writeup;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use error::{Error, Result};
pub use query::{Filter, SearchFilter, SearchScope, SortKey, SortOrder};
pub use store::TicketStore;
pub use ticket::{
    validate_code_uri, Comment, NewTicketOpts, Ticket, TicketLifecycle, TicketState, TicketStatus,
};
pub use writeup::{NewWriteupOpts, Writeup, WriteupStatus, WriteupVersion};

/// Re-exported for callers who want to talk to git-meta directly.
pub use git_meta_lib::{MetaValue, Session, Target};
