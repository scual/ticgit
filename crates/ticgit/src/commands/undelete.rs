use anyhow::Result;
use clap::Parser;

use crate::commands::open_store;
use crate::render;

#[derive(Debug, Parser)]
pub struct Args {
    /// Id (or unique prefix) of the deleted ticket to restore.
    #[arg(value_name = "TICKET")]
    pub ticket: String,

    /// Output the restored ticket as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the restored ticket as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

/// Undo a Deletion: append an `undel` operation. The ticket comes back with
/// its comments, tags, Sub-issues and Dependencies exactly as they were,
/// because `ti delete` never removed them.
pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let id = store.resolve_id(&args.ticket)?;
    store.undelete_ticket(&id)?;
    let ticket = store.load(&id)?;

    if args.json {
        println!("{}", render::ticket_json(&ticket)?);
        return Ok(());
    }
    if args.markdown {
        println!("{}", render::ticket_markdown(&ticket));
        return Ok(());
    }
    println!("Undeleted: {} - {}", ticket.short_id(), ticket.title);
    Ok(())
}
