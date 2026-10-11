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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
            self.revisions
                .iter()
                .map(|entry| entry.sha.clone())
                .collect()
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
    pub commits: Option<Vec<String>>,
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

/// Abstract VCS operations required by the code review subsystem.
pub trait ReviewVcs: Send + Sync {
    /// Return the currently checked out Git branch name.
    fn current_branch(&self) -> crate::Result<String>;

    /// Resolve a Git reference or expression to a full SHA.
    fn resolve_ref(&self, reference: &str) -> crate::Result<String>;

    /// Return the best default base ref (`origin/main`, `origin/master`, `main`, `master`, or `HEAD`).
    fn default_base_ref(&self) -> String;

    /// Compute the merge-base between `base_ref` and `branch_name`.
    fn merge_base(&self, base_ref: &str, branch_name: &str) -> crate::Result<String>;

    /// Resolve the head commit SHA for a review branch (checking local then `origin/`).
    fn resolve_branch_head(&self, branch_name: &str) -> crate::Result<String>;

    /// Return list of commit SHAs between `base_sha` and `head_sha`.
    fn revision_list(&self, base_sha: &str, head_sha: &str) -> crate::Result<Vec<String>>;

    /// Extract the `change-id` header from commit metadata if present.
    fn commit_change_id(&self, sha: &str) -> Option<String>;

    /// Compute the Git patch-id for a commit.
    fn commit_patch_id(&self, sha: &str) -> Option<String>;

    /// Return the remote URL for origin if configured.
    fn remote_url(&self) -> crate::Result<Option<String>>;

    /// Load head, base, commits, and title for a branch (trying `but` then Git).
    fn load_branch_snapshot(&self, branch_name: &str) -> crate::Result<ReviewBranchSnapshot>;

    /// Load reviewable candidate branch choices (trying `but` then Git).
    fn load_branch_choices(
        &self,
        connected_branches: &BTreeSet<String>,
    ) -> crate::Result<Vec<ReviewBranchChoice>>;

    /// Load detailed subject/author/shortstat metadata for a commit.
    fn commit_info(&self, sha: &str) -> ReviewCommitInfo;

    /// Return raw patch/diff lines for a commit.
    fn commit_patch_lines(&self, sha: &str) -> Vec<String>;

    /// Return the number of changed files between `oldest^` and `head`.
    fn changed_file_count(&self, oldest: &str, head: &str) -> usize;
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::RwLock;

/// In-memory fake VCS adapter for unit tests.
#[derive(Debug, Default)]
pub struct FakeVcsAdapter {
    pub current_branch: RwLock<Option<String>>,
    pub refs: RwLock<HashMap<String, String>>,
    pub default_base: RwLock<Option<String>>,
    pub merge_bases: RwLock<HashMap<(String, String), String>>,
    pub branch_heads: RwLock<HashMap<String, String>>,
    pub revisions: RwLock<HashMap<(String, String), Vec<String>>>,
    pub change_ids: RwLock<HashMap<String, String>>,
    pub patch_ids: RwLock<HashMap<String, String>>,
    pub remote_url: RwLock<Option<String>>,
    pub branch_snapshots: RwLock<HashMap<String, ReviewBranchSnapshot>>,
    pub branch_choices: RwLock<Vec<ReviewBranchChoice>>,
    pub commit_infos: RwLock<HashMap<String, ReviewCommitInfo>>,
    pub commit_patch_lines: RwLock<HashMap<String, Vec<String>>>,
    pub changed_file_counts: RwLock<HashMap<(String, String), usize>>,
}

impl FakeVcsAdapter {
    pub fn set_current_branch(&self, branch: &str) {
        *self.current_branch.write().unwrap() = Some(branch.to_string());
    }

    pub fn set_ref(&self, reference: &str, sha: &str) {
        self.refs
            .write()
            .unwrap()
            .insert(reference.to_string(), sha.to_string());
    }

