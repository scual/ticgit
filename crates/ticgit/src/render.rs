//! Terminal output: tables, single-ticket details, JSON, and Markdown.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use ticgit_lib::Ticket;
use ticgit_lib::TicketStatus;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use uuid::Uuid;

use crate::timefmt::relative_time;

/// Mapping of email → nick for display purposes.
pub type NickMap = HashMap<String, String>;

/// A short summary of a ticket, used to resolve relationship ids (parent,
/// children, depends_on, blocks) into something readable in `ti show`.
pub struct RelSummary {
    pub title: String,
    pub status: String,
}

/// Lookup from ticket id to its [`RelSummary`], built from the full ticket list.
pub type RelLookup = HashMap<Uuid, RelSummary>;

/// Build a [`RelLookup`] from every ticket so relationship ids can be rendered
/// with their title and status instead of a bare hex prefix.
pub fn build_rel_lookup(tickets: &[Ticket]) -> RelLookup {
    tickets
        .iter()
        .map(|t| {
            (
                t.id,
                RelSummary {
                    title: t.title.clone(),
                    status: t.status.as_str().to_string(),
                },
            )
        })
        .collect()
}

fn short_hex(id: &Uuid) -> String {
    id.to_string().chars().take(6).collect()
}

/// Format related ticket ids as a comma-separated list. With a lookup each
/// entry becomes `shortid "title" [status]`; without one, just `shortid`.
fn format_related<'a>(
    ids: impl Iterator<Item = &'a Uuid>,
    rels: Option<&RelLookup>,
) -> String {
    ids.map(|id| {
        let short = short_hex(id);
        match rels.and_then(|r| r.get(id)) {
            Some(info) => format!("{short} \"{}\" [{}]", flatten(&info.title), info.status),
            None => short,
        }
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// Build a NickMap from the user list returned by `TicketStore::list_users`.
pub fn build_nick_map(users: &BTreeMap<String, BTreeSet<String>>) -> NickMap {
    let mut map = HashMap::new();
    for (nick, emails) in users {
        for email in emails {
            map.insert(email.clone(), nick.clone());
        }
    }
    map
}

/// Resolve an email to its nick for display, or return the email as-is.
pub fn display_name(email: &str, nicks: Option<&NickMap>) -> String {
    if let Some(map) = nicks {
        if let Some(nick) = map.get(email) {
            return nick.clone();
        }
    }
    email.to_string()
}

/// Like `assigned_short()` but prefers nick if available.
fn display_assigned_short(assigned: Option<&str>, nicks: Option<&NickMap>) -> String {
    match assigned {
        Some(email) => {
            if let Some(map) = nicks {
                if let Some(nick) = map.get(email) {
                    return nick.clone();
                }
            }
            // fallback: local part of email
            email
                .split_once('@')
                .map(|(local, _)| local)
                .unwrap_or(email)
                .to_string()
        }
        None => String::new(),
    }
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_BLUE: &str = "\x1b[34m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_PURPLE: &str = "\x1b[35m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_CYAN: &str = "\x1b[36m";

/// Render a list of tickets as a compact table. `current` (if any) gets a `*`.
pub fn tickets_table(tickets: &[Ticket], current: Option<&uuid::Uuid>) -> String {
    let ref_lengths = open_ticket_ref_lengths(tickets);
    tickets_table_with_refs(tickets, current, &ref_lengths, None)
}

/// Render a list with caller-provided open-ticket short reference lengths.
pub fn tickets_table_with_refs(
    tickets: &[Ticket],
    current: Option<&uuid::Uuid>,
    ref_lengths: &BTreeMap<uuid::Uuid, usize>,
    nicks: Option<&NickMap>,
) -> String {
    let width = crossterm::terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(100)
        .max(40);
    tickets_table_with_width(
        tickets,
        current,
        ref_lengths,
        width,
        OffsetDateTime::now_utc(),
        nicks,
    )
}

#[derive(Debug, Clone)]
pub struct ReviewTableRow {
    pub ticket: Option<String>,
    pub ticket_unique_chars: usize,
    pub branch: String,
    pub approvals: String,
    pub status: String,
    pub title: String,
}

pub fn reviews_table(rows: &[ReviewTableRow]) -> String {
    let width = crossterm::terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(100)
        .max(40);
    reviews_table_with_width(rows, width)
}

fn reviews_table_with_width(rows: &[ReviewTableRow], width: usize) -> String {
    let width = width.saturating_sub(1).max(1);
    const TICKET_WIDTH: usize = 6;
    const REVIEW_WIDTH: usize = 5;
    const STATUS_WIDTH: usize = 8;
    const MIN_BRANCH_WIDTH: usize = 12;
    const MAX_BRANCH_WIDTH: usize = 28;
    const MIN_TITLE_WIDTH: usize = 12;

    let natural_branch_width = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(flatten(&row.branch).as_str()))
        .max()
        .unwrap_or(MIN_BRANCH_WIDTH)
        .clamp(MIN_BRANCH_WIDTH, MAX_BRANCH_WIDTH);
    let fixed_without_title =
        2 + TICKET_WIDTH + 1 + natural_branch_width + 1 + REVIEW_WIDTH + 1 + STATUS_WIDTH + 1;
    let (branch_width, title_width) = if fixed_without_title + MIN_TITLE_WIDTH <= width {
        (
            natural_branch_width,
            width
                .saturating_sub(fixed_without_title)
                .max(MIN_TITLE_WIDTH),
        )
    } else {
        let branch_width = width
            .saturating_sub(
                2 + TICKET_WIDTH + 1 + 1 + REVIEW_WIDTH + 1 + STATUS_WIDTH + 1 + MIN_TITLE_WIDTH,
            )
            .clamp(MIN_BRANCH_WIDTH.min(width), natural_branch_width);
        let fixed_without_title =
            2 + TICKET_WIDTH + 1 + branch_width + 1 + REVIEW_WIDTH + 1 + STATUS_WIDTH + 1;
        (
            branch_width,
            width
                .saturating_sub(fixed_without_title)
                .max(MIN_TITLE_WIDTH),
        )
    };

    let mut out = String::new();
    let header = format!(
        "  {} {} {} {} {}",
        fit("TicId", TICKET_WIDTH),
        fit("Branch", branch_width),
        fit("Rv", REVIEW_WIDTH),
        fit("Status", STATUS_WIDTH),
        fit("Title", title_width),
    );
    out.push_str(&ansi(ANSI_DIM, &header));
    out.push('\n');
    out.push_str(&ansi(ANSI_DIM, &"-".repeat(width)));
    out.push('\n');

    for row in rows {
        out.push_str("  ");
        out.push_str(&styled_review_ticket(row, TICKET_WIDTH));
        out.push(' ');
        out.push_str(&ansi(ANSI_CYAN, &fit(&flatten(&row.branch), branch_width)));
        out.push(' ');
        out.push_str(&ansi(
            review_progress_color(&row.approvals),
            &fit(&row.approvals, REVIEW_WIDTH),
        ));
        out.push(' ');
        out.push_str(&ansi(
            review_status_color(&row.status),
            &fit(review_status_label(&row.status), STATUS_WIDTH),
        ));
        out.push(' ');
        out.push_str(&ansi(ANSI_BLUE, &fit(&flatten(&row.title), title_width)));
        out.push('\n');
    }

    out
}

fn styled_review_ticket(row: &ReviewTableRow, width: usize) -> String {
    let Some(ticket) = row.ticket.as_deref() else {
        return ansi(ANSI_CYAN, &fit("", width));
    };
    let unique_len = row.ticket_unique_chars.min(ticket.len());
    let visible = truncate_display(ticket, width);
    let (unique, rest) = visible.split_at(unique_len.min(visible.len()));
    format!(
        "{}{}{}",
        ansi(ANSI_YELLOW, unique),
        ansi(ANSI_CYAN, rest),
        " ".repeat(width.saturating_sub(UnicodeWidthStr::width(visible.as_str())))
    )
}

pub fn open_ticket_ref_lengths(tickets: &[Ticket]) -> BTreeMap<uuid::Uuid, usize> {
    let open_hexes: Vec<_> = tickets
        .iter()
        .filter(|ticket| ticket.status == TicketStatus::Open)
        .map(|ticket| (ticket.id, ticket.id.to_string().replace('-', "")))
        .collect();

    open_hexes
        .iter()
        .map(|(id, hex)| {
            let length = (1..=hex.len())
                .find(|length| {
                    let prefix = &hex[..*length];
                    open_hexes
                        .iter()
                        .filter(|(_, other)| other.starts_with(prefix))
                        .count()
                        == 1
                })
                .unwrap_or(hex.len());
            (*id, length)
        })
        .collect()
}

fn tickets_table_with_width(
    tickets: &[Ticket],
    current: Option<&uuid::Uuid>,
    ref_lengths: &BTreeMap<uuid::Uuid, usize>,
    width: usize,
    now: OffsetDateTime,
    nicks: Option<&NickMap>,
) -> String {
    let width = width.saturating_sub(1).max(1);
    let id_width = ref_lengths.values().copied().max().unwrap_or(6).max(6);
    const STATUS_WIDTH: usize = 6;
    const STATE_WIDTH: usize = 11;
    const DATE_WIDTH: usize = 3;
    const PRIORITY_WIDTH: usize = 1;
    const ASSIGNED_WIDTH: usize = 8;
    const TAGS_WIDTH: usize = 20;
    const MIN_TITLE_WIDTH: usize = 12;

    let layout = TableLayout::new(
        width,
        id_width,
        DATE_WIDTH,
        STATUS_WIDTH,
        STATE_WIDTH,
        PRIORITY_WIDTH,
        ASSIGNED_WIDTH,
        TAGS_WIDTH,
        MIN_TITLE_WIDTH,
    );

    let mut out = String::new();
    let mut header = format!("  {} {} ", fit("TicId", id_width), fit("Dt", DATE_WIDTH),);
    if layout.show_priority {
        header.push_str(&fit("P", PRIORITY_WIDTH));
        header.push(' ');
    }
    header.push_str(&format!(
        " {} {} {}",
        fit("Title", layout.title_width),
        fit("Status", STATUS_WIDTH),
        fit("State", STATE_WIDTH)
    ));
    if layout.show_assigned {
        header.push(' ');
        header.push_str(&fit("Assgn", ASSIGNED_WIDTH));
    }
    if layout.show_tags {
        header.push(' ');
        header.push_str(&fit("Tags", TAGS_WIDTH));
    }
    out.push_str(&ansi(ANSI_DIM, &header));
    out.push('\n');
    out.push_str(&ansi(ANSI_DIM, &"-".repeat(width)));
    out.push('\n');

    for t in tickets {
        let marker = if Some(&t.id) == current { "*" } else { " " };
        let assigned = display_assigned_short(t.assigned.as_deref(), nicks);
        let tags = t.tags.iter().cloned().collect::<Vec<_>>().join(",");
        out.push_str(marker);
        out.push(' ');
        out.push_str(&styled_ticket_id(t, id_width, ref_lengths));
        out.push(' ');
        out.push_str(&ansi(
            ANSI_DIM,
            &fit(&compact_relative_time(t.created_at, now), DATE_WIDTH),
        ));
        out.push(' ');
        if layout.show_priority {
            let priority = t
                .priority
                .map(|value| value.to_string())
                .unwrap_or_default();
            out.push_str(&ansi(ANSI_PURPLE, &fit(&priority, PRIORITY_WIDTH)));
            out.push(' ');
        }
        if t.children.is_empty() {
            out.push_str(&ansi(
                ANSI_BLUE,
                &fit(&flatten(&t.title), layout.title_width),
            ));
        } else {
            let suffix = format!(" [+{}]", t.children.len());
            let avail = layout.title_width.saturating_sub(suffix.len());
            out.push_str(&ansi(ANSI_BLUE, &fit(&flatten(&t.title), avail)));
            out.push_str(&ansi(ANSI_DIM, &fit(&suffix, suffix.len())));
        }
        out.push(' ');
        out.push_str(&ansi(
            status_color(t.status.as_str()),
            &fit(t.status.as_str(), STATUS_WIDTH),
        ));
        out.push(' ');
        out.push_str(&ansi(
            state_color(t.state.as_str()),
            &fit(t.state.as_str(), STATE_WIDTH),
        ));
        if layout.show_assigned {
            out.push_str(&fit(&flatten(&assigned), ASSIGNED_WIDTH));
        }
        if layout.show_tags {
            out.push(' ');
            out.push_str(&ansi(ANSI_YELLOW, &fit(&flatten(&tags), TAGS_WIDTH)));
        }
        out.push('\n');
    }

    out
}

struct TableLayout {
    title_width: usize,
    show_priority: bool,
    show_assigned: bool,
    show_tags: bool,
}

impl TableLayout {
    #[allow(clippy::too_many_arguments)]
    fn new(
        width: usize,
        id_width: usize,
        date_width: usize,
        status_width: usize,
        state_width: usize,
        priority_width: usize,
        assigned_width: usize,
        tags_width: usize,
        min_title_width: usize,
    ) -> Self {
        let mut layout = Self {
            title_width: min_title_width,
            show_priority: true,
            show_assigned: true,
            show_tags: true,
        };

        while layout.fixed_width_without_title(
            id_width,
            date_width,
            status_width,
            state_width,
            priority_width,
            assigned_width,
            tags_width,
        ) + min_title_width
            > width
        {
            if layout.show_tags {
                layout.show_tags = false;
            } else if layout.show_assigned {
                layout.show_assigned = false;
            } else if layout.show_priority {
                layout.show_priority = false;
            } else {
                break;
            }
        }

        let fixed = layout.fixed_width_without_title(
            id_width,
            date_width,
            status_width,
            state_width,
            priority_width,
            assigned_width,
            tags_width,
        );
        layout.title_width = width.saturating_sub(fixed).max(min_title_width);
        layout
    }

    // Cohesive column-width layout inputs; a struct would only add indirection.
    #[allow(clippy::too_many_arguments)]
    fn fixed_width_without_title(
        &self,
        id_width: usize,
        date_width: usize,
        status_width: usize,
        state_width: usize,
        priority_width: usize,
        assigned_width: usize,
        tags_width: usize,
    ) -> usize {
        let marker_and_required_spacing = 1 + 1 + 1 + 2 + 1 + 1;
        let mut width =
            marker_and_required_spacing + id_width + date_width + status_width + state_width;
        if self.show_priority {
            width += 1 + priority_width;
        }
        if self.show_assigned {
            width += assigned_width;
        }
        if self.show_tags {
            width += 1 + tags_width;
        }
        width
    }
}

/// Render a single ticket and its comments, resolving emails to nicks.
pub fn ticket_detail(t: &Ticket, nicks: Option<&NickMap>, rels: Option<&RelLookup>) -> String {
    let mut out = String::new();
    let title_bar = "-".repeat(t.title.chars().count().max(20));
    out.push_str(&ansi(ANSI_DIM, &title_bar));
    out.push('\n');
    out.push_str(&detail_field("Title", &ansi(ANSI_BLUE, &t.title)));
    out.push_str(&detail_field("Id", &ansi(ANSI_CYAN, &t.id.to_string())));
    out.push_str(&detail_field(
        "Created",
        &ansi(
            ANSI_DIM,
            &format!(
                "{} ({})  by {}",
                friendly_date(t.created_at),
                relative_time(t.created_at, OffsetDateTime::now_utc()),
                display_name(&t.created_by, nicks)
            ),
        ),
    ));
    out.push_str(&detail_field(
        "Status",
        &ansi(status_color(t.status.as_str()), t.status.as_str()),
    ));
    out.push_str(&detail_field(
        "State",
        &ansi(state_color(t.state.as_str()), t.state.as_str()),
    ));
    if let Some(a) = &t.assigned {
        out.push_str(&detail_field("Assigned", &display_name(a, nicks)));
    }
    if let Some(p) = t.priority {
        out.push_str(&detail_field("Priority", &p.to_string()));
    }
    if let Some(p) = t.points {
        out.push_str(&detail_field("Points", &p.to_string()));
    }
    if let Some(m) = &t.milestone {
        out.push_str(&detail_field("Milestone", m));
    }
    if let Some(code) = &t.code {
        out.push_str(&detail_field("Code", &ansi(ANSI_CYAN, code)));
    }
    if let Some(parent_id) = &t.parent {
        let value = format_related(std::iter::once(parent_id), rels);
        out.push_str(&detail_field("Parent", &ansi(ANSI_CYAN, &value)));
    }
    if !t.children.is_empty() {
        let value = format_related(t.children.iter(), rels);
        out.push_str(&detail_field("Children", &ansi(ANSI_CYAN, &value)));
    }
    if !t.depends_on.is_empty() {
        let value = format_related(t.depends_on.iter(), rels);
        out.push_str(&detail_field("Depends", &ansi(ANSI_CYAN, &value)));
    }
    if !t.blocks.is_empty() {
        let value = format_related(t.blocks.iter(), rels);
        out.push_str(&detail_field("Blocks", &ansi(ANSI_CYAN, &value)));
    }
    if let Some(spec) = &t.spec {
        let first_line = spec.lines().next().unwrap_or("");
        out.push_str(&detail_field("Spec", &ansi(ANSI_DIM, first_line)));
    }
    if !t.tags.is_empty() {
        let tags: Vec<_> = t.tags.iter().cloned().collect();
        out.push_str(&detail_field("Tags", &ansi(ANSI_YELLOW, &tags.join(", "))));
    }
    if !t.meta.is_empty() {
        out.push_str(&ansi(ANSI_YELLOW, "Metadata:"));
        out.push('\n');
        for (field, value) in &t.meta {
            out.push_str(&format!(
                "  {}: {}\n",
                ansi(ANSI_CYAN, field),
                value.replace('\n', "\n    ")
            ));
        }
    }
    out.push_str(&ansi(ANSI_YELLOW, "Description:"));
    out.push('\n');
    match t.description.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(description) => {
            out.push('\n');
            out.push_str(&format!("  {}\n", description.replace('\n', "\n  ")));
        }
        None => {
            out.push_str("  ");
            out.push_str(&ansi(ANSI_DIM, "none"));
            out.push('\n');
        }
    }
    out.push_str(&ansi(ANSI_DIM, &title_bar));
    out.push('\n');

    if t.comments.is_empty() {
        out.push_str(&ansi(ANSI_DIM, "(no comments)"));
        out.push('\n');
    } else {
        for c in &t.comments {
            out.push_str(&format!(
                "\n{} {} {}\n  {}\n",
                ansi(ANSI_CYAN, &display_name(&c.author, nicks)),
                ansi(ANSI_DIM, "-"),
                ansi(ANSI_DIM, &c.at.format(&Rfc3339).unwrap_or_default()),
                c.body.replace('\n', "\n  "),
            ));
        }
    }
    out
}

/// Render a single ticket as JSON (for scripting).
pub fn ticket_json(t: &Ticket) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(t)
}

pub fn tickets_json(t: &[Ticket]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(t)
}

const TICKET_MARKDOWN_TEMPLATE: &str = r#"# Ticket: {title}

## Details

{details}

## Description

{description}

## Metadata

{metadata}

## Comments

{comments}

## Next Commands

{next_commands}
"#;

const TICKETS_MARKDOWN_TEMPLATE: &str = r#"# Tickets

- Count: {count}

## Overview

{overview}

## Ticket Details

{details}

## Next Commands

{next_commands}
"#;

const IMPORT_MARKDOWN_TEMPLATE: &str = r#"# GitHub Issue Import

- Imported: {imported}
- Skipped: {skipped}

## Imported Tickets

{tickets}

## Next Commands

{next_commands}
"#;

/// Render a single ticket as Markdown for agents and documents.
pub fn ticket_markdown(t: &Ticket) -> String {
    ticket_markdown_with_rels(t, None)
}

/// Like [`ticket_markdown`], but resolves relationship ids to title + status
/// when a [`RelLookup`] is supplied (used by `ti show --markdown`).
pub fn ticket_markdown_with_rels(t: &Ticket, rels: Option<&RelLookup>) -> String {
    render_template(
        TICKET_MARKDOWN_TEMPLATE,
        &[
            ("title", markdown_inline(&flatten(&t.title))),
            ("details", ticket_details_markdown(t, rels)),
            ("description", markdown_body(t.description.as_deref())),
            ("metadata", metadata_markdown(t)),
            ("comments", comments_markdown(t)),
            ("next_commands", ticket_next_commands(t)),
        ],
    )
}

/// Render tickets as Markdown, including overview and full per-ticket details.
pub fn tickets_markdown(tickets: &[Ticket]) -> String {
    render_template(
        TICKETS_MARKDOWN_TEMPLATE,
        &[
            ("count", tickets.len().to_string()),
            ("overview", tickets_overview_markdown(tickets)),
            ("details", tickets_details_markdown(tickets)),
            ("next_commands", tickets_next_commands(tickets)),
        ],
    )
}

/// Render checkout-clear status as Markdown.
pub fn checkout_clear_markdown() -> String {
    "\
# Current Ticket

- Current: none

## Next Commands

- `ti list --markdown` to inspect open tickets.
- `ti checkout <id>` to select a current ticket.
"
    .to_string()
}

/// Render GitHub import summary as Markdown.
pub fn import_markdown(imported: usize, skipped: usize, tickets: &[Ticket]) -> String {
    render_template(
        IMPORT_MARKDOWN_TEMPLATE,
        &[
            ("imported", imported.to_string()),
            ("skipped", skipped.to_string()),
            ("tickets", imported_tickets_markdown(tickets)),
            ("next_commands", import_next_commands(tickets)),
        ],
    )
}

fn render_template(template: &str, values: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

fn ticket_details_markdown(t: &Ticket, rels: Option<&RelLookup>) -> String {
    let mut out = String::new();
    writeln!(out, "- Id: {}", code_span(&t.id.to_string())).unwrap();
    writeln!(out, "- Short id: {}", code_span(&t.short_id())).unwrap();
    writeln!(out, "- Title: {}", markdown_inline(&t.title)).unwrap();
    writeln!(out, "- Status: {}", code_span(t.status.as_str())).unwrap();
    writeln!(out, "- State: {}", code_span(t.state.as_str())).unwrap();
    writeln!(
        out,
        "- Created: {} ({}) by {}",
        markdown_inline(&friendly_date(t.created_at)),
        code_span(&t.created_at.format(&Rfc3339).unwrap_or_default()),
        markdown_inline(&t.created_by)
    )
    .unwrap();
    writeln!(
        out,
        "- Assigned: {}",
        optional_inline(t.assigned.as_deref())
    )
    .unwrap();
    writeln!(
        out,
        "- Closed by: {}",
        optional_inline(t.closed_by.as_deref())
    )
    .unwrap();
    writeln!(
        out,
        "- Priority: {}",
        t.priority
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string())
    )
    .unwrap();
    writeln!(
        out,
        "- Points: {}",
        t.points
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string())
    )
    .unwrap();
    writeln!(
        out,
        "- Milestone: {}",
        optional_inline(t.milestone.as_deref())
    )
    .unwrap();
    writeln!(out, "- Code: {}", optional_inline(t.code.as_deref())).unwrap();
    if let Some(parent_id) = &t.parent {
        let value = format_related(std::iter::once(parent_id), rels);
        writeln!(out, "- Parent: {}", markdown_inline(&value)).unwrap();
    }
    if !t.children.is_empty() {
        let value = format_related(t.children.iter(), rels);
        writeln!(out, "- Children: {}", markdown_inline(&value)).unwrap();
    }
    if !t.depends_on.is_empty() {
        let value = format_related(t.depends_on.iter(), rels);
        writeln!(out, "- Depends on: {}", markdown_inline(&value)).unwrap();
    }
    if !t.blocks.is_empty() {
        let value = format_related(t.blocks.iter(), rels);
        writeln!(out, "- Blocks: {}", markdown_inline(&value)).unwrap();
    }
    writeln!(out, "- Tags: {}", tags_inline(t)).unwrap();
    out.trim_end().to_string()
}

fn metadata_markdown(t: &Ticket) -> String {
    if t.meta.is_empty() {
        return "_No metadata._".to_string();
    }

    let mut out = String::new();
    for (field, value) in &t.meta {
        writeln!(
            out,
            "- {}: {}",
            code_span(field),
            markdown_body(Some(value))
        )
        .unwrap();
    }
    out.trim_end().to_string()
}

fn comments_markdown(t: &Ticket) -> String {
    if t.comments.is_empty() {
        return "_No comments._".to_string();
    }

    let mut out = String::new();
    for (index, comment) in t.comments.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        writeln!(out, "**Comment {}**", index + 1).unwrap();
        writeln!(out).unwrap();
        writeln!(out, "- Author: {}", markdown_inline(&comment.author)).unwrap();
        writeln!(
            out,
            "- At: {}",
            code_span(&comment.at.format(&Rfc3339).unwrap_or_default())
        )
        .unwrap();
        writeln!(out).unwrap();
        writeln!(out, "{}", markdown_body(Some(&comment.body))).unwrap();
    }
    out.trim_end().to_string()
}

