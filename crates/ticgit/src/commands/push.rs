use anyhow::Result;
use clap::Parser;
use serde_json::json;

use crate::commands::open_store;
use crate::commands::sync::{
    meta_namespace, remote_url, ssh_project_web_url, sync_remote, verify_before_push,
};

#[derive(Debug, Parser)]
pub struct Args {
    /// Remote to push to. Defaults to git-meta's first configured meta remote.
    #[arg(short = 'r', long = "remote")]
    pub remote: Option<String>,

    /// Output the push result as JSON.
    #[arg(long = "json")]
    pub json: bool,

    /// Permit the push to delete remote tickets that are absent from the local
    /// store. Off by default: a push that would remove remote tickets is
    /// refused, guarding against a stranded/incomplete index clobbering the
    /// remote.
    #[arg(long = "allow-deletes")]
    pub allow_deletes: bool,

    /// Skip the pre-push `ti verify` integrity gate. Off by default: a store
    /// that fails verify is not allowed to overwrite the remote. Use only for
    /// intentional recovery flows.
    #[arg(long = "no-verify")]
    pub no_verify: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;
    let remote = sync_remote(args.remote.as_deref())?;
    let namespace = meta_namespace()?;
    let url = remote
        .as_deref()
        .map(remote_url)
        .transpose()?
        .unwrap_or_else(|| "(none)".to_string());
    let web_url = ssh_project_web_url(&url);

    if !args.json {
        if let Some(remote) = &remote {
            println!("Remote: {remote}");
        }
        println!("Ref: refs/{namespace}/main");
        println!("URL: {url}");
        if let Some(web_url) = &web_url {
            println!("Web URL: {web_url}");
        }
    }

    // Gate on integrity, identically to `ti sync`, so a plain `ti push` can
    // never overwrite the shared remote with a store that fails `ti verify`.
    verify_before_push(&store, args.no_verify)?;

    store.push(args.remote.as_deref(), args.allow_deletes)?;
    let total = store.list()?.len();

    if args.json {
        let out = json!({
            "remote": remote,
            "ref": format!("refs/{namespace}/main"),
            "url": url,
            "web_url": web_url,
            "pushed_total": total,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Push: {total} ticket(s) synced.");
        println!("Done.");
    }
    Ok(())
}
