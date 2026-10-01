use anyhow::Result;
use clap::Parser;
use ticgit_lib::{next_queue, NextOptions};

use crate::commands::{open_store, SessionGitDir};
use crate::render;
use crate::session_state::State;

/// Pick the next ticket to work on.
///
/// Excludes closed tickets, sub-issues, tickets with unresolved dependencies,
/// and tickets tagged `deferred`/`backlog` (restore with `--include-deferred`).
///
/// Ordering (first = work on next): priority ascending — lower is more
/// important, and unprioritised (`none`) tickets always sort last, so a numeric
/// priority (even a large one) cannot sink below them; then state
/// (in-progress, assigned, review, new, then blocked last); then oldest-created.
#[derive(Debug, Parser)]
pub struct Args {
    /// Only consider tickets with this tag.
    #[arg(short = 'g', long = "tag")]
    pub tag: Option<String>,

    /// Only consider tickets assigned to this user.
    #[arg(short = 'a', long = "assigned")]
    pub assigned: Option<String>,

    /// Include tickets tagged `deferred` or `backlog` (hidden by default).
    #[arg(long = "include-deferred")]
    pub include_deferred: bool,

    /// Output as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let git_dir = store.session().repo_git_dir();
    let all_tickets = store.list()?;

    let by_id = render::by_id_map(&all_tickets);

    let opts = NextOptions {
        tag: args.tag.clone(),
        assigned: args.assigned.clone(),
        include_deferred: args.include_deferred,
    };
    let candidates = next_queue(&all_tickets, &opts);

    let ticket = match candidates.into_iter().next() {
        Some(t) => t,
        None => {
            if args.json {
                println!("{}", serde_json::json!({ "next": null }));
            } else if args.markdown {
                println!("# Next Ticket\n\nNo open tickets match the criteria.");
            } else {
                println!("No open tickets to work on.");
            }
            return Ok(());
        }
    };

    // Check it out
    let mut state = State::load().unwrap_or_default();
    state.set_current(&git_dir, ticket.id);
    state.save()?;

    if args.json {
        println!("{}", render::ticket_json_with_subissues(ticket, &by_id)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(ticket));
        let tree = render::build_subissue_tree(ticket, &by_id);
        if !tree.is_empty() {
            println!("## Sub-issues\n\n{}", render::subissue_tree_markdown(&tree));
        }
        return Ok(());
    }

    println!("Next: {} - {}", ticket.short_id(), ticket.title);
    println!(
        "  State: {}  Priority: {}  Points: {}",
        ticket.state.as_str(),
        ticket
            .priority
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into()),
        ticket
            .points
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into()),
    );
    if let Some(a) = &ticket.assigned {
        println!("  Assigned: {a}");
    }
    if !ticket.tags.is_empty() {
        let tags: Vec<_> = ticket.tags.iter().cloned().collect();
        println!("  Tags: {}", tags.join(", "));
    }
    println!("Checked out.");

    let tree = render::build_subissue_tree(ticket, &by_id);
    if !tree.is_empty() {
        println!("Sub-issues:");
        print!("{}", render::subissue_tree_text(&tree));
    }
    Ok(())
}
