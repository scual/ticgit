use anyhow::Result;
use clap::Parser;

use crate::commands::{open_store, SessionGitDir};
use crate::render;
use crate::session_state::State;

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id (or unique prefix) to mark current.
    pub ticket: Option<String>,

    /// Clear the currently-checked-out ticket.
    #[arg(short = 'c', long = "clear", conflicts_with = "ticket")]
    pub clear: bool,

    /// Output the checked-out ticket as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the checked-out ticket as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let git_dir = store.session().repo_git_dir();
    let mut state = State::load().unwrap_or_default();

    if args.clear {
        state.clear_current(&git_dir);
        state.save()?;
        if args.json {
            println!("{}", serde_json::json!({ "current": null }));
            return Ok(());
        }
        if args.markdown {
            println!("{}", render::checkout_clear_markdown());
            return Ok(());
        }
        println!("Cleared current ticket.");
        return Ok(());
    }

    let reference = args
        .ticket
        .ok_or_else(|| anyhow::anyhow!("ticket id (or prefix) required"))?;
    let id = store.resolve_id(&reference)?;
    let ticket = store.load(&id)?;
    if ticket.is_deleted() {
        anyhow::bail!(
            "ticket {} is deleted; run `ti undelete {}` first to work on it",
            ticket.short_id(),
            ticket.short_id()
        );
    }
    state.set_current(&git_dir, id);
    state.save()?;
    if args.json {
        println!("{}", render::ticket_json(&ticket)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(&ticket));
        return Ok(());
    }
    println!("Checked out: {} - {}", ticket.short_id(), ticket.title);
    Ok(())
}
