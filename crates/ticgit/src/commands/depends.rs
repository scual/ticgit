use anyhow::Result;
use clap::Parser;

use crate::commands::{open_store, resolve_ticket};
use crate::render;

#[derive(Debug, Parser)]
pub struct Args {
    /// The ticket that is depended on (the blocker).
    /// Current ticket depends on this ticket.
    pub dependency: Option<String>,

    /// Ticket to modify (defaults to checked-out ticket).
    #[arg(short = 't', long = "ticket")]
    pub ticket: Option<String>,

    /// Remove this dependency instead of adding it.
    #[arg(long = "remove")]
    pub remove: bool,

    /// Clear all dependencies from the ticket.
    #[arg(long = "clear", conflicts_with = "dependency")]
    pub clear: bool,

    /// Output as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let id = resolve_ticket(&store, args.ticket.as_deref())?;

    if args.clear {
        let ticket = store.load(&id)?;
        let deps: Vec<_> = ticket.depends_on.iter().copied().collect();
        for dep_id in deps {
            store.remove_dependency(&id, &dep_id)?;
        }
        let ticket = store.load(&id)?;
        return output(&ticket, None, args.json, args.markdown);
    }

    let dep_ref = args
        .dependency
        .ok_or_else(|| anyhow::anyhow!("dependency ticket id required (or use --clear)"))?;
    let dep_id = store.resolve_id(&dep_ref)?;

    if args.remove {
        store.remove_dependency(&id, &dep_id)?;
        let ticket = store.load(&id)?;
        let dep = store.load(&dep_id)?;
        if args.json || args.markdown {
            return output(&ticket, Some(&dep), args.json, args.markdown);
        }
        println!(
            "{} no longer depends on {}",
            ticket.short_id(),
            dep.short_id()
        );
        return Ok(());
    }

    store.add_dependency(&id, &dep_id)?;
    let ticket = store.load(&id)?;
    let dep = store.load(&dep_id)?;

    if args.json || args.markdown {
        return output(&ticket, Some(&dep), args.json, args.markdown);
    }

    println!(
        "{} ({}) now depends on {} ({})",
        ticket.short_id(),
        ticket.title,
        dep.short_id(),
        dep.title
    );
    Ok(())
}

fn output(
    ticket: &ticgit_lib::Ticket,
    dependency: Option<&ticgit_lib::Ticket>,
    json: bool,
    markdown: bool,
) -> Result<()> {
    if json {
        // Include the resolved counterpart so an agent sees both sides.
        let mut obj = serde_json::json!({ "ticket": serde_json::to_value(ticket)? });
        if let Some(dep) = dependency {
            obj["dependency"] = serde_json::to_value(dep)?;
        }
        println!("{}", serde_json::to_string_pretty(&obj)?);
    } else if markdown {
        println!("{}", render::ticket_markdown(ticket));
    }
    Ok(())
}