    pub fn set_default_base(&self, base: &str) {
        *self.default_base.write().unwrap() = Some(base.to_string());
    }

    pub fn set_merge_base(&self, base_ref: &str, branch: &str, sha: &str) {
        self.merge_bases
            .write()
            .unwrap()
            .insert((base_ref.to_string(), branch.to_string()), sha.to_string());
    }

    pub fn set_branch_head(&self, branch: &str, sha: &str) {
        self.branch_heads
            .write()
            .unwrap()
            .insert(branch.to_string(), sha.to_string());
    }

    pub fn set_revisions(&self, base_sha: &str, head_sha: &str, commits: Vec<String>) {
        self.revisions
            .write()
            .unwrap()
            .insert((base_sha.to_string(), head_sha.to_string()), commits);
    }

    pub fn set_change_id(&self, sha: &str, change_id: &str) {
        self.change_ids
            .write()
            .unwrap()
            .insert(sha.to_string(), change_id.to_string());
    }

    pub fn set_patch_id(&self, sha: &str, patch_id: &str) {
        self.patch_ids
            .write()
            .unwrap()
            .insert(sha.to_string(), patch_id.to_string());
    }

    pub fn set_remote_url(&self, url: Option<&str>) {
        *self.remote_url.write().unwrap() = url.map(str::to_string);
    }

    pub fn set_branch_snapshot(&self, branch: &str, snapshot: ReviewBranchSnapshot) {
        self.branch_snapshots
            .write()
            .unwrap()
            .insert(branch.to_string(), snapshot);
    }

    pub fn set_branch_choices(&self, choices: Vec<ReviewBranchChoice>) {
        *self.branch_choices.write().unwrap() = choices;
    }

    pub fn set_commit_info(&self, sha: &str, info: ReviewCommitInfo) {
        self.commit_infos
            .write()
            .unwrap()
            .insert(sha.to_string(), info);
    }

    pub fn set_commit_patch_lines(&self, sha: &str, lines: Vec<String>) {
        self.commit_patch_lines
            .write()
            .unwrap()
            .insert(sha.to_string(), lines);
    }

    pub fn set_changed_file_count(&self, oldest: &str, head: &str, count: usize) {
        self.changed_file_counts
            .write()
            .unwrap()
            .insert((oldest.to_string(), head.to_string()), count);
    }
}

impl ReviewVcs for FakeVcsAdapter {
    fn current_branch(&self) -> crate::Result<String> {
        self.current_branch.read().unwrap().clone().ok_or_else(|| {
            crate::Error::Vcs(
                "cannot default review to detached HEAD; pass --branch or a review id".into(),
            )
        })
    }

    fn resolve_ref(&self, reference: &str) -> crate::Result<String> {
        self.refs
            .read()
            .unwrap()
            .get(reference)
            .cloned()
            .ok_or_else(|| crate::Error::Vcs(format!("unknown ref `{reference}`")))
    }

    fn default_base_ref(&self) -> String {
        self.default_base
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "HEAD".to_string())
    }

    fn merge_base(&self, base_ref: &str, branch_name: &str) -> crate::Result<String> {
        if let Some(sha) = self
            .merge_bases
            .read()
            .unwrap()
            .get(&(base_ref.to_string(), branch_name.to_string()))
        {
            return Ok(sha.clone());
        }
        self.resolve_ref(base_ref)
            .or_else(|_| self.resolve_ref("HEAD"))
    }

    fn resolve_branch_head(&self, branch_name: &str) -> crate::Result<String> {
        let heads = self.branch_heads.read().unwrap();
        if let Some(sha) = heads.get(branch_name) {
            return Ok(sha.clone());
        }
        let origin = format!("origin/{branch_name}");
        if let Some(sha) = heads.get(&origin) {
            return Ok(sha.clone());
        }
        let refs = self.refs.read().unwrap();
        if let Some(sha) = refs.get(branch_name) {
            return Ok(sha.clone());
        }
        if let Some(sha) = refs.get(&origin) {
            return Ok(sha.clone());
        }
        Err(crate::Error::Vcs(format!(
            "could not resolve review branch `{branch_name}`; no Git ref with that name exists"
        )))
    }

