use std::process::Command;

use anyhow::{Context, Result};
use clap::Parser;
use ticgit_lib::{ForkMerge, Ticket, TicketStore};

use crate::commands::open_store;

#[derive(Debug, Parser)]
pub struct Args {
    /// URL or saved nickname of the fork to pull tickets from.
    pub source: String,

    /// Save the URL under this nickname for future use.
    pub nickname: Option<String>,

    /// Output the pull summary as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Output the pull summary as Markdown.
    #[arg(long = "markdown", conflicts_with = "json")]
    pub markdown: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;

    // Resolve source to a URL (check nicknames first, then treat as URL).
    let url = resolve_source(&args.source)?;

    // If a nickname was given, save it.
    if let Some(ref nick) = args.nickname {
        save_nickname(nick, &url)?;
        println!("Saved remote \"{nick}\" → {url}");
    }

    // Fetch into a temp bare repo and open a store on it.
    let remote_tickets = fetch_remote_tickets(&url)?;

    if remote_tickets.is_empty() {
        println!("No tickets found on remote.");
        return Ok(());
    }

    let mut imported = Vec::new();
    let mut updated = Vec::new();

    for remote in &remote_tickets {
        match store.merge_fork_ticket(remote)? {
            ForkMerge::Imported => imported.push(remote.clone()),
            ForkMerge::Updated => updated.push(remote.clone()),
            ForkMerge::Unchanged => {}
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "source": url,
                "imported": imported.len(),
                "updated": updated.len(),
                "imported_tickets": imported,
                "updated_tickets": updated,
            })
        );
        return Ok(());
    }

    if args.markdown {
        println!("{}", pull_markdown(&url, &imported, &updated));
        return Ok(());
    }

    println!("Pulled from: {url}");
    if imported.is_empty() && updated.is_empty() {
        println!("No changes.");
    } else {
        if !imported.is_empty() {
            println!("Imported {} new ticket(s):", imported.len());
            for t in imported.iter().take(10) {
                println!("  {} {}", t.short_id(), t.title);
            }
            if imported.len() > 10 {
                println!("  ... and {} more", imported.len() - 10);
            }
        }
        if !updated.is_empty() {
            println!("Updated {} ticket(s):", updated.len());
            for t in updated.iter().take(10) {
                println!("  {} {}", t.short_id(), t.title);
            }
            if updated.len() > 10 {
                println!("  ... and {} more", updated.len() - 10);
            }
        }
    }

    Ok(())
}

/// Resolve a source string to a URL. Checks `.git/config` for a saved
/// nickname first, then treats the string as a literal URL.
fn resolve_source(source: &str) -> Result<String> {
    // Try as a nickname first.
    let key = format!("ticgit.remotes.{source}.url");
    if let Some(url) = git_config_get(&key)? {
        return Ok(url);
    }
    // Treat as URL if it looks like one (contains :// or : for SSH).
    if source.contains("://") || source.contains(':') || source.starts_with('/') {
        return Ok(source.to_string());
    }
    anyhow::bail!(
        "unknown remote \"{source}\" — use a URL or save one first with: ti pull <url> {source}"
    );
}

/// Save a nickname → URL mapping in `.git/config`.
fn save_nickname(nick: &str, url: &str) -> Result<()> {
    let key = format!("ticgit.remotes.{nick}.url");
    git_run(&["config", "--local", &key, url])
}

/// Set up a temp repo with the URL as a remote, pull git-meta data
/// from it, and return all tickets found.
fn fetch_remote_tickets(url: &str) -> Result<Vec<Ticket>> {
    let tmpdir = tempfile::tempdir().context("creating temp dir for fetch")?;
    let path = tmpdir.path();

    // Init a normal repo (git-meta needs a non-bare repo for its sqlite db).
    git_at(path, &["init", "--quiet", "-b", "main"])?;
    git_at(path, &["config", "user.email", "pull@ticgit.dev"])?;
    git_at(path, &["config", "user.name", "ticgit-pull"])?;
    git_at(path, &["commit", "--allow-empty", "-m", "init", "--quiet"])?;

    // Configure a remote with git-meta refspecs so session.pull() works.
    git_at(path, &["remote", "add", "origin", url])?;
    let namespace = meta_namespace()?;
    let fetch_refspec = format!("+refs/{namespace}/main:refs/{namespace}/remotes/main");
    git_at(
        path,
        &["config", "--add", "remote.origin.fetch", &fetch_refspec],
    )?;
    git_at(path, &["config", "remote.origin.meta", "true"])?;

    // Open a store on the temp repo and pull from the remote.
    let repo = gix::open(path).context("opening temp repo")?;
    let session = ticgit_lib::Session::open(repo.path()).context("opening session on temp repo")?;
    let remote_store =
        TicketStore::from_session(session).context("opening ticket store on temp repo")?;

    // Pull populates the sqlite database from the remote's refs.
    remote_store
        .pull(Some("origin"))
        .context("pulling metadata from remote")?;

    remote_store.list().context("listing tickets from remote")
}

fn meta_namespace() -> Result<String> {
    Ok(git_config_get("meta.namespace")?.unwrap_or_else(|| "meta".to_string()))
}

fn pull_markdown(url: &str, imported: &[Ticket], updated: &[Ticket]) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Pull from {url}\n\n"));
    if imported.is_empty() && updated.is_empty() {
        out.push_str("No changes.\n");
        return out;
    }
    if !imported.is_empty() {
        out.push_str(&format!(
            "### Imported {} new ticket(s)\n\n",
            imported.len()
        ));
        for t in imported {
            out.push_str(&format!("- **{}** {}\n", t.short_id(), t.title));
        }
        out.push('\n');
    }
    if !updated.is_empty() {
        out.push_str(&format!("### Updated {} ticket(s)\n\n", updated.len()));
        for t in updated {
            out.push_str(&format!("- **{}** {}\n", t.short_id(), t.title));
        }
        out.push('\n');
    }
    out
}

fn git_config_get(key: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--get", key])
        .output()
        .with_context(|| format!("running git config --get {key}"))?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git config --get {key} failed: {}", stderr.trim());
    }
}

fn git_run(args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn git_at(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}
