//! `ti hook`: install and run the Sync hook (see `CONTEXT.md`).
//!
//! The installed hook is a thin shell block; everything else happens in
//! `ti hook run pre-push`. See `docs/adr/0001-sync-hook-logic-lives-in-binary.md`.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use dialoguer::Select;

use crate::render::{ANSI_PURPLE, ANSI_RESET, ANSI_YELLOW};

/// Set while a Sync hook is syncing, so the push it triggers does not recurse.
const GUARD_ENV: &str = "TI_SYNC_IN_PROGRESS";

const BLOCK_START: &str = "# >>> ti hook >>>";
const BLOCK_END: &str = "# <<< ti hook <<<";
const SHEBANG: &str = "#!/usr/bin/env sh";

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the Sync hook (runs `ti sync` before code is pushed).
    Install(TargetArgs),
    /// Check that the Sync hook is installed and current.
    Check(TargetArgs),
    /// Remove the Sync hook.
    Uninstall(TargetArgs),
    /// Run a hook. Called by the installed hook, not by hand.
    #[command(hide = true)]
    Run(RunArgs),
}

#[derive(Debug, Parser)]
pub struct TargetArgs {
    /// Hook location. Detected from the repo if omitted; `install` asks in a terminal.
    #[arg(long = "target", value_enum)]
    pub target: Option<Target>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// `.husky/pre-push` (shared with the team when committed).
    Husky,
    /// The repo's git hooks directory (local to this clone).
    Git,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HookName {
    PrePush,
}

#[derive(Debug, Parser)]
pub struct RunArgs {
    #[arg(value_enum)]
    pub hook: HookName,
    /// Name of the remote being pushed to (git passes this as `$1`).
    pub remote: Option<String>,
    /// URL of the remote being pushed to (git passes this as `$2`).
    pub url: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Install(args) => install(args),
        Command::Check(args) => check(args),
        Command::Uninstall(args) => uninstall(args),
        Command::Run(args) => {
            run_hook(args);
            Ok(())
        }
    }
}

// -- install / check / uninstall ------------------------------------------

fn install(args: TargetArgs) -> Result<()> {
    let target = match args.target {
        Some(target) => target,
        None if std::io::stdin().is_terminal() => prompt_target(detect_target()?)?,
        None => detect_target()?,
    };
    let path = hook_path(target)?;
    let existing = std::fs::read_to_string(&path).ok();
    let next = install_block(existing.as_deref());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
    make_executable(&path)?;
    println!("Installed Sync hook in {}.", path.display());
    Ok(())
}

fn check(args: TargetArgs) -> Result<()> {
    let path = hook_path(args.target.map_or_else(detect_target, Ok)?)?;
    let current = std::fs::read_to_string(&path)
        .map(|contents| contents.contains(&block()))
        .unwrap_or(false);
    if current {
        println!("{} has the Sync hook and it is current.", path.display());
        return Ok(());
    }
    bail!(
        "{} does not have the Sync hook or it is out of date",
        path.display()
    );
}

fn uninstall(args: TargetArgs) -> Result<()> {
    let path = hook_path(args.target.map_or_else(detect_target, Ok)?)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let Some(next) = remove_block(&existing) else {
        println!("No Sync hook in {}.", path.display());
        return Ok(());
    };
    if next.trim().is_empty() || next.trim() == SHEBANG {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    } else {
        std::fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
    }
    println!("Removed Sync hook from {}.", path.display());
    Ok(())
}

fn prompt_target(detected: Target) -> Result<Target> {
    let targets = [Target::Husky, Target::Git];
    let labels = [
        "Husky - .husky/pre-push (shared with the team)",
        "Git - hooks directory (this clone only)",
    ];
    let selected = Select::new()
        .with_prompt("Install Sync hook")
        .items(labels)
        .default(targets.iter().position(|t| *t == detected).unwrap_or(0))
        .interact()?;
    Ok(targets[selected])
}

fn detect_target() -> Result<Target> {
    Ok(if repo_root()?.join(".husky").is_dir() {
        Target::Husky
    } else {
        Target::Git
    })
}

