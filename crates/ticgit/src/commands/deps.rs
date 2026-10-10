use anyhow::Result;
use clap::Parser;
use ticgit_lib::{dependency_tree, DepDirection};

use crate::commands::{open_store, resolve_ticket};
use crate::render;

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id, prefix, or `@`. Defaults to the checked-out ticket.
    pub ticket: Option<String>,

    /// Ticket id (or prefix) via a flag instead of positionally.
    #[arg(short = 't', long = "ticket", conflicts_with = "ticket")]
    pub ticket_flag: Option<String>,

    /// Show the dependents tree (downstream: what finishing this unblocks)
    /// instead of the default blockers tree (upstream: what must finish first).
    #[arg(long = "dependents")]
    pub dependents: bool,

    /// Show both the blockers and the dependents tree.
    #[arg(long = "both")]
    pub both: bool,

    /// Walk through closed tickets too (closed nodes are pruned by default).
    #[arg(long = "all")]
    pub all: bool,

    /// Output as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let reference = args.ticket.as_deref().or(args.ticket_flag.as_deref());
    let id = resolve_ticket(&store, reference)?;
    let tickets = store.list()?;

    // Which directions to render, in display order.
    let directions: &[DepDirection] = if args.both {
        &[DepDirection::Blockers, DepDirection::Dependents]
    } else if args.dependents {
        &[DepDirection::Dependents]
    } else {
        &[DepDirection::Blockers]
    };

    let blocks: Vec<Block> = directions
        .iter()
        .map(|&direction| Block {
            direction,
            nodes: dependency_tree(&tickets, id, direction, args.all),
        })
        .collect();

    if args.json {
        let payload = if args.both {
            serde_json::Value::Array(blocks.iter().map(|b| b.to_json(&id)).collect())
        } else {
            blocks[0].to_json(&id)
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if args.markdown {
        for b in &blocks {
            println!("## {} of {}\n", b.heading(), short(&id));
            let body = render::dep_tree_markdown(&b.nodes);
            if body.is_empty() {
                println!("_{}_\n", b.empty_note());
            } else {
                print!("{body}");
                println!();
            }
        }
        return Ok(());
    }

    for b in &blocks {
        println!("{} of {}:", b.heading(), short(&id));
        let body = render::dep_tree_text(&b.nodes);
        if body.is_empty() {
            println!("  ({})", b.empty_note());
        } else {
            print!("{body}");
        }
        println!();
    }
    Ok(())
}

struct Block {
    direction: DepDirection,
    nodes: Vec<ticgit_lib::DepNode>,
}

impl Block {
    fn key(&self) -> &'static str {
        match self.direction {
            DepDirection::Blockers => "blockers",
            DepDirection::Dependents => "dependents",
        }
    }

    fn heading(&self) -> &'static str {
        match self.direction {
            DepDirection::Blockers => "Blockers",
            DepDirection::Dependents => "Dependents",
        }
    }

    fn empty_note(&self) -> &'static str {
        match self.direction {
            DepDirection::Blockers => "no blockers",
            DepDirection::Dependents => "no dependents",
        }
    }

    fn to_json(&self, id: &uuid::Uuid) -> serde_json::Value {
        serde_json::json!({
            "ticket": id.to_string(),
            "direction": self.key(),
            "nodes": render::dep_tree_json(&self.nodes),
        })
    }
}

fn short(id: &uuid::Uuid) -> String {
    id.to_string().chars().take(6).collect()
}
