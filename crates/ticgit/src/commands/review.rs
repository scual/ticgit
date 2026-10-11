use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use ticgit_lib::{
    format_review_revision, NewReviewOpts, RealVcsAdapter, ReviewMessage, ReviewRevisionChange,
    ReviewVcs,
};

use crate::commands::open_store;
use crate::render::{self, open_ticket_ref_lengths, ReviewTableRow};

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub command: CommandKind,
}

#[derive(Debug, Subcommand)]
pub enum CommandKind {
    /// Create review metadata for a branch.
    New(NewArgs),
    /// List known reviews.
    List(ListArgs),
    /// Show review metadata and messages.
    Show(ShowArgs),
    /// Request a reviewer.
    AddReviewer(AddReviewerArgs),
    /// Add a review comment.
    Comment(CommentArgs),
    /// Approve a review or commit.
    Approve(ApproveArgs),
    /// Request changes on a review.
    RequestChanges(RequestChangesArgs),
    /// Refresh the review head and revision list.
    Update(UpdateArgs),
    /// Record an integration/merge commit.
    Integrate(IntegrateArgs),
}

#[derive(Debug, Parser)]
pub struct NewArgs {
    /// Branch name to review. Defaults to the current branch.
    #[arg(long)]
    pub branch: Option<String>,

    /// Base ref for the review. Defaults to the first available main/master ref.
    #[arg(long)]
    pub base: Option<String>,

    /// Ticket id (or prefix) to link to this review.
    #[arg(long)]
    pub ticket: Option<String>,

    /// Review title. Defaults to the linked ticket title or branch name.
    #[arg(long)]
    pub title: Option<String>,

    /// Review description.
    #[arg(long)]
    pub description: Option<String>,

