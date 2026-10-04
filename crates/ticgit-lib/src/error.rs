//! Error types for the ticgit library.

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git-meta error: {0}")]
    GitMeta(#[from] git_meta_lib::Error),

    #[error("ticket {0} not found")]
    NotFound(Uuid),

    #[error("ticket prefix `{0}` is ambiguous: matches {1} tickets")]
    Ambiguous(String, usize),

    #[error("ticket prefix `{0}` matches no ticket")]
    NoMatch(String),

    #[error("invalid ticket state `{0}` (expected one of: new, assigned, in-progress, blocked, review, resolved, wontfix, duplicate, invalid)")]
    InvalidState(String),

    #[error("invalid ticket status `{0}` (expected one of: open, closed)")]
    InvalidStatus(String),

    #[error("invalid value: {0}")]
    InvalidValue(String),

    #[error("invalid format version `{0}`")]
    InvalidFormatVersion(String),

    #[error(
        "ticket {id} uses format version {version}, but this ti supports up to {supported}; upgrade ti"
    )]
    FormatTooNew {
        id: Uuid,
        version: u32,
        supported: u32,
    },

    #[error("cannot close: ticket has {0}")]
    OpenSubissues(String),

    #[error("cannot close: ticket has {0}")]
    OpenDependencies(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("time formatting error: {0}")]
    Time(String),

    #[error("signing error: {0}")]
    Signing(String),
}

impl Error {
    /// True when a pull failed only because the remote has no meta ref yet.
    /// git-meta-lib reports this as an opaque git error, so this is the one
    /// place that knows its wording.
    pub fn is_missing_remote_ref(&self) -> bool {
        matches!(self, Error::GitMeta(_)) && self.to_string().contains("couldn't find remote ref")
    }
}

pub type Result<T> = std::result::Result<T, Error>;