fn hook_path(target: Target) -> Result<PathBuf> {
    match target {
        Target::Husky => Ok(repo_root()?.join(".husky").join("pre-push")),
        Target::Git => {
            let dir = git_hooks_dir()?;
            if dir.components().any(|c| c.as_os_str() == "_")
                && dir.to_string_lossy().contains(".husky")
            {
                bail!(
                    "git hooks path is {}, which Husky regenerates; use --target husky",
                    dir.display()
                );
            }
            Ok(dir.join("pre-push"))
        }
    }
}

fn repo_root() -> Result<PathBuf> {
    let repo = gix::discover(".").context("finding git repository")?;
    repo.workdir()
        .map(PathBuf::from)
        .or_else(|| repo.git_dir().parent().map(PathBuf::from))
        .context("could not determine repository root")
}

fn git_hooks_dir() -> Result<PathBuf> {
    let output = Process::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-path", "hooks"])
        .output()
        .context("running git rev-parse")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

// -- the shell block -------------------------------------------------------

/// The marked block written into the hook file. Deliberately thin: it only
/// finds `ti` and hands over; no `exec`, so later steps in a shared hook file
/// still run, and it never fails the hook.
fn block() -> String {
    format!(
        r#"{BLOCK_START}
# Managed by `ti hook install`; remove with `ti hook uninstall`.
if ! command -v ti >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/ti" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
fi
if command -v ti >/dev/null 2>&1; then
  ti hook run pre-push "$@" || true
elif [ -f .git-meta ]; then
  echo "ti: not found, skipping ticket sync" >&2
fi
{BLOCK_END}"#
    )
}

/// New hook-file contents with the block installed: replaced in place if
/// present, appended to the end of an existing file, or a fresh file.
fn install_block(existing: Option<&str>) -> String {
    let block = block();
    match existing {
        Some(text) if text.contains(BLOCK_START) && text.contains(BLOCK_END) => {
            replace_block(text, &block)
        }
        Some(text) if !text.trim().is_empty() => format!("{}\n\n{block}\n", text.trim_end()),
        _ => format!("{SHEBANG}\n\n{block}\n"),
    }
}

fn block_range(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_START)?;
    let end = start + text[start..].find(BLOCK_END)? + BLOCK_END.len();
    Some((start, end))
}

fn replace_block(text: &str, block: &str) -> String {
    match block_range(text) {
        Some((start, end)) => format!("{}{block}{}", &text[..start], &text[end..]),
        None => text.to_string(),
    }
}

/// The file contents without the block, or `None` if it has no block.
fn remove_block(text: &str) -> Option<String> {
    let (start, end) = block_range(text)?;
    let head = text[..start].trim_end();
    let tail = text[end..].trim_start();
    Some(match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("{tail}\n"),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}\n"),
    })
}

// -- running the hook ------------------------------------------------------

fn run_hook(args: RunArgs) {
    match args.hook {
        HookName::PrePush => pre_push(args.remote.as_deref()),
    }
}

/// Never fails and never exits non-zero: sync problems become a warning.
fn pre_push(remote: Option<&str>) {
    if std::env::var_os(GUARD_ENV).is_some() {
        return;
    }
    let mut stdin = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut stdin);
    }
    let meta_remote = super::sync::sync_remote(None).ok().flatten();
    if !should_sync(&stdin, remote, meta_remote.as_deref()) {
        return;
    }

    // The sync pushes, which fires this hook again for the meta ref.
    std::env::set_var(GUARD_ENV, "1");
    match sync_once() {
        Ok(line) => say(ANSI_PURPLE, &line),
        Err(err) => {
            let reason = format!("{err:#}");
            let reason = reason.lines().next().unwrap_or_default();
            say(
                ANSI_YELLOW,
                &format!("ti: ticket sync failed, tickets not pushed ({reason})"),
            );
        }
    }
}

fn sync_once() -> Result<String> {
    let store = super::open_store()?;
    let outcome = super::sync::sync_tickets(&store, None)?;
    Ok(format!(
        "ti: synced tickets ({} pulled, {} pushed)",
        outcome.new_tickets.len(),
        outcome.total
    ))
}

