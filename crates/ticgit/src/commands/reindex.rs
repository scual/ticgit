use anyhow::{bail, Result};
use clap::Parser;

use crate::commands::open_store;

#[derive(Debug, Parser)]
pub struct Args {
    /// Replace the store with the remote tree (drops unsynced local changes,
    /// prunes ghost records); refuses while unsynced changes exist.
    #[arg(long = "reset")]
    pub reset: bool,

    /// With --reset: proceed even though unsynced local changes would be lost.
    #[arg(long = "force", requires = "reset")]
    pub force: bool,

    /// Output the result as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the result as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    // Best-effort: reindex from what the remote holds now. Offline, or with
    // no meta remote, it reads the refs the last sync fetched.
    let fetched = match store.fetch_remote_tip() {
        Ok(fetched) => fetched,
        Err(e) => {
            eprintln!(
                "warning: could not fetch the meta remote ({e}); using the last fetched refs"
            );
            false
        }
    };

    let tickets = if args.reset {
        if !args.force {
            let unsynced = store.unsynced_changes()?;
            if unsynced > 0 {
                bail!(
                    "refusing to reset: the local store holds {unsynced} unsynced change(s) \
                     the remote lacks, and --reset would drop them.\n\
                     Run `ti sync` first to publish them, or pass --force to discard them."
                );
            }
        }
        store.reindex_reset()?
    } else {
        store.reindex()?
    };

    if args.json {
        println!(
            "{}",
            serde_json::json!({ "tickets": tickets, "fetched": fetched, "reset": args.reset })
        );
    } else if args.markdown {
        print!("{}", reindex_markdown(tickets, fetched, args.reset));
    } else if args.reset {
        println!("Reset store from refs/meta/*: {tickets} ticket(s).");
    } else {
        println!("Reindexed store from refs/meta/*: {tickets} ticket(s).");
    }
    Ok(())
}

fn reindex_markdown(tickets: usize, fetched: bool, reset: bool) -> String {
    format!(
        "## Reindex\n\n- Mode: {}\n- Fetched the meta remote: {}\n- Tickets: {tickets}\n",
        if reset { "reset" } else { "additive" },
        if fetched { "yes" } else { "no" },
    )
}