    fn revision_list(&self, base_sha: &str, head_sha: &str) -> crate::Result<Vec<String>> {
        if base_sha.is_empty() || head_sha.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .revisions
            .read()
            .unwrap()
            .get(&(base_sha.to_string(), head_sha.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    fn commit_change_id(&self, sha: &str) -> Option<String> {
        self.change_ids.read().unwrap().get(sha).cloned()
    }

    fn commit_patch_id(&self, sha: &str) -> Option<String> {
        self.patch_ids.read().unwrap().get(sha).cloned()
    }

    fn remote_url(&self) -> crate::Result<Option<String>> {
        Ok(self.remote_url.read().unwrap().clone())
    }

    fn load_branch_snapshot(&self, branch_name: &str) -> crate::Result<ReviewBranchSnapshot> {
        if let Some(snapshot) = self.branch_snapshots.read().unwrap().get(branch_name) {
            return Ok(snapshot.clone());
        }
        let head_sha = self.resolve_branch_head(branch_name)?;
        let base_sha = self.merge_base(&self.default_base_ref(), branch_name)?;
        let commits = self.revision_list(&base_sha, &head_sha)?;
        let title = self.commit_info(&head_sha).subject;
        Ok(ReviewBranchSnapshot {
            base_sha,
            head_sha,
            commits,
            title,
        })
    }

    fn load_branch_choices(
        &self,
        connected_branches: &BTreeSet<String>,
    ) -> crate::Result<Vec<ReviewBranchChoice>> {
        let choices = self.branch_choices.read().unwrap();
        Ok(choices
            .iter()
            .filter(|c| !connected_branches.contains(&c.name))
            .cloned()
            .collect())
    }

    fn commit_info(&self, sha: &str) -> ReviewCommitInfo {
        self.commit_infos
            .read()
            .unwrap()
            .get(sha)
            .cloned()
            .unwrap_or_else(|| ReviewCommitInfo {
                subject: sha.get(..7).unwrap_or(sha).to_string(),
                body: String::new(),
                author: String::new(),
                updated: String::new(),
                shortstat: String::new(),
                change_id: self.commit_change_id(sha),
                patch_id: self.commit_patch_id(sha),
            })
    }

    fn commit_patch_lines(&self, sha: &str) -> Vec<String> {
        self.commit_patch_lines
            .read()
            .unwrap()
            .get(sha)
            .cloned()
            .unwrap_or_default()
    }

    fn changed_file_count(&self, oldest: &str, head: &str) -> usize {
        self.changed_file_counts
            .read()
            .unwrap()
            .get(&(oldest.to_string(), head.to_string()))
            .copied()
            .unwrap_or(0)
    }
}

/// Real VCS adapter that runs `git` and `but` commands.
#[derive(Debug, Clone, Default)]
pub struct RealVcsAdapter {
    pub cwd: Option<PathBuf>,
}

impl RealVcsAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self { cwd: None }
    }

    #[must_use]
    pub fn for_path(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: Some(cwd.into()),
        }
    }