fn say(color: &str, line: &str) {
    let use_color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    if use_color {
        eprintln!("{color}{line}{ANSI_RESET}");
    } else {
        eprintln!("{line}");
    }
}

/// Whether a push should trigger a sync: it must go to the remote tickets sync
/// with and update at least one branch (not just tags, deletions or meta refs).
/// `stdin` holds git's `<local ref> <local oid> <remote ref> <remote oid>` lines.
fn should_sync(stdin: &str, pushed_remote: Option<&str>, meta_remote: Option<&str>) -> bool {
    let (Some(pushed), Some(meta)) = (pushed_remote, meta_remote) else {
        return false;
    };
    if pushed != meta {
        return false;
    }
    stdin.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let (_local_ref, local_oid, remote_ref) = (fields.next(), fields.next(), fields.next());
        match (local_oid, remote_ref) {
            (Some(oid), Some(remote_ref)) => {
                remote_ref.starts_with("refs/heads/") && oid.chars().any(|c| c != '0')
            }
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "1111111111111111111111111111111111111111";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn push_line(local_ref: &str, oid: &str, remote_ref: &str) -> String {
        format!("{local_ref} {oid} {remote_ref} {ZERO}\n")
    }

    #[test]
    fn syncs_when_a_branch_goes_to_the_meta_remote() {
        let stdin = push_line("refs/heads/main", OID, "refs/heads/main");
        assert!(should_sync(&stdin, Some("origin"), Some("origin")));
    }

    #[test]
    fn skips_meta_refs_tags_and_deletions() {
        let meta = push_line("refs/meta/main", OID, "refs/meta/main");
        let tag = push_line("refs/tags/v1", OID, "refs/tags/v1");
        let delete = push_line("(delete)", ZERO, "refs/heads/old");
        for stdin in [meta, tag, delete, String::new()] {
            assert!(!should_sync(&stdin, Some("origin"), Some("origin")));
        }
    }

    #[test]
    fn skips_other_remotes_and_missing_meta_remote() {
        let stdin = push_line("refs/heads/main", OID, "refs/heads/main");
        assert!(!should_sync(&stdin, Some("fork"), Some("origin")));
        assert!(!should_sync(&stdin, Some("origin"), None));
        assert!(!should_sync(&stdin, None, Some("origin")));
    }

    #[test]
    fn mixed_push_syncs_when_any_branch_is_updated() {
        let stdin = format!(
            "{}{}",
            push_line("refs/meta/main", OID, "refs/meta/main"),
            push_line("refs/heads/main", OID, "refs/heads/main")
        );
        assert!(should_sync(&stdin, Some("origin"), Some("origin")));
    }

    #[test]
    fn install_creates_a_fresh_hook_file() {
        let text = install_block(None);
        assert!(text.starts_with("#!/usr/bin/env sh\n"));
        assert!(text.contains(BLOCK_START) && text.contains(BLOCK_END));
    }

    #[test]
    fn install_appends_after_existing_steps_and_is_idempotent() {
        let once = install_block(Some("#!/bin/sh\nnpm test\n"));
        assert!(once.starts_with("#!/bin/sh\nnpm test\n\n"));
        assert!(once.find("npm test").unwrap() < once.find(BLOCK_START).unwrap());
        assert_eq!(install_block(Some(&once)), once);
    }

    #[test]
    fn install_replaces_a_stale_block_in_place() {
        let stale = format!("a\n{BLOCK_START}\nold\n{BLOCK_END}\nb\n");
        let next = install_block(Some(&stale));
        assert!(!next.contains("\nold\n"));
        assert!(next.starts_with("a\n") && next.ends_with("\nb\n"));
    }

    #[test]
    fn remove_block_keeps_the_rest() {
        let text = install_block(Some("#!/bin/sh\nnpm test\n"));
        assert_eq!(remove_block(&text).unwrap(), "#!/bin/sh\nnpm test\n");
        assert_eq!(remove_block("npm test\n"), None);
    }
}
