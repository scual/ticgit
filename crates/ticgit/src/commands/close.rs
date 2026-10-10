use anyhow::Result;
use clap::Parser;
use ticgit_lib::{TicketState, TicketStatus};

use crate::commands::{apply_lifecycle, open_store, resolve_ticket, SessionGitDir};
use crate::render;
use crate::session_state::State;

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id (or prefix). Defaults to the currently checked-out ticket.
    #[arg(value_name = "TICKET")]
    pub ticket: Option<String>,

    /// Ticket id (or prefix); same as the positional argument, matching `ti state -t`.
    #[arg(
        short = 't',
        long = "ticket",
        value_name = "TICKET",
        conflicts_with = "ticket"
    )]
    pub ticket_flag: Option<String>,

    /// Output the updated ticket as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the updated ticket as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,

    /// Close even if the ticket still has open sub-issues or open dependencies.
    #[arg(long = "force")]
    pub force: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let id = resolve_ticket(
        &store,
        args.ticket_flag.as_deref().or(args.ticket.as_deref()),
    )?;
    apply_lifecycle(
        &store,
        &id,
        TicketStatus::Closed,
        TicketState::Resolved,
        args.force,
    )?;

    let git_dir = store.session().repo_git_dir();
    let mut state = State::load().unwrap_or_default();
    let cleared_current = state.current_for(&git_dir) == Some(id);
    if cleared_current {
        state.clear_current(&git_dir);
        state.save()?;
    }

    let ticket = store.load(&id)?;
    if args.json {
        println!("{}", render::ticket_json(&ticket)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(&ticket));
        return Ok(());
    }

    if cleared_current {
        println!("Closed {} and cleared current ticket.", ticket.short_id());
    } else {
        println!("Closed {}.", ticket.short_id());
    }
    Ok(())
}