    /// Initial reviewer email. May be passed multiple times.
    #[arg(long = "reviewer")]
    pub reviewers: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct ListArgs {
    /// Show all reviews, including closed and merged reviews.
    #[arg(long)]
    pub all: bool,

    /// Only show reviews with this status.
    #[arg(long)]
    pub status: Option<String>,

    /// Output as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

#[derive(Debug, Parser)]
pub struct ShowArgs {
    /// Branch name or branch-id. Defaults to the current branch.
    pub review: Option<String>,

    /// Output as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

#[derive(Debug, Parser)]
pub struct AddReviewerArgs {
    /// Either `<email>` or `<branch> <email>`.
    #[arg(required = true, num_args = 1..=2)]
    pub args: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct CommentArgs {
    /// Optional path for an inline comment.
    #[arg(long)]
    pub path: Option<String>,

    /// Optional single line for an inline comment.
    #[arg(long)]
    pub line: Option<u32>,

    /// Optional line range for an inline comment, such as 42-82.
    #[arg(long)]
    pub lines: Option<String>,

    /// Optional commit for this comment.
    #[arg(long)]
    pub commit: Option<String>,

    /// Either `<body...>` or `<branch> <body...>`.
    #[arg(required = true, num_args = 1..)]
    pub args: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct ApproveArgs {
    /// Optional approval comment.
    #[arg(long)]
    pub comment: Option<String>,

    /// Optional branch-id, branch name, or commit SHA.
    pub target: Option<String>,
}

#[derive(Debug, Parser)]
pub struct RequestChangesArgs {
    /// Either `<body...>` or `<branch> <body...>`.
    #[arg(required = true, num_args = 1..)]
    pub args: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct UpdateArgs {
    /// Branch name or branch-id. Defaults to the current branch.
    pub review: Option<String>,

    /// New head commit. Defaults to the branch head.
    #[arg(long)]
    pub head: Option<String>,
}

#[derive(Debug, Parser)]
pub struct IntegrateArgs {
    /// Either `<sha>` or `<branch> <sha>`.
    #[arg(required = true, num_args = 1..=2)]
    pub args: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ReviewOutput {
    branch_id: String,
    branch: String,
    ticket_id: Option<String>,
    ticket_ref: Option<String>,
    title: String,
    description: String,
    status: String,
    base_sha: Option<String>,
    head_sha: Option<String>,
    integration_sha: Option<String>,
    reviewers: Vec<String>,
    revisions: Vec<ReviewRevisionChange>,
    messages: Vec<ReviewMessage>,
    approvals: String,
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        CommandKind::New(args) => new(args),
        CommandKind::List(args) => list(args),
        CommandKind::Show(args) => show(args),
        CommandKind::AddReviewer(args) => add_reviewer(args),
        CommandKind::Comment(args) => comment(args),
        CommandKind::Approve(args) => approve(args),
        CommandKind::RequestChanges(args) => request_changes(args),
        CommandKind::Update(args) => update(args),
        CommandKind::Integrate(args) => integrate(args),
    }
}

fn new(args: NewArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let opts = NewReviewOpts {
        branch: args.branch,
        base: args.base,
        ticket: args.ticket,
        title: args.title,
        description: args.description,
        reviewers: args.reviewers,
        commits: None,
    };
    let review = store.create_review(&opts, &vcs)?;
    println!("Created review {}: {}", review.branch_id, review.title);
    Ok(())
}

fn list(args: ListArgs) -> Result<()> {
    let store = open_store()?;
    let reviews = store.list_reviews()?;
    if reviews.is_empty() {
        if args.json {
            println!("[]");
        } else if args.markdown {
            println!("{}", reviews_markdown(&[]));
        } else {
            println!("No reviews");
        }
        return Ok(());
    }

    let all_tickets = store.list()?;
    let ref_lengths = open_ticket_ref_lengths(&all_tickets);
    let mut rows = Vec::new();
    let mut outputs = Vec::new();

    for review in reviews {
        let output = review_output(&store, &review, &all_tickets, &ref_lengths);
        if should_skip_review_list_row(&args, &output.status) {
            continue;
        }
        rows.push(ReviewTableRow {
            ticket: output.ticket_ref.clone(),
            ticket_unique_chars: output
                .ticket_id
                .as_deref()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .and_then(|id| ref_lengths.get(&id).copied())
                .unwrap_or(6),
            branch: output.branch.clone(),
            status: output.status.clone(),
            title: output.title.clone(),
            approvals: output.approvals.clone(),
        });
        outputs.push(output);
    }

    if rows.is_empty() {
        if args.json {
            println!("[]");
        } else if args.markdown {
            println!("{}", reviews_markdown(&[]));
        } else if let Some(status) = args.status.as_deref() {
            println!("No reviews with status `{status}`");
        } else if args.all {
            println!("No reviews");
        } else {
            println!("No open reviews");
        }
        return Ok(());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&outputs)?);
    } else if args.markdown {
        println!("{}", reviews_markdown(&outputs));
    } else {
        print!("{}", render::reviews_table(&rows));
    }
    Ok(())
}

fn should_skip_review_list_row(args: &ListArgs, status: &str) -> bool {
    if let Some(wanted) = args.status.as_deref() {
        return wanted != status;
    }
    !args.all && matches!(status, "closed" | "merged")
}

fn review_output(
    store: &ticgit_lib::TicketStore,
    review: &ticgit_lib::Review,
    all_tickets: &[ticgit_lib::Ticket],
    ref_lengths: &std::collections::BTreeMap<uuid::Uuid, usize>,
) -> ReviewOutput {
    let ticket_ref = review.ticket_id.and_then(|id| {
        all_tickets
            .iter()
            .find(|ticket| ticket.id == id)
            .map(|ticket| {
                let len = ref_lengths.get(&ticket.id).copied().unwrap_or(6).max(6);
                ticket.id.to_string().replace('-', "")[..len].to_string()
            })
    });
    let progress = store.review_progress(&review.commits(), &review.messages);
    ReviewOutput {
        branch_id: review.branch_id.clone(),
        branch: review.branch().to_string(),
        ticket_id: review.ticket_id.map(|id| id.to_string()),
        ticket_ref,
        title: review.title.clone(),
        description: review.description.clone(),
        status: review.status.clone(),
        base_sha: review.base_sha.clone(),
        head_sha: review.head_sha.clone(),
        integration_sha: review.integration_sha.clone(),
        reviewers: review.reviewers.clone(),
        revisions: review.revisions.clone(),
        messages: review.messages.clone(),
        approvals: progress.label(),
    }
}

fn reviews_markdown(reviews: &[ReviewOutput]) -> String {
    let mut out = format!("# Reviews\n\n- Count: {}\n\n", reviews.len());
    out.push_str("| Ticket | Branch | Review | Status | Title |\n");
    out.push_str("|---|---|---:|---|---|\n");
    for review in reviews {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            review.ticket_ref.as_deref().unwrap_or(""),
            markdown_cell(&review.branch),
            review.approvals,
            markdown_cell(&review.status),
            markdown_cell(&review.title)
        ));
    }
    out
}

fn review_markdown(review: &ReviewOutput) -> String {
    let mut out = format!("# Review: {}\n\n", review.title);
    out.push_str(&format!("- Branch: `{}`\n", review.branch));
    if let Some(ticket_ref) = review.ticket_ref.as_deref() {
        out.push_str(&format!("- Ticket: `{ticket_ref}`\n"));
    }
    out.push_str(&format!("- Status: `{}`\n", review.status));
    out.push_str(&format!("- Approvals: `{}`\n", review.approvals));
    if !review.description.is_empty() {
        out.push_str("\n## Description\n\n");
        out.push_str(&review.description);
        out.push('\n');
    }
    if !review.revisions.is_empty() {
        out.push_str("\n## Revisions\n\n");
        for revision in &review.revisions {
            out.push_str(&format!("- `{}`\n", format_review_revision(revision)));
        }
    }
    if !review.messages.is_empty() {
        out.push_str("\n## Messages\n\n");
        for message in &review.messages {
            out.push_str(&format!(
                "- **{}** {}{}: {}\n",
                markdown_cell(&message.message_type),
                markdown_cell(&message.author),
                message_location(message),
                markdown_cell(&message.body)
            ));
        }
    }
    out
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn show(args: ShowArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let branch_id = store.resolve_review_branch_id(args.review.as_deref(), Some(&vcs))?;
    let review = store.load_review(&branch_id)?;
    let all_tickets = store.list()?;
    let ref_lengths = open_ticket_ref_lengths(&all_tickets);
    let output = review_output(&store, &review, &all_tickets, &ref_lengths);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", review_markdown(&output));
        return Ok(());
    }

    println!("Review: {}", output.branch_id);
    println!("Branch: {}", output.branch);
    if let Some(ticket_ref) = output.ticket_ref.as_deref() {
        println!("Tickets: {ticket_ref}");
    }
    for (label, value) in [
        ("Title", output.title.as_str()),
        ("Description", output.description.as_str()),
        ("Status", output.status.as_str()),
        ("Approvals", output.approvals.as_str()),
    ] {
        if !value.is_empty() {
            println!("{label}: {value}");
        }
    }
    for (label, value) in [
        ("Base", output.base_sha.as_deref()),
        ("Head", output.head_sha.as_deref()),
        ("Integration", output.integration_sha.as_deref()),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            println!("{label}: {value}");
        }
    }
    if !output.reviewers.is_empty() {
        println!("Reviewers: {}", output.reviewers.join(", "));
    }

    if !output.revisions.is_empty() {
        println!("Revisions:");
        for entry in &output.revisions {
            println!("  {}", format_review_revision(entry));
        }
    }

    if !output.messages.is_empty() {
        println!("Messages:");
        for message in &output.messages {
            let location = message_location(message);
            println!(
                "  [{}] {}{}: {}",
                message.message_type, message.author, location, message.body
            );
        }
    }
    Ok(())
}

fn add_reviewer(args: AddReviewerArgs) -> Result<()> {
    let (review_arg, email) = split_optional_review_arg(args.args)?;
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let branch_id = store.resolve_review_branch_id(review_arg.as_deref(), Some(&vcs))?;
    store.add_reviewer(&branch_id, &email)?;
    println!("Added reviewer {email} to {branch_id}");
    Ok(())
}

fn comment(args: CommentArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let (review_arg, body) = split_review_and_body(&store, &vcs, args.args)?;
    let branch_id = store.resolve_review_branch_id(review_arg.as_deref(), Some(&vcs))?;
    let lines = args
        .lines
        .or_else(|| args.line.map(|line| line.to_string()));
    store.add_review_comment(
        &branch_id,
        ReviewMessage {
            author: store.email().to_string(),
            body,
            message_type: "comment".into(),
            commit: args.commit,
            path: args.path,
            lines,
            at: None,
        },
    )?;
    println!("Added comment to {branch_id}");
    Ok(())
}

fn approve(args: ApproveArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let (branch_id, commit) = match args.target.as_deref() {
        Some(target)
            if looks_like_sha(target)
                && store
                    .resolve_review_branch_id(Some(target), Some(&vcs))
                    .is_err() =>
        {
            (
                store.resolve_review_branch_id(None, Some(&vcs)).ok(),
                Some(target.to_string()),
            )
        }
        Some(target) => (
            Some(store.resolve_review_branch_id(Some(target), Some(&vcs))?),
            None,
        ),
        None => (
            Some(store.resolve_review_branch_id(None, Some(&vcs))?),
            None,
        ),
    };

    let comment_body = args.comment.unwrap_or_else(|| "Approved".into());
    if let Some(commit) = commit.as_deref() {
        store.approve_review_commit(branch_id.as_deref(), commit, &comment_body)?;
        if let Some(ref branch_id) = branch_id {
            println!("Approved {branch_id}");
        } else {
            println!("Approved commit {commit}");
        }
    } else if let Some(ref branch_id) = branch_id {
        store.approve_review(branch_id, &comment_body, None)?;
        println!("Approved {branch_id}");
    }
    Ok(())
}

fn request_changes(args: RequestChangesArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let (review_arg, body) = split_review_and_body(&store, &vcs, args.args)?;
    let branch_id = store.resolve_review_branch_id(review_arg.as_deref(), Some(&vcs))?;
    store.request_changes(
        &branch_id,
        ReviewMessage {
            author: store.email().to_string(),
            body,
            message_type: "changes-requested".into(),
            commit: None,
            path: None,
            lines: None,
            at: None,
        },
    )?;
    println!("Requested changes on {branch_id}");
    Ok(())
}

fn update(args: UpdateArgs) -> Result<()> {
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let branch_id = store.resolve_review_branch_id(args.review.as_deref(), Some(&vcs))?;
    let (_review, head) = store.update_review(&branch_id, args.head.as_deref(), &vcs)?;
    println!("Updated review {} to {}", branch_id, short_sha(&head));
    Ok(())
}

fn integrate(args: IntegrateArgs) -> Result<()> {
    let (review_arg, sha) = split_optional_review_arg(args.args)?;
    let store = open_store()?;
    let vcs = RealVcsAdapter::new();
    let branch_id = store.resolve_review_branch_id(review_arg.as_deref(), Some(&vcs))?;
    let resolved_sha = vcs.resolve_ref(&sha).unwrap_or(sha);
    store.integrate_review(&branch_id, &resolved_sha)?;
    println!("Integrated {} at {}", branch_id, short_sha(&resolved_sha));
    Ok(())
}

fn split_optional_review_arg(args: Vec<String>) -> Result<(Option<String>, String)> {
    match args.as_slice() {
        [value] => Ok((None, value.clone())),
        [review, value] => Ok((Some(review.clone()), value.clone())),
        _ => bail!("expected one or two arguments"),
    }
}

fn split_review_and_body(
    store: &ticgit_lib::TicketStore,
    vcs: &impl ReviewVcs,
    args: Vec<String>,
) -> Result<(Option<String>, String)> {
    if args.len() == 1 {
        return Ok((None, args[0].clone()));
    }

    if store
        .resolve_review_branch_id(Some(&args[0]), Some(vcs))
        .is_ok()
    {
        Ok((Some(args[0].clone()), args[1..].join(" ")))
    } else {
        Ok((None, args.join(" ")))
    }
}

fn looks_like_sha(value: &str) -> bool {
    value.len() >= 3 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn short_sha(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

fn message_location(message: &ReviewMessage) -> String {
    let mut pieces = Vec::new();
    if let Some(commit) = message.commit.as_deref() {
        pieces.push(short_sha(commit).to_string());
    }
    if let Some(path) = message.path.as_deref() {
        if let Some(lines) = message.lines.as_deref() {
            pieces.push(format!("{path}:{lines}"));
        } else {
            pieces.push(path.to_string());
        }
    }
    if pieces.is_empty() {
        String::new()
    } else {
        format!(" ({})", pieces.join(" "))
    }
}
