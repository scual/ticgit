use anyhow::Result;
use clap::Parser;
use serde_json::json;

use crate::commands::open_store;
use crate::commands::sync::{meta_namespace, remote_url, ssh_project_web_url, sync_remote};

#[derive(Debug, Parser)]
pub struct Args {
    /// Remote to push to. Defaults to git-meta's first configured meta remote.
    #[arg(short = 'r', long = "remote")]
    pub remote: Option<String>,

    /// Output the push result as JSON.
    #[arg(long = "json")]
    pub json: bool,
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

    store.push(args.remote.as_deref())?;
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
