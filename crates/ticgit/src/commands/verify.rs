use anyhow::Result;
use clap::Parser;

use crate::commands::open_store;

#[derive(Debug, Parser)]
pub struct Args {
    /// Output the report as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the report as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let report = store.verify()?;
    let failed: Vec<_> = report.iter().filter(|o| !o.ok).collect();

    if args.json {
        let tickets: Vec<_> = report
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id,
                    "short_id": o.short_id,
                    "ok": o.ok,
                    "issues": o.issues,
                    "warnings": o.warnings,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "ok": failed.is_empty(),
                "total": report.len(),
                "failed": failed.len(),
                "tickets": tickets,
            })
        );
    } else if args.markdown {
        let unsigned: usize = report.iter().map(|o| o.warnings.len()).sum();
        println!("# Verify\n");
        println!("- Tickets checked: {}", report.len());
        println!("- Failed: {}", failed.len());
        println!("- Unsigned op warnings: {unsigned}\n");
        if failed.is_empty() {
            println!("All tickets consistent.");
        } else {
            for o in &failed {
                println!("## `{}`\n", o.short_id);
                for issue in &o.issues {
                    println!("- {issue}");
                }
            }
        }
    } else if failed.is_empty() {
        let unsigned: usize = report.iter().map(|o| o.warnings.len()).sum();
        if unsigned > 0 {
            println!(
                "All {} ticket(s) consistent ({unsigned} unsigned op(s) trusted by default).",
                report.len()
            );
        } else {
            println!("All {} ticket(s) consistent.", report.len());
        }
    } else {
        eprintln!(
            "{} of {} ticket(s) failed verification:",
            failed.len(),
            report.len()
        );
        for o in &failed {
            eprintln!("  {}", o.short_id);
            for issue in &o.issues {
                eprintln!("    - {issue}");
            }
        }
    }

    if !failed.is_empty() {
        anyhow::bail!("{} ticket(s) failed verification", failed.len());
    }
    Ok(())
}