    fn git_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }
        cmd.args(args);
        cmd
    }

    fn but_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("but");
        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }
        cmd.args(args);
        cmd
    }

    fn git_output(&self, args: &[&str]) -> crate::Result<String> {
        let output = self.git_command(args).output().map_err(|e| {
            crate::Error::Vcs(format!("failed to spawn git {}: {e}", args.join(" ")))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Vcs(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn resolve_branch_ref(&self, branch_name: &str) -> crate::Result<ResolvedBranchRef> {
        let mut candidates = vec![branch_name.to_string()];
        if !branch_name.starts_with("origin/") && !branch_name.starts_with("refs/") {
            candidates.push(format!("origin/{branch_name}"));
        }
        let mut last_error = None;
        for candidate in candidates {
            match self.resolve_ref(&candidate) {
                Ok(sha) => {
                    return Ok(ResolvedBranchRef {
                        name: candidate,
                        sha,
                    });
                }
                Err(err) => last_error = Some(err),
            }
        }
        if let Some(err) = last_error {
            Err(crate::Error::Vcs(format!(
                "could not resolve review branch `{branch_name}`: {err}"
            )))
        } else {
            Err(crate::Error::Vcs(
                "could not resolve empty review branch".into(),
            ))
        }
    }

    fn fill_missing_commits_ahead(&self, choices: &mut [ReviewBranchChoice]) {
        let target = self.default_base_ref();
        for choice in choices.iter_mut() {
            if choice.commits_ahead.is_none() {
                if let Ok(branch_ref) = self.resolve_branch_ref(&choice.name) {
                    let range = format!("{target}..{}", branch_ref.name);
                    if let Ok(output) = self.git_command(&["rev-list", "--count", &range]).output()
                    {
                        if output.status.success() {
                            choice.commits_ahead =
                                String::from_utf8_lossy(&output.stdout).trim().parse().ok();
                        }
                    }
                }
            }
        }
    }

    fn load_git_review_branch_choices(
        &self,
        connected_branches: &BTreeSet<String>,
    ) -> crate::Result<Vec<ReviewBranchChoice>> {
        let output = self.git_command(&[
            "for-each-ref",
            "--format=%(refname:short)%09%(committerdate:unix)%09%(authorname)%09%(authoremail)",
            "refs/heads",
            "refs/remotes",
        ]).output().map_err(|e| crate::Error::Vcs(format!("running git for-each-ref: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Vcs(format!(
                "git for-each-ref failed: {}",
                stderr.trim()
            )));
        }
        let mut choices = parse_git_review_branch_choices(connected_branches, &output.stdout)?;
        self.fill_missing_commits_ahead(&mut choices);
        Ok(choices)
    }
}

#[derive(Debug)]
struct ResolvedBranchRef {
    name: String,
    sha: String,
}

impl ReviewVcs for RealVcsAdapter {
    fn current_branch(&self) -> crate::Result<String> {
        let branch = self.git_output(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        if branch == "HEAD" {
            return Err(crate::Error::Vcs(
                "cannot default review to detached HEAD; pass --branch or a review id".into(),
            ));
        }
        Ok(branch)
    }

    fn resolve_ref(&self, reference: &str) -> crate::Result<String> {
        self.git_output(&["rev-parse", reference])
    }

    fn default_base_ref(&self) -> String {
        for candidate in ["origin/main", "origin/master", "main", "master"] {
            if self.resolve_ref(candidate).is_ok() {
                return candidate.to_string();
            }
        }
        "HEAD".into()
    }

    fn merge_base(&self, base_ref: &str, branch_name: &str) -> crate::Result<String> {
        if let Ok(output) = self
            .git_command(&["merge-base", base_ref, branch_name])
            .output()
        {
            if output.status.success() {
                let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !sha.is_empty() {
                    return Ok(sha);
                }
            }
        }
        self.resolve_ref(base_ref)
            .or_else(|_| self.resolve_ref("HEAD"))
    }

    fn resolve_branch_head(&self, branch_name: &str) -> crate::Result<String> {
        self.resolve_branch_ref(branch_name).map(|r| r.sha)
    }

    fn revision_list(&self, base_sha: &str, head_sha: &str) -> crate::Result<Vec<String>> {
        if base_sha.is_empty() || head_sha.is_empty() {
            return Ok(Vec::new());
        }
        let range = format!("{base_sha}..{head_sha}");
        let output = self.git_command(&["rev-list", &range]).output();
        let Ok(output) = output else {
            return Ok(Vec::new());
        };
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn commit_change_id(&self, sha: &str) -> Option<String> {
        let output = self.git_command(&["cat-file", "-p", sha]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .take_while(|line| !line.is_empty())
            .find_map(|line| line.strip_prefix("change-id ").map(str::to_string))
    }

    fn commit_patch_id(&self, sha: &str) -> Option<String> {
        let mut diff = self
            .git_command(&["diff-tree", "--patch", sha])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let stdout = diff.stdout.take()?;
        let patch_id = self
            .git_command(&["patch-id", "--stable"])
            .stdin(Stdio::from(stdout))
            .output()
            .ok()?;
        let _ = diff.wait();
        if !patch_id.status.success() {
            return None;
        }
        String::from_utf8_lossy(&patch_id.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string)
    }

    fn remote_url(&self) -> crate::Result<Option<String>> {
        match self.git_output(&["remote", "get-url", "origin"]) {
            Ok(url) if !url.is_empty() => Ok(Some(url)),
            _ => Ok(None),
        }
    }

    fn load_branch_snapshot(&self, branch_name: &str) -> crate::Result<ReviewBranchSnapshot> {
        if let Ok(output) = self
            .but_command(&["branch", "show", branch_name, "--json"])
            .output()
        {
            if output.status.success() {
                if let Ok(mut snapshot) = parse_review_branch_snapshot(branch_name, &output.stdout)
                {
                    if snapshot.head_sha.is_empty() {
                        snapshot.head_sha = self.resolve_ref(branch_name).unwrap_or_default();
                    }
                    if snapshot.base_sha.is_empty() {
                        snapshot.base_sha = self
                            .merge_base(&self.default_base_ref(), branch_name)
                            .unwrap_or_default();
                    }
                    return Ok(snapshot);
                }
            }
        }

        let branch_ref = self.resolve_branch_ref(branch_name)?;
        let base_sha = self.merge_base(&self.default_base_ref(), &branch_ref.name)?;
        let head_sha = branch_ref.sha;
        let commits = self.revision_list(&base_sha, &head_sha)?;
        let title = self.commit_info(&head_sha).subject;
        Ok(ReviewBranchSnapshot {
            base_sha,
            head_sha,
            commits,
            title,
        })
    }

    fn load_branch_choices(
        &self,
        connected_branches: &BTreeSet<String>,
    ) -> crate::Result<Vec<ReviewBranchChoice>> {
        if let Ok(output) = self.but_command(&["branch", "list", "--json"]).output() {
            if output.status.success() {
                if let Ok(branch_list) = serde_json::from_slice::<ButBranchList>(&output.stdout) {
                    let mut branches = branch_list.branches;
                    for stack in branch_list.applied_stacks {
                        branches.extend(stack.heads);
                    }
                    let mut choices = branches
                        .into_iter()
                        .filter(|b| !connected_branches.contains(&b.name))
                        .map(|b| ReviewBranchChoice {
                            name: b.name,
                            last_commit_at: b
                                .last_commit_at
                                .and_then(|ms| OffsetDateTime::from_unix_timestamp(ms / 1000).ok()),
                            commits_ahead: b.commits_ahead,
                            author: branch_author_display(b.last_author),
                        })
                        .collect::<Vec<_>>();
                    self.fill_missing_commits_ahead(&mut choices);
                    choices.sort_by(|a, b| {
                        b.last_commit_at
                            .cmp(&a.last_commit_at)
                            .then_with(|| a.name.cmp(&b.name))
                    });
                    let mut seen = BTreeSet::new();
                    choices.retain(|c| seen.insert(c.name.clone()));
                    return Ok(choices);
                }
            }
        }
        self.load_git_review_branch_choices(connected_branches)
    }

    fn commit_info(&self, sha: &str) -> ReviewCommitInfo {
        let output = self
            .git_command(&["show", "-s", "--format=%s%n%an <%ae>%n%ct%n%B", sha])
            .output();
        let mut lines = output
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let subject = lines
            .first()
            .cloned()
            .unwrap_or_else(|| sha.get(..7).unwrap_or(sha).to_string());
        let author = lines.get(1).cloned().unwrap_or_default();
        let updated = lines
            .get(2)
            .and_then(|ts| ts.parse::<i64>().ok())
            .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts).ok())
            .map(|ts| format!("{ts}"))
            .unwrap_or_default();
        let body = if lines.len() > 3 {
            lines
                .drain(3..)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        } else {
            String::new()
        };
        let shortstat = self
            .git_command(&["show", "--shortstat", "--format=", sha])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        ReviewCommitInfo {
            subject,
            body,
            author,
            updated,
            shortstat,
            change_id: self.commit_change_id(sha),
            patch_id: self.commit_patch_id(sha),
        }
    }

    fn commit_patch_lines(&self, sha: &str) -> Vec<String> {
        let output = self
            .git_command(&[
                "show",
                "--format=",
                "--patch",
                "--find-renames",
                "--color=never",
                sha,
            ])
            .output();
        output
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn changed_file_count(&self, oldest: &str, head: &str) -> usize {
        let base = format!("{oldest}^");
        let output = self
            .git_command(&["diff", "--name-only", "--find-renames", &base, head])
            .output();
        output
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or_default()
    }
}

fn git_review_branch_choice_from_ref_line(
    connected_branches: &BTreeSet<String>,
    line: &str,
) -> Option<ReviewBranchChoice> {
    let mut fields = line.split('\t');
    let name = fields.next()?.trim();
    if !is_reviewable_git_branch_name(name) || connected_branches.contains(name) {
        return None;
    }
    let last_commit_at = fields
        .next()
        .and_then(|ts| ts.parse::<i64>().ok())
        .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts).ok());
    let author_name = fields.next().unwrap_or_default().trim();
    let author_email = fields
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    Some(ReviewBranchChoice {
        name: name.to_string(),
        last_commit_at,
        commits_ahead: None,
        author: if !author_name.is_empty() {
            author_name.to_string()
        } else if !author_email.is_empty() {
            author_email.to_string()
        } else {
            "-".to_string()
        },
    })
}

/// Extract the branch name from a `code` specifier like `origin:my-branch` or `https://...:my-branch`.
#[must_use]
pub fn code_branch_name(code: &str) -> Option<&str> {
    code.rsplit_once(':')
        .map(|(_, branch)| branch.trim())
        .filter(|branch| !branch.is_empty())
}

/// Check if a Git branch name is suitable for review (excluding base branches and meta/gitbutler refs).
#[must_use]
pub fn is_reviewable_git_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name != "HEAD"
        && name != "origin"
        && name != "main"
        && name != "master"
        && name != "origin/main"
        && name != "origin/master"
        && !name.ends_with("/HEAD")
        && !name.starts_with("gitbutler/")
        && !name.starts_with("meta/")
}

/// Parse git commit count from `git rev-list --count` output.
#[must_use]
pub fn parse_git_commit_count(stdout: &[u8]) -> Option<i64> {
    String::from_utf8_lossy(stdout).trim().parse().ok()
}

/// Parse review branch choices from `git for-each-ref` output.
pub fn parse_git_review_branch_choices(
    connected_branches: &BTreeSet<String>,
    stdout: &[u8],
) -> crate::Result<Vec<ReviewBranchChoice>> {
    let mut choices = String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| git_review_branch_choice_from_ref_line(connected_branches, line))
        .collect::<Vec<_>>();
    choices.sort_by(|a, b| {
        b.last_commit_at
            .cmp(&a.last_commit_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = BTreeSet::new();
    choices.retain(|choice| seen.insert(choice.name.clone()));
    Ok(choices)
}

/// Parse a `ReviewBranchSnapshot` from `but branch show --json` output.
pub fn parse_review_branch_snapshot(
    branch_name: &str,
    json: &[u8],
) -> crate::Result<ReviewBranchSnapshot> {
    let show = serde_json::from_slice::<ButBranchShow>(json)
        .map_err(|e| crate::Error::Vcs(format!("parsing but branch show --json: {e}")))?;
    let head = show.commits.first();
    let head_sha = head
        .map(|commit| commit.sha.clone())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_default();
    let commits = show
        .commits
        .iter()
        .map(|commit| commit.sha.clone())
        .filter(|sha| !sha.is_empty())
        .collect::<Vec<_>>();
    let base_sha = show.base_commit.map(|base| base.sha).unwrap_or_default();
    let title = head
        .map(|commit| commit.message.clone())
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| branch_name.to_string());
    Ok(ReviewBranchSnapshot {
        base_sha,
        head_sha,
        commits,
        title,
    })
}

#[derive(Debug, Deserialize)]
struct ButBranchList {
    #[serde(default, rename = "appliedStacks")]
    applied_stacks: Vec<ButAppliedStack>,
    #[serde(default)]
    branches: Vec<ButBranch>,
}

#[derive(Debug, Deserialize)]
struct ButAppliedStack {
    #[serde(default)]
    heads: Vec<ButBranch>,
}

#[derive(Debug, Deserialize)]
struct ButBranch {
    name: String,
    #[serde(default, rename = "lastCommitAt")]
    last_commit_at: Option<i64>,
    #[serde(default, rename = "commitsAhead")]
    commits_ahead: Option<i64>,
    #[serde(default, rename = "lastAuthor")]
    last_author: Option<ButAuthor>,
}

#[derive(Debug, Deserialize)]
struct ButAuthor {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ButBranchShow {
    #[serde(default)]
    commits: Vec<ButBranchCommit>,
    #[serde(default, rename = "baseCommit")]
    base_commit: Option<ButBranchBaseCommit>,
}

#[derive(Debug, Deserialize)]
struct ButBranchCommit {
    sha: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct ButBranchBaseCommit {
    sha: String,
}

fn branch_author_display(author: Option<ButAuthor>) -> String {
    author
        .and_then(|a| a.name.or(a.email))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "-".to_string())
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
        assert_eq!(
            parse_review_revision_change(&formatted_partial),
            Some(partial)
        );

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

    #[test]
    fn review_vcs_fake_adapter() {
        let fake = FakeVcsAdapter::default();
        fake.set_current_branch("feature/cool");
        assert_eq!(fake.current_branch().unwrap(), "feature/cool");

        fake.set_ref("HEAD", "1111111");
        assert_eq!(fake.resolve_ref("HEAD").unwrap(), "1111111");

        fake.set_branch_head("feature/cool", "2222222");
        assert_eq!(fake.resolve_branch_head("feature/cool").unwrap(), "2222222");

        fake.set_default_base("main");
        fake.set_merge_base("main", "feature/cool", "0000000");
        assert_eq!(fake.merge_base("main", "feature/cool").unwrap(), "0000000");

        fake.set_revisions(
            "0000000",
            "2222222",
            vec!["1111111".into(), "2222222".into()],
        );
        assert_eq!(
            fake.revision_list("0000000", "2222222").unwrap(),
            vec!["1111111".to_string(), "2222222".to_string()]
        );

        fake.set_change_id("2222222", "ch-2");
        assert_eq!(fake.commit_change_id("2222222"), Some("ch-2".to_string()));

        fake.set_patch_id("2222222", "p-2");
        assert_eq!(fake.commit_patch_id("2222222"), Some("p-2".to_string()));

        let snapshot = fake.load_branch_snapshot("feature/cool").unwrap();
        assert_eq!(snapshot.head_sha, "2222222");
        assert_eq!(snapshot.base_sha, "0000000");
    }
}
