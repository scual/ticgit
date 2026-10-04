use anyhow::Result;
use clap::Parser;

use crate::commands::open_store;

#[derive(Debug, Parser)]
pub struct Args {
    /// Apply the migration, persisting version markers. Without this flag the
    /// command is a dry run that reports what would change but mutates nothing.
    #[arg(long = "write")]
    pub write: bool,

    /// Output the plan as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the plan as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let plan = store.migrate(args.write)?;
    let current = ticgit_lib::CURRENT_TICKET_FORMAT;
    let changed: Vec<_> = plan.iter().filter(|o| o.changed).collect();

    if args.json {
        let tickets: Vec<_> = plan
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id,
                    "short_id": o.short_id,
                    "from": o.from,
                    "to": o.to,
                    "changed": o.changed,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "applied": args.write,
                "current_format": current,
                "total": plan.len(),
                "changed": changed.len(),
                "tickets": tickets,
            })
        );
        return Ok(());
    }

    if args.markdown {
        println!("# Migration plan\n");
        println!("- Mode: {}", if args.write { "write" } else { "dry-run" });
        println!("- Target format version: {current}");
        println!("- Tickets: {}", plan.len());
        println!("- Needing change: {}\n", changed.len());
        if !changed.is_empty() {
            println!("| Ticket | From | To |");
            println!("| --- | --- | --- |");
            for o in &changed {
                let from = o
                    .from
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unversioned".to_string());
                println!("| `{}` | {} | {} |", o.short_id, from, o.to);
            }
        }
        return Ok(());
    }

    if changed.is_empty() {
        println!(
            "All {} ticket(s) already at format version {current}.",
            plan.len()
        );
    } else if args.write {
        println!(
            "Migrated {} of {} ticket(s) to format version {current}.",
            changed.len(),
            plan.len()
        );
    } else {
        println!(
            "{} of {} ticket(s) need migration to format version {current} \
             (run `ti migrate --write` to apply):",
            changed.len(),
            plan.len()
        );
        for o in &changed {
            let from = o
                .from
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unversioned".to_string());
            println!("  {} {} -> {}", o.short_id, from, o.to);
        }
    }
    Ok(())
}
