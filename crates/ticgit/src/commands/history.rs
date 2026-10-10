use anyhow::Result;
use clap::Parser;
use ticgit_lib::{HistoryAction, HistoryEntry};

use crate::commands::{open_store, resolve_ticket};

#[derive(Debug, Parser)]
pub struct Args {
    /// Ticket id (or prefix). Defaults to the currently checked-out ticket.
    #[arg(short = 't', long = "ticket")]
    pub ticket: Option<String>,

    /// Maximum number of entries to show.
    #[arg(short = 'n', long = "limit")]
    pub limit: Option<usize>,

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
    let mut entries = store.history(&id)?;
    entries.truncate(args.limit.unwrap_or(100));

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if args.markdown {
        print_markdown(&id.to_string(), &entries);
        return Ok(());
    }

    print_terminal(&entries);
    Ok(())
}

fn action_label(e: &HistoryEntry) -> &'static str {
    match e.action {
        HistoryAction::Created => "created",
        HistoryAction::Set => "set",
        HistoryAction::Cleared => "cleared",
        HistoryAction::Commented => "commented",
    }
}

fn display_value(e: &HistoryEntry, max: usize) -> String {
    let raw = e.value.as_deref().unwrap_or("");
    let one_line = raw.lines().next().unwrap_or("");
    if one_line.chars().count() > max || raw.lines().count() > 1 {
        let cut: String = one_line.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    } else {
        one_line.to_string()
    }
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";

fn print_terminal(entries: &[HistoryEntry]) {
    if entries.is_empty() {
        println!("(no history)");
        return;
    }

    let mut last_date = String::new();
    for e in entries {
        let date = format!(
            "{:04}-{:02}-{:02}",
            e.at.year(),
            u8::from(e.at.month()),
            e.at.day()
        );
        let time = format!("{:02}:{:02}", e.at.hour(), e.at.minute());

        if date != last_date {
            println!("\n{}{}{}", ANSI_DIM, date, ANSI_RESET);
            last_date = date;
        }

        let verb = match e.action {
            HistoryAction::Created | HistoryAction::Set | HistoryAction::Commented => {
                format!("{}{}{}", ANSI_GREEN, action_label(e), ANSI_RESET)
            }
            HistoryAction::Cleared => format!("{}{}{}", ANSI_YELLOW, action_label(e), ANSI_RESET),
        };

        let short_email = e.by.split('@').next().unwrap_or(&e.by);

        let value_display = display_value(e, 60);

        println!(
            "  {}{}{} {} {}{}{} → {}{}{}  {}{}{}",
            ANSI_DIM,
            time,
            ANSI_RESET,
            verb,
            ANSI_CYAN,
            e.field,
            ANSI_RESET,
            ANSI_YELLOW,
            value_display,
            ANSI_RESET,
            ANSI_DIM,
            short_email,
            ANSI_RESET,
        );
    }
    println!();
}

fn print_markdown(ticket_id: &str, entries: &[HistoryEntry]) {
    let short: String = ticket_id.chars().take(6).collect();
    println!("# History: {}\n", short);

    if entries.is_empty() {
        println!("_No history._");
        return;
    }

    println!("| Time | Action | Field | Value | By |");
    println!("| --- | --- | --- | --- | --- |");
    for e in entries {
        let dt = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            e.at.year(),
            u8::from(e.at.month()),
            e.at.day(),
            e.at.hour(),
            e.at.minute()
        );
        let value_display = display_value(e, 50);
        let short_email = e.by.split('@').next().unwrap_or(&e.by);
        println!(
            "| {} | {} | `{}` | {} | {} |",
            dt,
            action_label(e),
            e.field,
            value_display.replace('|', "\\|"),
            short_email
        );
    }
}
