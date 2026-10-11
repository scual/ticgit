use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A single revision commit in a review, optionally with its change-id and patch-id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRevisionChange {
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_id: Option<String>,
}

/// A review message (comment, approval, or changes-requested).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewMessage {
    pub author: String,
    pub body: String,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// A code review record on a branch target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Review {
    pub branch_id: String,
    pub branch_name: Option<String>,
    pub ticket_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub status: String,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub integration_sha: Option<String>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
    pub reviewers: Vec<String>,
    pub revisions: Vec<ReviewRevisionChange>,
    pub revision_history: Vec<ReviewRevisionChange>,
    pub messages: Vec<ReviewMessage>,
}

impl Review {
    /// Return the friendly branch name if known, otherwise the branch id.
    #[must_use]
    pub fn branch(&self) -> &str {
        self.branch_name.as_deref().unwrap_or(&self.branch_id)
    }

    /// List commit SHAs associated with this review.
    #[must_use]
    pub fn commits(&self) -> Vec<String> {
        if !self.revisions.is_empty() {
            self.revisions.iter().map(|entry| entry.sha.clone()).collect()
        } else {
            self.head_sha.iter().cloned().collect()
        }
    }
}

/// Approval and reviewed marks for a single commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitReviewStatus {
    pub reviewed: BTreeSet<String>,
    pub approvals: BTreeSet<String>,
    pub signed_off: BTreeSet<String>,
}

/// Aggregate review progress across commits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewProgress {
    pub reviewed: usize,
    pub approved: usize,
    pub total: usize,
}

impl ReviewProgress {
    #[must_use]
    pub fn label(self) -> String {
        review_progress_label(self.approved, self.total)
    }
}

/// Summary label for review progress, e.g. "-/3" or "2/3".
#[must_use]
pub fn review_progress_label(approved: usize, total: usize) -> String {
    if approved == 0 {
        format!("-/{total}")
    } else {
        format!("{approved}/{total}")
    }
}

/// Snapshot of branch head, base, commits, and title from VCS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewBranchSnapshot {
    pub base_sha: String,
    pub head_sha: String,
    pub commits: Vec<String>,
    pub title: String,
}

/// A candidate branch to select for review in TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewBranchChoice {
    pub name: String,
    pub last_commit_at: Option<OffsetDateTime>,
    pub commits_ahead: Option<i64>,
    pub author: String,
}

/// Detailed metadata for a commit displayed in review UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommitInfo {
    pub subject: String,
    pub body: String,
    pub author: String,
    pub updated: String,
    pub shortstat: String,
    pub change_id: Option<String>,
    pub patch_id: Option<String>,
}

/// Options when creating a new review.
#[derive(Debug, Clone, Default)]
pub struct NewReviewOpts {
    pub branch: Option<String>,
    pub base: Option<String>,
    pub ticket: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub reviewers: Vec<String>,
}

/// Format a [`ReviewRevisionChange`] into the wire string format `<sha>:<change_id>:<patch_id>`.
#[must_use]
pub fn format_review_revision(entry: &ReviewRevisionChange) -> String {
    format!(
        "{}:{}:{}",
        entry.sha,
        entry.change_id.as_deref().unwrap_or_default(),
        entry.patch_id.as_deref().unwrap_or_default()
    )
}

/// Parse a revision entry string (colon-separated or JSON) into a [`ReviewRevisionChange`].
#[must_use]
pub fn parse_review_revision_change(value: &str) -> Option<ReviewRevisionChange> {
    if let Ok(entry) = serde_json::from_str::<ReviewRevisionChange>(value) {
        return Some(entry);
    }

    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some((sha, rest)) = value.split_once(':') {
        let (change_id, patch_id) = rest.split_once(':').unwrap_or((rest, ""));
        return Some(ReviewRevisionChange {
            sha: sha.to_string(),
            change_id: non_empty(change_id),
            patch_id: non_empty(patch_id),
        });
    }
    Some(ReviewRevisionChange {
        sha: value.to_string(),
        change_id: None,
        patch_id: None,
    })
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_review_revision() {
        let entry = ReviewRevisionChange {
            sha: "abc1234".to_string(),
            change_id: Some("ch-42".to_string()),
            patch_id: Some("p-99".to_string()),
        };
        let formatted = format_review_revision(&entry);
        assert_eq!(formatted, "abc1234:ch-42:p-99");
        assert_eq!(parse_review_revision_change(&formatted), Some(entry));

        let partial = ReviewRevisionChange {
            sha: "def5678".to_string(),
            change_id: None,
            patch_id: Some("p-1".to_string()),
        };
        let formatted_partial = format_review_revision(&partial);
        assert_eq!(formatted_partial, "def5678::p-1");
        assert_eq!(parse_review_revision_change(&formatted_partial), Some(partial));

        let sha_only = parse_review_revision_change("1234567").unwrap();
        assert_eq!(sha_only.sha, "1234567");
        assert_eq!(sha_only.change_id, None);
        assert_eq!(sha_only.patch_id, None);
    }

    #[test]
    fn review_message_serde_shape() {
        let message = ReviewMessage {
            author: "alice@example.com".to_string(),
            body: "Looks good".to_string(),
            message_type: "approval".to_string(),
            commit: Some("abcdef".to_string()),
            path: None,
            lines: None,
            at: Some("2026-10-10T12:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("\"type\":\"approval\""));
        assert!(!json.contains("message_type"));
        let roundtrip: ReviewMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(message, roundtrip);
    }

    #[test]
    fn review_progress_labels() {
        assert_eq!(review_progress_label(0, 3), "-/3");
        assert_eq!(review_progress_label(1, 3), "1/3");
        assert_eq!(review_progress_label(3, 3), "3/3");
    }
}
