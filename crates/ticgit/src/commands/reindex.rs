use anyhow::Result;
use clap::Parser;

use crate::commands::open_store;

#[derive(Debug, Parser)]
pub struct Args {
    /// Output the result as JSON.
    #[arg(long = "json")]
    pub json: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let tickets = store.reindex()?;

    if args.json {
        println!("{}", serde_json::json!({ "tickets": tickets }));
    } else {
        println!("Reindexed store from refs/meta/*: {tickets} ticket(s).");
    }
    Ok(())
}
