use std::io::{self, IsTerminal, Write};

use anyhow::{bail, Result};
use clap::Parser;
use uuid::Uuid;

use crate::commands::{open_store, SessionGitDir};
use crate::render;
use crate::session_state::State;
use ticgit_lib::Ticket;

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id(s) or prefix(es) to delete. At least one is required.
    #[arg(required = true)]
    pub ids: Vec<String>,

    /// Also delete all descendant sub-issues (the whole subtree).
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,

    /// Why the ticket(s) are deleted; recorded in the delete operation and
    /// shown by `ti history`.
    #[arg(long = "reason", value_name = "TEXT")]
    pub reason: Option<String>,

    /// Skip the confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Output the deleted tickets as a JSON array.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the deleted tickets as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;

    // Validate-then-act: resolve every id up front and bail before deleting if
    // any is unknown or an ambiguous prefix.
    let mut roots: Vec<Uuid> = Vec::new();
    for reference in &args.ids {
        roots.push(store.resolve_id(reference)?);
    }

    let all_tickets = store.list()?;
    for root in &roots {
        if all_tickets.iter().any(|t| t.id == *root && t.is_deleted()) {
            bail!(ticgit_lib::Error::AlreadyDeleted(*root));
        }
    }
    // Recursive expansion skips descendants that are already deleted.
    let targets: Vec<Uuid> = collect_targets(&roots, &all_tickets, args.recursive)
        .into_iter()
        .filter(|id| {
            roots.contains(id) || !all_tickets.iter().any(|t| t.id == *id && t.is_deleted())
        })
        .collect();

    // Snapshot ticket objects before deletion (for the confirmation prompt).
    let mut snapshots: Vec<Ticket> = Vec::new();
    for id in &targets {
        snapshots.push(store.load(id)?);
    }

    // Confirmation gating.
    let machine = args.json || args.markdown;
    if !args.yes {
        if machine {
            bail!("refusing to delete without --yes in machine mode (--json/--markdown)");
        }
        if !io::stdin().is_terminal() {
            bail!("refusing to delete without --yes (no interactive terminal)");
        }
        if !confirm(&snapshots)? {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Deletion is an operation: no keys are removed and no parent or
    // Dependency set on another ticket is touched (ADR 0003).
    let mut deleted: Vec<Ticket> = Vec::with_capacity(targets.len());
    for id in &targets {
        store.delete_ticket(id, args.reason.as_deref())?;
        deleted.push(store.load(id)?);
    }

    // Clear session state if a deleted id was the checked-out ticket.
    let git_dir = store.session().repo_git_dir();
    let mut state = State::load().unwrap_or_default();
    if let Some(current) = state.current_for(&git_dir) {
        if targets.contains(&current) {
            state.clear_current(&git_dir);
            state.save()?;
        }
    }

    if args.json {
        println!("{}", render::tickets_json(&deleted)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", deleted_markdown(&deleted));
        return Ok(());
    }
    println!("Deleted {} ticket(s):", deleted.len());
    for t in &deleted {
        println!("  {} — {}", t.short_id(), t.title);
    }
    println!("Restore with `ti undelete <id>`.");
    Ok(())
}

/// Collect the deduped, ordered set of ids to delete. Without `recursive`, this
/// is just the roots (deduped). With `recursive`, each root is expanded into its
/// full descendant subtree (including closed descendants), guarding cycles and
/// skipping dangling child references.
fn collect_targets(roots: &[Uuid], all: &[Ticket], recursive: bool) -> Vec<Uuid> {
    use std::collections::{HashMap, HashSet};
    let by_id: HashMap<Uuid, &Ticket> = all.iter().map(|t| (t.id, t)).collect();
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut ordered: Vec<Uuid> = Vec::new();
    for root in roots {
        collect_one(*root, &by_id, recursive, &mut seen, &mut ordered);
    }
    ordered
}

fn collect_one(
    id: Uuid,
    by_id: &std::collections::HashMap<Uuid, &Ticket>,
    recursive: bool,
    seen: &mut std::collections::HashSet<Uuid>,
    ordered: &mut Vec<Uuid>,
) {
    if !seen.insert(id) {
        return;
    }
    ordered.push(id);
    if recursive {
        if let Some(t) = by_id.get(&id) {
            for child in &t.children {
                if by_id.contains_key(child) {
                    collect_one(*child, by_id, recursive, seen, ordered);
                }
            }
        }
    }
}

fn confirm(snaps: &[Ticket]) -> Result<bool> {
    eprintln!("About to delete {} ticket(s):", snaps.len());
    for t in snaps {
        eprintln!("  {} — {} ({})", t.short_id(), t.title, t.state.as_str());
    }
    eprint!("Delete {} ticket(s)? [y/N] ", snaps.len());
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn deleted_markdown(snaps: &[Ticket]) -> String {
    let mut out = String::from("# Deleted tickets\n\n");
    for t in snaps {
        out.push_str(&format!("- {} — {}\n", t.short_id(), t.title));
    }
    out
}