fn tickets_overview_markdown(tickets: &[Ticket]) -> String {
    if tickets.is_empty() {
        return "_No tickets._".to_string();
    }

    let mut out = String::from("| Id | Title | Status | State | Assigned | Tags | Created |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
    for ticket in tickets {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            table_cell(&ticket.short_id()),
            table_cell(&ticket.title),
            table_cell(ticket.status.as_str()),
            table_cell(ticket.state.as_str()),
            table_cell(ticket.assigned.as_deref().unwrap_or("")),
            table_cell(&ticket.tags.iter().cloned().collect::<Vec<_>>().join(", ")),
            table_cell(&friendly_date(ticket.created_at)),
        )
        .unwrap();
    }
    out.trim_end().to_string()
}

fn tickets_details_markdown(tickets: &[Ticket]) -> String {
    if tickets.is_empty() {
        return "_No tickets matched._".to_string();
    }

    tickets
        .iter()
        .map(ticket_detail_section_markdown)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn ticket_detail_section_markdown(t: &Ticket) -> String {
    render_template(
        "\
### {title}

{details}

#### Description

{description}

#### Metadata

{metadata}

#### Comments

{comments}",
        &[
            ("title", markdown_inline(&flatten(&t.title))),
            ("details", ticket_details_markdown(t, None)),
            ("description", markdown_body(t.description.as_deref())),
            ("metadata", metadata_markdown(t)),
            ("comments", comments_markdown(t)),
        ],
    )
}

fn imported_tickets_markdown(tickets: &[Ticket]) -> String {
    if tickets.is_empty() {
        "_No new tickets imported._".to_string()
    } else {
        tickets_details_markdown(tickets)
    }
}

fn ticket_next_commands(t: &Ticket) -> String {
    let id = t.short_id();
    let mut commands = vec![
        format!("`ti show {id} --markdown` to refresh this ticket."),
        format!("`ti checkout {id}` to make this the current ticket."),
        format!("`ti comment -t {id} \"progress update\"` to add a progress note."),
        format!("`ti edit {id}` to update the title or description."),
        format!("`ti tag -t {id} <tag>` to add a queryable tag."),
    ];

    if t.status == TicketStatus::Open {
        commands.push(format!("`ti state blocked -t {id}` to mark it blocked."));
        commands.push(format!("`ti state closed -t {id}` to resolve it."));
    } else {
        commands.push(format!("`ti state open -t {id}` to reopen it."));
    }

    commands
        .into_iter()
        .map(|command| format!("- {command}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tickets_next_commands(tickets: &[Ticket]) -> String {
    let mut commands = vec![
        "`ti list --markdown --all` to include closed tickets.".to_string(),
        "`ti list --markdown --tag <tag>` to narrow by tag.".to_string(),
        "`ti list --markdown --assigned <user>` to narrow by assignee.".to_string(),
    ];
    if let Some(ticket) = tickets.first() {
        let id = ticket.short_id();
        commands.push(format!(
            "`ti show {id} --markdown` to inspect the first ticket."
        ));
        commands.push(format!("`ti checkout {id}` to make it current."));
    } else {
        commands.push("`ti new --title \"...\" --markdown` to create a ticket.".to_string());
    }

    commands
        .into_iter()
        .map(|command| format!("- {command}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn import_next_commands(tickets: &[Ticket]) -> String {
    let mut commands = vec![
        "`ti list --markdown --tag github` to review imported GitHub tickets.".to_string(),
        "`ti sync` to share imported ticket metadata.".to_string(),
    ];
    if let Some(ticket) = tickets.first() {
        commands.push(format!(
            "`ti show {} --markdown` to inspect the first imported ticket.",
            ticket.short_id()
        ));
    }
    commands
        .into_iter()
        .map(|command| format!("- {command}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn optional_inline(value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(markdown_inline)
        .unwrap_or_else(|| "none".to_string())
}

fn tags_inline(t: &Ticket) -> String {
    if t.tags.is_empty() {
        "none".to_string()
    } else {
        t.tags
            .iter()
            .map(|tag| code_span(tag))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn markdown_body(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "_None._".to_string())
}

fn markdown_inline(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "\\<")
        .replace('>', "\\>")
        .replace('\n', " ")
}

fn table_cell(value: &str) -> String {
    markdown_inline(&flatten(value)).replace('|', "\\|")
}

fn code_span(value: &str) -> String {
    if value.contains('`') {
        format!("`` {value} ``")
    } else {
        format!("`{value}`")
    }
}

fn fit(value: &str, width: usize) -> String {
    let truncated = truncate_display(value, width);
    let padding = width.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
    format!("{truncated}{}", " ".repeat(padding))
}

fn styled_ticket_id(
    ticket: &Ticket,
    width: usize,
    ref_lengths: &BTreeMap<uuid::Uuid, usize>,
) -> String {
    let hex = ticket.id.to_string().replace('-', "");
    let display_len = ref_lengths.get(&ticket.id).copied().unwrap_or(6).max(6);
    let visible: String = hex.chars().take(display_len).collect();
    let reference_len = ref_lengths
        .get(&ticket.id)
        .copied()
        .unwrap_or(6)
        .min(visible.len());
    let (reference, rest) = visible.split_at(reference_len);
    let padding = width.saturating_sub(visible.len());

    format!(
        "{}{}{}",
        ansi(ANSI_YELLOW, reference),
        ansi(ANSI_CYAN, rest),
        " ".repeat(padding)
    )
}

fn truncate_display(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }

    let ellipsis = if max_width > 3 { "..." } else { "." };
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    let content_width = max_width.saturating_sub(ellipsis_width);
    let mut out = String::new();
    let mut width = 0;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > content_width {
            break;
        }
        out.push(ch);
        width += char_width;
    }
    out.push_str(ellipsis);
    out
}

fn flatten(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ansi(color: &str, value: &str) -> String {
    format!("{color}{value}{ANSI_RESET}")
}

fn detail_field(label: &str, value: &str) -> String {
    format!(
        "{}{} {value}\n",
        ansi(ANSI_YELLOW, &format!("{label:<8}")),
        ansi(ANSI_DIM, ":")
    )
}

fn state_color(state: &str) -> &'static str {
    match state {
        "new" | "assigned" | "in-progress" => ANSI_GREEN,
        "blocked" | "review" => ANSI_YELLOW,
        "resolved" | "wontfix" | "duplicate" | "invalid" => ANSI_PURPLE,
        _ => ANSI_DIM,
    }
}

fn status_color(status: &str) -> &'static str {
    match status {
        "open" => ANSI_GREEN,
        "closed" => ANSI_PURPLE,
        _ => ANSI_DIM,
    }
}

fn review_status_color(status: &str) -> &'static str {
    match status {
        "approved" | "merged" => ANSI_GREEN,
        "changes-requested" => "\x1b[31m",
        "closed" => ANSI_DIM,
        "open" => ANSI_CYAN,
        _ => ANSI_DIM,
    }
}

fn review_status_label(status: &str) -> &str {
    match status {
        "changes-requested" => "ch.req",
        status => status,
    }
}

fn review_progress_color(progress: &str) -> &'static str {
    if progress.starts_with("-/") {
        ANSI_DIM
    } else {
        let Some((approved, total)) = progress.split_once('/') else {
            return ANSI_DIM;
        };
        match (approved.parse::<usize>(), total.parse::<usize>()) {
            (Ok(approved), Ok(total)) if total > 0 && approved >= total => ANSI_GREEN,
            (Ok(approved), _) if approved > 0 => ANSI_YELLOW,
            _ => ANSI_DIM,
        }
    }
}

fn compact_relative_time(then: OffsetDateTime, now: OffsetDateTime) -> String {
    let label = relative_time(then, now);
    if label.len() <= 3 {
        return label;
    }
    label
        .strip_suffix("mo")
        .unwrap_or(&label)
        .chars()
        .take(3)
        .collect()
}

fn friendly_date(when: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        when.year(),
        u8::from(when.month()),
        when.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use ticgit_lib::TicketState;
    use uuid::Uuid;

    #[test]
    fn open_ticket_refs_ignore_closed_ticket_collisions() {
        let open = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "open",
            TicketState::New,
        );
        let other_open = ticket(
            "d7a2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "other",
            TicketState::New,
        );
        let closed = ticket(
            "d7f99999-d6ec-3da1-a180-0a33fb090d59",
            "closed",
            TicketState::Resolved,
        );

        let refs = open_ticket_ref_lengths(&[open.clone(), other_open, closed]);

        assert_eq!(refs.get(&open.id), Some(&3));
    }

    #[test]
    fn tickets_table_shows_priority_when_width_allows() {
        let mut ticket = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "priority ticket",
            TicketState::New,
        );
        ticket.priority = Some(2);
        ticket.assigned = Some("tester@example.com".to_string());
        ticket.tags.insert("feature".to_string());
        let refs = open_ticket_ref_lengths(&[ticket.clone()]);

        let table = strip_ansi(&tickets_table_with_width(
            &[ticket],
            None,
            &refs,
            100,
            OffsetDateTime::UNIX_EPOCH,
            None,
        ));

        assert!(!table.contains("Pri"));
        assert!(table.contains("Dt  P  Title"));
        assert!(table.contains(" 2 priority ticket"));
        assert!(table.contains("Assgn"));
        assert!(table.contains("Tags"));
    }

    #[test]
    fn tickets_table_drops_optional_columns_as_width_shrinks() {
        let mut ticket = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "priority ticket",
            TicketState::New,
        );
        ticket.priority = Some(2);
        ticket.assigned = Some("tester@example.com".to_string());
        ticket.tags.insert("feature".to_string());
        let refs = open_ticket_ref_lengths(&[ticket.clone()]);

        let medium = strip_ansi(&tickets_table_with_width(
            &[ticket.clone()],
            None,
            &refs,
            54,
            OffsetDateTime::UNIX_EPOCH,
            None,
        ));
        assert!(!medium.contains("Pri"));
        assert!(medium.contains("Dt  P  Title"));
        assert!(medium.contains(" 2 "));
        assert!(!medium.contains("Assgn"));
        assert!(!medium.contains("Tags"));

        let narrow = strip_ansi(&tickets_table_with_width(
            &[ticket],
            None,
            &refs,
            46,
            OffsetDateTime::UNIX_EPOCH,
            None,
        ));
        assert!(!narrow.contains(" 2 "));
        assert!(!narrow.contains("Assgn"));
        assert!(!narrow.contains("Tags"));
    }

    #[test]
    fn tickets_table_has_no_blank_line_between_header_and_separator() {
        let ticket = ticket(
            "d7f2d8f6-d6ec-3da1-a180-0a33fb090d59",
            "priority ticket",
            TicketState::New,
        );
        let refs = open_ticket_ref_lengths(std::slice::from_ref(&ticket));

        let table = strip_ansi(&tickets_table_with_width(
            &[ticket],
            None,
            &refs,
            80,
            OffsetDateTime::UNIX_EPOCH,
            None,
        ));
        let lines = table.lines().collect::<Vec<_>>();

        assert!(lines[0].contains("TicId"));
        assert!(lines[1].starts_with("---"));
    }

    #[test]
    fn reviews_table_uses_compact_colored_columns() {
        let rows = vec![ReviewTableRow {
            ticket: Some("859dd6".to_string()),
            ticket_unique_chars: 3,
            branch: "patch-based-review-versions@1779170645".to_string(),
            approvals: "3/5".to_string(),
            status: "changes-requested".to_string(),
            title: "Use patch ids for review versions".to_string(),
        }];

        let table = strip_ansi(&reviews_table_with_width(&rows, 80));

        assert!(table.contains("TicId"));
        assert!(table.contains("Branch"));
        assert!(table.contains("Rv"));
        assert!(table.contains("Status"));
        assert!(table.contains("Title"));
        assert!(!table.contains("BranchId"));
        assert!(table.contains("859dd6"));
        assert!(table.contains("3/5"));
        assert!(table.contains("ch.req"));
        assert!(table.contains("Use patch ids"));
        assert!(table
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("---")));
    }

    #[test]
    fn compact_relative_time_fits_table_date_column() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(400);

        assert_eq!(
            UnicodeWidthStr::width(
                fit(
                    &compact_relative_time(now - time::Duration::hours(48), now),
                    3
                )
                .as_str()
            ),
            3
        );
        assert_eq!(
            UnicodeWidthStr::width(
                fit(
                    &compact_relative_time(now - time::Duration::days(3), now),
                    3
                )
                .as_str()
            ),
            3
        );
        assert_eq!(
            compact_relative_time(now - time::Duration::days(360), now),
            "12"
        );
    }

    fn ticket(id: &str, title: &str, state: TicketState) -> Ticket {
        Ticket {
            id: Uuid::parse_str(id).unwrap(),
            title: title.to_string(),
            description: None,
            spec: None,
            status: state.status(),
            state,
            assigned: None,
            closed_by: None,
            priority: None,
            points: None,
            milestone: None,
            code: None,
            parent: None,
            children: BTreeSet::new(),
            depends_on: BTreeSet::new(),
            blocks: BTreeSet::new(),
            tags: BTreeSet::new(),
            meta: BTreeMap::new(),
            comments: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            created_by: "tester@example.com".to_string(),
        }
    }

    fn strip_ansi(input: &str) -> String {
        let mut stripped = String::new();
        let mut chars = input.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                stripped.push(ch);
            }
        }
        stripped
    }
}
