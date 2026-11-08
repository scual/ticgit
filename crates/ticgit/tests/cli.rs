use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

struct TestRepo {
    dir: TempDir,
    state_file: TempDir,
}

impl TestRepo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("repo tempdir");
        let state_file = tempfile::tempdir().expect("state tempdir");
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "tester@example.com"]);
        git(dir.path(), &["config", "user.name", "Tester"]);
        git(
            dir.path(),
            &["commit", "--allow-empty", "-m", "init", "--quiet"],
        );
        Self { dir, state_file }
    }

    fn ti(&self) -> assert_cmd::Command {
        self.ti_in(self.dir.path())
    }

    /// Like [`ti`], but runs the binary from an arbitrary directory (e.g. a
    /// linked worktree) while sharing the same hermetic state file.
    fn ti_in(&self, cwd: &Path) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("ti").expect("ti binary");
        cmd.current_dir(cwd);
        cmd.env(
            "TICGIT_STATE_FILE",
            self.state_file.path().join("state.json"),
        );
        // Keep tests hermetic: the editor-resolution order is GIT_EDITOR ->
        // core.editor -> VISUAL -> EDITOR, so an ambient GIT_EDITOR/VISUAL in
        // the developer's shell would shadow a test's own `EDITOR` override.
        cmd.env_remove("GIT_EDITOR");
        cmd.env_remove("VISUAL");
        cmd.env_remove("EDITOR");
        cmd
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn create_ticket(repo: &TestRepo, title: &str) -> String {
    let output = repo
        .ti()
        .args(["new", "--title", title, "--id-only"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap().trim().to_string()
}

#[test]
fn review_cli_records_branch_review_flow() {
    let repo = TestRepo::new();
    let ticket_id = create_ticket(&repo, "Code review tooling");

    repo.ti()
        .args([
            "review",
            "new",
            "--branch",
            "main",
            "--ticket",
            &ticket_id,
            "--title",
            "Stable review title",
            "--description",
            "Stable review description",
            "--reviewer",
            "alice@example.com",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created review main@"))
        .stdout(predicate::str::contains("Stable review title"));

    repo.ti()
        .args(["review", "list", "--status", "open"])
        .assert()
        .success()
        .stdout(predicate::str::contains("BranchId").not())
        .stdout(predicate::str::contains("Rv"))
        .stdout(predicate::str::contains("TicId"))
        .stdout(predicate::str::contains(&ticket_id[..1]))
        .stdout(predicate::str::contains(&ticket_id[1..6]))
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("Stable review title"));

    fs::write(
        repo.dir.path().join("review.txt"),
        "updated review content\n",
    )
    .expect("write file");
    git(repo.dir.path(), &["add", "review.txt"]);
    git(
        repo.dir.path(),
        &["commit", "-m", "Last commit message", "--quiet"],
    );
    repo.ti()
        .args(["review", "update", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated review main@"));

    repo.ti()
        .args(["review", "show", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Title: Stable review title"))
        .stdout(predicate::str::contains(
            "Description: Stable review description",
        ))
        .stdout(predicate::str::contains("::"))
        .stdout(predicate::str::contains("Last commit message").not());

    repo.ti()
        .args(["review", "show", &ticket_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tickets:"))
        .stdout(predicate::str::contains(&ticket_id[..6]))
        .stdout(predicate::str::contains("Stable review title"));

    let review_json = repo
        .ti()
        .args(["review", "show", &ticket_id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let review_json: Value = serde_json::from_slice(&review_json).unwrap();
    assert_eq!(review_json["title"], "Stable review title");
    assert_eq!(review_json["ticket_ref"], &ticket_id[..6]);

    repo.ti()
        .args(["review", "show", &ticket_id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Review: Stable review title"))
        .stdout(predicate::str::contains("- Ticket:"));

    repo.ti()
        .args(["review", "add-reviewer", "bob@example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added reviewer bob@example.com"));

    repo.ti()
        .args([
            "review",
            "comment",
            "--path",
            "src/parser.rs",
            "--line",
            "42",
            "needs bounds checking",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added comment"));

    repo.ti()
        .args(["review", "request-changes", "needs error handling"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Requested changes"));

    repo.ti()
        .args(["review", "approve", "--comment", "looks good"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Approved main@"));

    let head = git_output(repo.dir.path(), &["rev-parse", "HEAD"]);
    repo.ti()
        .args(["review", "integrate", &head])
        .assert()
        .success()
        .stdout(predicate::str::contains("Integrated main@"));

    repo.ti()
        .args(["review", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No open reviews"))
        .stdout(predicate::str::contains("Stable review title").not());

    repo.ti()
        .args(["review", "list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stable review title"))
        .stdout(predicate::str::contains("merged"));

    let list_json = repo
        .ti()
        .args(["review", "list", "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list_json: Value = serde_json::from_slice(&list_json).unwrap();
    assert_eq!(list_json[0]["title"], "Stable review title");
    assert_eq!(list_json[0]["ticket_ref"], &ticket_id[..6]);

    repo.ti()
        .args(["review", "list", "--all", "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Reviews"))
        .stdout(predicate::str::contains("Stable review title"));

    repo.ti()
        .args(["review", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status: merged"))
        .stdout(predicate::str::contains("Tickets:"))
        .stdout(predicate::str::contains("alice@example.com"))
        .stdout(predicate::str::contains("bob@example.com"))
        .stdout(predicate::str::contains("[comment]"))
        .stdout(predicate::str::contains("src/parser.rs:42"))
        .stdout(predicate::str::contains("[changes-requested]"))
        .stdout(predicate::str::contains("[approval]"))
        .stdout(predicate::str::contains("looks good"));
}

#[cfg(unix)]
fn editor_script(repo: &TestRepo, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = repo.state_file.path().join("editor.sh");
    fs::write(
        &path,
        format!("#!/bin/sh\ncat > \"$1\" <<'EOF'\n{contents}\nEOF\n"),
    )
    .expect("write editor script");

    let mut permissions = fs::metadata(&path).expect("editor metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod editor script");
    path
}

#[cfg(unix)]
fn capturing_editor_script(repo: &TestRepo, captured: &Path, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = repo.state_file.path().join("capturing-editor.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncp \"$1\" \"{}\"\ncat > \"$1\" <<'EOF'\n{contents}\nEOF\n",
            captured.display()
        ),
    )
    .expect("write capturing editor script");

    let mut permissions = fs::metadata(&path).expect("editor metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod editor script");
    path
}

#[cfg(unix)]
fn executable_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(dir).expect("script dir");
    let path = dir.join(name);
    fs::write(&path, contents).expect("write executable script");

    let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod executable script");
    path
}

#[test]
fn init_is_idempotent() {
    let repo = TestRepo::new();
    repo.ti()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("ticgit initialised"));

    repo.ti()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("schema 1"));
}

#[test]
fn agent_prints_markdown_guide() {
    let mut cmd = assert_cmd::Command::cargo_bin("ti").expect("ti binary");
    cmd.arg("agent")
        .assert()
        .success()
        .stdout(predicate::str::contains("---"))
        .stdout(predicate::str::contains("name: ticgit"))
        .stdout(predicate::str::contains("# TicGit Agent Guide"))
        .stdout(predicate::str::contains("ti new -F /tmp/ticket.md"))
        .stdout(predicate::str::contains("ti list --markdown"))
        .stdout(predicate::str::contains("Prefer `--markdown`"))
        .stdout(predicate::str::contains("ti close <id>"))
        // The guide steers agents to --markdown for reading, while still
        // documenting --json for machine parsing.
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn agent_skill_installs_local_shared_skill_and_checks_it() {
    let repo = TestRepo::new();

    repo.ti()
        .args(["agent", "skill", "--target", "agents-local"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".agents/skills/ticgit/SKILL.md"));

    let skill = fs::read_to_string(repo.dir.path().join(".agents/skills/ticgit/SKILL.md"))
        .expect("skill file");
    assert!(skill.contains("name: ticgit"));
    assert!(skill.contains("# TicGit Agent Guide"));
    assert!(skill.contains("ti list --markdown"));

    repo.ti()
        .args(["agent", "skill", "--target", "agents-local", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed and current"));
}

#[test]
fn agent_skill_installs_agents_md_idempotently() {
    let repo = TestRepo::new();

    repo.ti()
        .args(["agent", "skill", "--target", "agents-md"])
        .assert()
        .success();
    repo.ti()
        .args(["agent", "skill", "--target", "agents-md"])
        .assert()
        .success();

    let agents = fs::read_to_string(repo.dir.path().join("AGENTS.md")).expect("AGENTS.md");
    assert_eq!(agents.matches("<!-- ticgit-agent-start -->").count(), 1);
    assert!(agents.contains("This project uses TicGit"));
    assert!(agents.contains("Run `ti agent`"));
}

#[test]
fn machine_output_schema_is_published_and_matches_cli_contract() {
    let schema: Value = serde_json::from_str(include_str!(env!("TICGIT_SCHEMA_V1_PATH"))).unwrap();
    assert_eq!(schema["$id"], "https://ticgit.dev/schema/v1.json");
    assert_eq!(schema["$defs"]["ticket"]["additionalProperties"], false);

    let required: BTreeSet<_> = schema["$defs"]["ticket"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        required,
        BTreeSet::from([
            "id".to_string(),
            "title".to_string(),
            "description".to_string(),
            "spec".to_string(),
            "status".to_string(),
            "state".to_string(),
            "assigned".to_string(),
            "closed_by".to_string(),
            "priority".to_string(),
            "points".to_string(),
            "milestone".to_string(),
            "code".to_string(),
            "parent".to_string(),
            "children".to_string(),
            "depends_on".to_string(),
            "blocks".to_string(),
            "tags".to_string(),
            "meta".to_string(),
            "comments".to_string(),
            "created_at".to_string(),
            "created_by".to_string(),
        ])
    );
    assert!(schema["$defs"]["ticket"]["properties"]["state"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .any(|state| state == "blocked"));
    assert_eq!(
        schema["$defs"]["ticket"]["properties"]["meta"]["additionalProperties"]["type"],
        "string"
    );
    // `subissues` is an optional, additive property — documented but not required.
    assert!(schema["$defs"]["ticket"]["properties"]["subissues"].is_object());
    assert!(!required.contains("subissues"));
    assert_eq!(
        schema["$defs"]["subissueNode"]["additionalProperties"],
        false
    );
    // `deleted` is additive too: always emitted by this binary (`null` for a
    // live ticket) but not required, so output from older binaries validates.
    assert!(!required.contains("deleted"));
    let deletion = &schema["$defs"]["deletion"];
    assert_eq!(deletion["additionalProperties"], false);
    assert_eq!(deletion["required"], serde_json::json!(["at", "by"]));
    assert_eq!(
        schema["$defs"]["ticket"]["properties"]["deleted"]["oneOf"],
        serde_json::json!([{ "type": "null" }, { "$ref": "#/$defs/deletion" }])
    );

    let repo = TestRepo::new();
    let id = create_ticket(&repo, "schema ticket");
    repo.ti()
        .args(["meta", "-t", &id, "branch", "feature/schema"])
        .assert()
        .success();
    repo.ti()
        .args(["comment", "-t", &id, "schema note"])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ticket: Value = serde_json::from_slice(&output).unwrap();
    let ticket_keys: BTreeSet<_> = ticket
        .as_object()
        .unwrap()
        .keys()
        .map(|key| key.to_string())
        .collect();
    let mut list_keys = required.clone();
    list_keys.insert("deleted".to_string());
    let mut show_keys = list_keys.clone();
    show_keys.insert("subissues".to_string());
    assert_eq!(ticket_keys, show_keys);
    assert!(ticket["deleted"].is_null());
    assert!(ticket["subissues"].is_array());
    assert_eq!(ticket["id"], id);
    assert_eq!(ticket["status"], "open");
    assert_eq!(ticket["state"], "new");
    assert_eq!(ticket["meta"]["branch"], "feature/schema");
    assert_eq!(ticket["comments"][0]["body"], "schema note");

    let output = repo
        .ti()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list: Value = serde_json::from_slice(&output).unwrap();
    let first = &list.as_array().unwrap()[0];
    assert_eq!(first["id"], id);
    let first_keys: BTreeSet<_> = first.as_object().unwrap().keys().cloned().collect();
    assert_eq!(first_keys, list_keys);

    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    let deleted = show_json(&repo, &id)["deleted"].clone();
    let deleted_keys: BTreeSet<_> = deleted.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        deleted_keys,
        BTreeSet::from(["at".to_string(), "by".to_string()])
    );
}

#[test]
fn json_machine_mode_keeps_stdout_parseable_and_plain() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "machine ticket");

    for args in [
        vec!["show", &id, "--json"],
        vec!["list", "--json"],
        vec!["state", "blocked", "-t", &id, "--json"],
    ] {
        let output = repo
            .ti()
            .args(args)
            .assert()
            .success()
            .stderr(predicate::eq(""))
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(output).unwrap();
        assert!(
            !stdout.contains("\x1b["),
            "JSON stdout must not contain ANSI escape sequences: {stdout:?}"
        );
        serde_json::from_str::<Value>(&stdout).expect("machine stdout is valid JSON");
    }
}

#[test]
fn json_machine_mode_errors_use_stderr_without_stdout() {
    let repo = TestRepo::new();

    repo.ti()
        .args(["show", "ffffffff", "--json"])
        .assert()
        .failure()
        .stdout(predicate::eq(""))
        .stderr(predicate::str::contains(
            "ticket prefix `ffffffff` matches no ticket",
        ));
}

#[test]
fn ambiguous_ticket_prefixes_fail_cleanly_in_machine_mode() {
    let repo = TestRepo::new();
    let mut prefixes: std::collections::BTreeMap<char, Vec<String>> =
        std::collections::BTreeMap::new();
    let ambiguous_prefix = (0..64).find_map(|i| {
        let id = create_ticket(&repo, &format!("ambiguous {i}"));
        let prefix = id.chars().next().unwrap();
        let ids = prefixes.entry(prefix).or_default();
        ids.push(id);
        (ids.len() == 2).then_some(prefix.to_string())
    });
    let ambiguous_prefix = ambiguous_prefix.expect("created two tickets with same leading hex");

    repo.ti()
        .args(["show", &ambiguous_prefix, "--json"])
        .assert()
        .failure()
        .stdout(predicate::eq(""))
        .stderr(predicate::str::contains("ambiguous"));
}

#[test]
fn version_flags_print_cargo_version() {
    let expected = format!("ti {}\n", env!("CARGO_PKG_VERSION"));

    for flag in ["-v", "--version"] {
        let mut cmd = assert_cmd::Command::cargo_bin("ti").expect("ti binary");
        cmd.arg(flag)
            .assert()
            .success()
            .stdout(predicate::eq(expected.as_str()));
    }
}

#[test]
fn help_lists_sync_pull_and_push() {
    let mut cmd = assert_cmd::Command::cargo_bin("ti").expect("ti binary");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sync"))
        .stdout(predicate::str::contains("pull"))
        .stdout(predicate::str::contains("push"));
}

#[test]
fn bare_ti_defaults_to_list() {
    let repo = TestRepo::new();
    create_ticket(&repo, "bare ti");

    repo.ti()
        .assert()
        .success()
        .stdout(predicate::str::contains("bare ti"))
        .stderr(predicate::str::contains("unknown tag mode").not());
}

#[test]
fn list_open_shows_all_open_tickets_without_default_truncation() {
    let repo = TestRepo::new();
    for i in 0..25 {
        create_ticket(&repo, &format!("open ticket {i}"));
    }

    let default_output = repo
        .ti()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let default_list: Value = serde_json::from_slice(&default_output).unwrap();
    assert_eq!(default_list.as_array().unwrap().len(), 20);

    let open_output = repo
        .ti()
        .args(["list", "--open", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let open_list: Value = serde_json::from_slice(&open_output).unwrap();
    assert_eq!(open_list.as_array().unwrap().len(), 25);
}

#[test]
fn init_bootstraps_git_meta_defaults() {
    let repo = TestRepo::new();
    git(
        repo.dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    );

    repo.ti()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Configured git-meta remote 'origin' with defaults.",
        ))
        .stdout(predicate::str::contains("ticgit initialised"));

    assert_eq!(
        git_output(repo.dir.path(), &["config", "--get", "meta.namespace"]),
        "meta",
    );
    assert_eq!(
        git_output(
            repo.dir.path(),
            &["config", "--bool", "--get", "remote.origin.meta"],
        ),
        "true",
    );
    let fetch = git_output(
        repo.dir.path(),
        &["config", "--get-all", "remote.origin.fetch"],
    );
    assert!(fetch.contains("+refs/meta/main:refs/meta/remotes/main"));
}

#[test]
fn sync_prints_remote_url_and_ref() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    repo.ti()
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains("Remote: origin"))
        .stdout(predicate::str::contains("Ref: refs/meta/main"))
        .stdout(predicate::str::contains(format!("URL: {remote_url}")));
}

#[test]
fn push_sends_tickets_to_bare_remote() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();
    create_ticket(&repo, "pushed ticket");

    repo.ti()
        .arg("push")
        .assert()
        .success()
        .stdout(predicate::str::contains("Remote: origin"))
        .stdout(predicate::str::contains("Ref: refs/meta/main"))
        .stdout(predicate::str::contains(format!("URL: {remote_url}")))
        .stdout(predicate::str::contains("1 ticket(s) synced"))
        .stdout(predicate::str::contains("Done."));
}

#[test]
fn push_json_emits_result_object() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();
    create_ticket(&repo, "pushed ticket");

    let output = repo
        .ti()
        .args(["push", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["remote"], "origin");
    assert_eq!(json["ref"], "refs/meta/main");
    assert_eq!(json["url"], remote_url);
    assert_eq!(json["pushed_total"], 1);
    // The human-readable lines must not leak into JSON stdout.
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("Done."));
}

#[test]
fn sync_json_emits_result_object() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();
    create_ticket(&repo, "synced ticket");
    // Seed the remote so the pull half of sync succeeds.
    repo.ti().arg("push").assert().success();

    let output = repo
        .ti()
        .args(["sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ref"], "refs/meta/main");
    assert_eq!(json["pushed_total"], 1);
    assert!(json["pulled"].as_array().unwrap().is_empty());
}

#[test]
fn new_show_and_list_round_trip() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "first bug");

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], id);
    assert_eq!(json["title"], "first bug");
    assert_eq!(json["status"], "open");
    assert_eq!(json["state"], "new");

    repo.ti()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("first bug"));

    repo.ti()
        .args(["show", &id, "--filter", ".title"])
        .assert()
        .success()
        .stdout(predicate::eq("first bug\n"));

    repo.ti()
        .args(["show", &id, "--filter"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available filters:"))
        .stdout(predicate::str::contains("ti show <id> --filter '.title'"));
}

#[test]
fn markdown_output_includes_ticket_data_and_next_commands() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "markdown ticket");

    repo.ti()
        .args(["meta", "-t", &id, "branch", "feature/markdown"])
        .assert()
        .success();
    repo.ti()
        .args(["comment", "-t", &id, "markdown note"])
        .assert()
        .success();

    repo.ti()
        .args(["show", &id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Ticket: markdown ticket"))
        .stdout(predicate::str::contains(format!("- Id: `{id}`")))
        .stdout(predicate::str::contains("feature/markdown"))
        .stdout(predicate::str::contains("markdown note"))
        .stdout(predicate::str::contains("## Next Commands"))
        .stdout(predicate::str::contains("ti checkout"));

    repo.ti()
        .args(["list", "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Tickets"))
        .stdout(predicate::str::contains(
            "| Id | Title | Status | State | Assigned | Tags | Created |",
        ))
        .stdout(predicate::str::contains("## Ticket Details"))
        .stdout(predicate::str::contains("ti show"));

    repo.ti()
        .args(["state", "blocked", "-t", &id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("- Status: `open`"))
        .stdout(predicate::str::contains("- State: `blocked`"))
        .stdout(predicate::str::contains("ti state closed"));

    repo.ti().args(["checkout", &id]).assert().success();
    repo.ti()
        .args(["checkout", "--clear", "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Current Ticket"))
        .stdout(predicate::str::contains("- Current: none"))
        .stdout(predicate::str::contains("ti list --markdown"));
}

#[test]
fn new_reads_title_and_description_from_file() {
    let repo = TestRepo::new();
    let file = repo.state_file.path().join("ticket.md");
    fs::write(&file, "file title\n\nfile description\nsecond line\n").unwrap();

    let output = repo
        .ti()
        .args(["new", "-F"])
        .arg(&file)
        .args(["--tags", "agent,feature", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["title"], "file title");
    assert_eq!(json["description"], "file description\nsecond line");
    let tags = json["tags"].as_array().unwrap();
    assert!(tags.iter().any(|tag| tag == "agent"));
    assert!(tags.iter().any(|tag| tag == "feature"));
}

#[test]
#[cfg(unix)]
fn edit_updates_title_and_description() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "old title");
    let editor = editor_script(&repo, "new title\n\nnew description\nsecond line\n");

    repo.ti()
        .env("GIT_EDITOR", &editor)
        .args(["edit", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated"));

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["title"], "new title");
    assert_eq!(json["description"], "new description\nsecond line");
}

#[test]
#[cfg(unix)]
fn comment_editor_prompt_includes_ticket_title() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "prompt title");
    let captured = repo.state_file.path().join("comment-template.md");
    let editor = capturing_editor_script(&repo, &captured, "edited body\n");

    repo.ti()
        .env("GIT_EDITOR", &editor)
        .args(["comment", "-t", &id, "--edit"])
        .assert()
        .success();

    let template = fs::read_to_string(captured).unwrap();
    assert!(template.contains("# Ticket comment"));
    assert!(template.contains("# Ticket: prompt title"));

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["comments"][0]["body"], "edited body");
}

#[test]
fn edit_reads_title_and_description_from_file() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "old title");
    let file = repo.state_file.path().join("ticket-edit.md");
    fs::write(&file, "file edit title\n\nfile edit description\n").unwrap();

    let output = repo
        .ti()
        .args(["edit", &id, "-F"])
        .arg(&file)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["title"], "file edit title");
    assert_eq!(json["description"], "file edit description");
}

#[test]
fn meta_sets_inline_and_file_values() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "metadata ticket");
    let file = repo.state_file.path().join("meta-value.txt");
    fs::write(&file, "feature/meta\n").unwrap();

    repo.ti()
        .args(["meta", "-t", &id, "branch", "feature/parser"])
        .assert()
        .success()
        .stdout(predicate::str::contains("meta branch: feature/parser"));

    let output = repo
        .ti()
        .args(["meta", "-t", &id, "notes", "-F"])
        .arg(&file)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["meta"]["branch"], "feature/parser");
    assert_eq!(json["meta"]["notes"], "feature/meta\n");

    repo.ti()
        .args(["show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Metadata:"))
        .stdout(predicate::str::contains("branch"))
        .stdout(predicate::str::contains("feature/parser"));
}

#[test]
fn mutating_commands_update_ticket() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "mutate me");

    repo.ti()
        .args(["tag", "-t", &id, "bug,ui"])
        .assert()
        .success();
    repo.ti()
        .args(["assign", "-t", &id, "tester@example.com"])
        .assert()
        .success();
    repo.ti()
        .args(["points", "-t", &id, "5"])
        .assert()
        .success();
    repo.ti()
        .args(["milestone", "-t", &id, "v1"])
        .assert()
        .success();
    repo.ti()
        .args(["meta", "-t", &id, "source", "cli-test"])
        .assert()
        .success();
    repo.ti()
        .args(["state", "resolved", "-t", &id])
        .assert()
        .success();
    repo.ti()
        .args(["comment", "-t", &id, "fixed", "now"])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "closed");
    assert_eq!(json["state"], "resolved");
    assert_eq!(json["assigned"], "tester@example.com");
    assert_eq!(json["closed_by"], "tester@example.com");
    assert_eq!(json["points"], 5);
    assert_eq!(json["milestone"], "v1");
    assert_eq!(json["tags"].as_array().unwrap().len(), 2);
    assert_eq!(json["meta"]["source"], "cli-test");
    assert_eq!(json["comments"][0]["body"], "fixed now");
}

#[test]
fn ticket_mutations_support_json_output() {
    let repo = TestRepo::new();

    let output = repo
        .ti()
        .args(["new", "--title", "json ticket", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let id = json["id"].as_str().unwrap().to_string();
    assert_eq!(json["title"], "json ticket");

    let output = repo
        .ti()
        .args(["tag", "-t", &id, "bug", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json["tags"].as_array().unwrap().iter().any(|t| t == "bug"));

    let output = repo
        .ti()
        .args(["assign", "-t", &id, "octocat@github.com", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["assigned"], "octocat@github.com");

    let output = repo
        .ti()
        .args(["points", "-t", &id, "8", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["points"], 8);

    let output = repo
        .ti()
        .args(["milestone", "-t", &id, "v2", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["milestone"], "v2");

    let output = repo
        .ti()
        .args(["comment", "-t", &id, "hello", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["comments"][0]["body"], "hello");

    let output = repo
        .ti()
        .args(["state", "blocked", "-t", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "open");
    assert_eq!(json["state"], "blocked");
    assert_eq!(json["closed_by"], Value::Null);

    let output = repo
        .ti()
        .args(["claim", "-t", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["assigned"], "tester@example.com");
    assert_eq!(json["status"], "open");
    assert_eq!(json["state"], "assigned");

    let output = repo
        .ti()
        .args(["checkout", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], id);
}

#[test]
fn state_and_status_commands_accept_status_state_and_combined_values() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "lifecycle ticket");

    let output = repo
        .ti()
        .args(["state", "closed", "-t", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "closed");
    assert_eq!(json["state"], "resolved");

    let output = repo
        .ti()
        .args(["status", "open:blocked", "-t", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "open");
    assert_eq!(json["state"], "blocked");

    let output = repo
        .ti()
        .args(["state", "closed:wontfix", "-t", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "closed");
    assert_eq!(json["state"], "wontfix");

    repo.ti()
        .args(["status", "review", "-t", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("open:review"));
}

#[test]
fn state_without_value_requires_tty_stdin() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "lifecycle menu");

    repo.ti()
        .args(["state", "-t", &id])
        .write_stdin("")
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing STATE"));
}

#[test]
fn state_without_value_rejects_json_without_explicit_state() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "json state");

    repo.ti()
        .args(["state", "-t", &id, "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--json"));
}

#[test]
fn checkout_makes_ticket_optional_for_show_and_comment() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "selected ticket");

    repo.ti().args(["checkout", &id[..6]]).assert().success();
    repo.ti()
        .args(["comment", "from", "current"])
        .assert()
        .success();

    repo.ti()
        .arg("show")
        .assert()
        .success()
        .stdout(predicate::str::contains("selected ticket"))
        .stdout(predicate::str::contains("from current"));
}

#[test]
fn close_resolves_current_ticket_and_clears_checkout() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "current close ticket");

    repo.ti().args(["checkout", &id]).assert().success();
    repo.ti()
        .arg("close")
        .assert()
        .success()
        .stdout(predicate::str::contains("cleared current ticket"));

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "closed");
    assert_eq!(json["state"], "resolved");

    repo.ti()
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("none checked out"));
}

#[test]
fn close_accepts_ticket_flag_like_state() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "close via flag");

    for flag in ["-t", "--ticket"] {
        let target = create_ticket(&repo, "flag target");
        let output = repo
            .ti()
            .args(["close", flag, &target, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["id"], target);
        assert_eq!(json["status"], "closed");
        assert_eq!(json["state"], "resolved");
    }

    // The positional form and the flag are mutually exclusive.
    repo.ti().args(["close", &id, "-t", &id]).assert().failure();
}

#[test]
fn close_explicit_ticket_keeps_other_checkout() {
    let repo = TestRepo::new();
    let current = create_ticket(&repo, "current ticket");
    let other = create_ticket(&repo, "other ticket");

    repo.ti().args(["checkout", &current]).assert().success();
    let output = repo
        .ti()
        .args(["close", &other, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], other);
    assert_eq!(json["status"], "closed");
    assert_eq!(json["state"], "resolved");

    repo.ti()
        .arg("show")
        .assert()
        .success()
        .stdout(predicate::str::contains("current ticket"));
}

#[test]
fn delete_removes_a_ticket_with_yes() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "doomed");
    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    // Gone from list.
    let out = repo
        .ti()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[test]
fn delete_without_yes_fails_non_interactively() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "keep me");
    repo.ti().args(["delete", &id]).assert().failure();
    // Still present.
    assert!(show_json(&repo, &id)["deleted"].is_null());
}

#[test]
fn delete_unknown_id_is_atomic() {
    let repo = TestRepo::new();
    let keep = create_ticket(&repo, "survivor");
    // Second arg is a bogus prefix; nothing should be deleted.
    repo.ti()
        .args(["delete", &keep, "ffffffff", "--yes"])
        .assert()
        .failure();
    assert!(show_json(&repo, &keep)["deleted"].is_null());
}

#[test]
fn delete_parent_keeps_its_sub_issues_linked() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    repo.ti()
        .args(["delete", &parent, "--yes"])
        .assert()
        .success();
    // Deletion is soft: the child survives, alive, still pointing at its
    // (deleted) parent, so `ti undelete` restores the tree without relinking.
    let t = show_json(&repo, &child);
    assert_eq!(t["parent"], parent.as_str());
    assert!(t["deleted"].is_null());
}

#[test]
fn delete_recursive_removes_subtree_including_closed() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");
    // Close the grandchild to prove recursive delete ignores status.
    repo.ti().args(["close", &grandchild]).assert().success();
    repo.ti()
        .args(["delete", &parent, "--recursive", "--yes"])
        .assert()
        .success();
    for id in [&parent, &child, &grandchild] {
        assert!(show_json(&repo, id)["deleted"].is_object(), "{id} deleted");
    }
    assert!(list_ids(&repo, &["--all"]).is_empty());
}

#[test]
fn delete_multiple_ids_in_one_invocation() {
    let repo = TestRepo::new();
    let a = create_ticket(&repo, "a");
    let b = create_ticket(&repo, "b");
    repo.ti()
        .args(["delete", &a, &b, "--yes"])
        .assert()
        .success();
    let out = repo
        .ti()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn delete_clears_checked_out_ticket() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "current");
    repo.ti().args(["checkout", &id]).assert().success();
    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    // After clearing, `ti show` with no id fails with the "none checked out"
    // message. If the session were NOT cleared, it would instead fail trying to
    // load the dangling current id — so asserting this specific message proves
    // the pointer was cleared, not left dangling.
    repo.ti()
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("none checked out"));
}

#[test]
fn delete_json_emits_array_of_deleted_tickets() {
    let repo = TestRepo::new();
    let a = create_ticket(&repo, "alpha");
    let b = create_ticket(&repo, "beta");
    let out = repo
        .ti()
        .args(["delete", &a, &b, "--yes", "--json"])
        .assert()
        .success()
        .stderr(predicate::eq(""))
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    assert!(!stdout.contains("\x1b["), "no ANSI in JSON: {stdout:?}");
    let arr: Value = serde_json::from_str(&stdout).unwrap();
    let ids: std::collections::BTreeSet<String> = arr
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, std::collections::BTreeSet::from([a, b]));
}

#[test]
fn delete_json_is_array_even_for_single_id() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "solo");
    let out = repo
        .ti()
        .args(["delete", &id, "--yes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(v.is_array());
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], id);
}

#[test]
fn delete_json_requires_yes() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "needs yes");
    repo.ti()
        .args(["delete", &id, "--json"])
        .assert()
        .failure()
        .stdout(predicate::eq(""));
    // Not deleted.
    assert!(show_json(&repo, &id)["deleted"].is_null());
}

#[test]
fn delete_markdown_lists_deleted_tickets() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "mark me");
    let out = repo
        .ti()
        .args(["delete", &id, "--yes", "--markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(out).unwrap();
    assert!(
        md.contains("# Deleted tickets"),
        "missing heading in:\n{md}"
    );
    assert!(md.contains("mark me"), "missing title in:\n{md}");
}

#[test]
fn new_checkout_selects_created_ticket() {
    let repo = TestRepo::new();

    repo.ti()
        .args(["new", "--title", "checked out on create", "--checkout"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Checked out:"));

    let output = repo
        .ti()
        .args(["show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["title"], "checked out on create");
}

#[test]
fn list_filters_and_saved_views_work() {
    let repo = TestRepo::new();
    let bug = create_ticket(&repo, "bug ticket");
    let docs = create_ticket(&repo, "docs ticket");

    repo.ti()
        .args(["tag", "-t", &bug, "bug"])
        .assert()
        .success();
    repo.ti()
        .args(["tag", "-t", &docs, "docs"])
        .assert()
        .success();

    repo.ti()
        .args(["list", "--tag", "bug"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bug ticket"))
        .stdout(predicate::str::contains("docs ticket").not());

    // Save the last list filters as a view.
    repo.ti().args(["views", "save", "bugs"]).assert().success();

    repo.ti()
        .args(["views"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bugs"))
        .stdout(predicate::str::contains("--tag bug"));

    // Load the view via `ti list <name>`.
    repo.ti()
        .args(["list", "bugs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bug ticket"))
        .stdout(predicate::str::contains("docs ticket").not());

    // Delete the view.
    repo.ti()
        .args(["views", "delete", "bugs"])
        .assert()
        .success();
}

#[test]
fn list_depends_on_filters_dependent_tickets() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");
    let unrelated = create_ticket(&repo, "unrelated");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--all", "--depends-on", &blocker, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], dependent);
    assert_ne!(tickets[0]["id"], unrelated);
}

#[test]
fn list_blocks_filters_blocker_tickets() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");
    let unrelated = create_ticket(&repo, "unrelated");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--all", "--blocks", &dependent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], blocker);
    assert_ne!(tickets[0]["id"], unrelated);
}

#[test]
fn list_relationship_filters_return_nothing_for_reversed_direction() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--all", "--depends-on", &dependent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert!(tickets.is_empty());

    let output = repo
        .ti()
        .args(["list", "--all", "--blocks", &blocker, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert!(tickets.is_empty());
}

#[test]
fn list_depends_on_includes_subissue_dependent_without_subissues_flag() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let blocker = create_ticket(&repo, "blocker");
    let sub_dependent = repo
        .ti()
        .args([
            "new",
            "--title",
            "sub dependent",
            "--subissue",
            &parent,
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let sub_dependent = String::from_utf8(sub_dependent).unwrap().trim().to_string();

    repo.ti()
        .args(["depends", "--ticket", &sub_dependent, &blocker])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--all", "--depends-on", &blocker, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], sub_dependent);
}

#[test]
fn list_blocks_includes_subissue_blocker_without_subissues_flag() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let dependent = create_ticket(&repo, "dependent");
    let sub_blocker = repo
        .ti()
        .args([
            "new",
            "--title",
            "sub blocker",
            "--subissue",
            &parent,
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let sub_blocker = String::from_utf8(sub_blocker).unwrap().trim().to_string();

    repo.ti()
        .args(["depends", "--ticket", &dependent, &sub_blocker])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--all", "--blocks", &dependent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], sub_blocker);
}

#[test]
fn list_blocks_hides_closed_blocker_by_default_and_shows_with_all() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    repo.ti().args(["close", &blocker]).assert().success();

    let output = repo
        .ti()
        .args(["list", "--blocks", &dependent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 0);

    let output = repo
        .ti()
        .args(["list", "--all", "--blocks", &dependent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], blocker);
}

#[test]
fn list_rejects_old_dependency_flag_names() {
    let repo = TestRepo::new();
    let ticket = create_ticket(&repo, "ticket");

    repo.ti()
        .args(["list", "--depends", &ticket])
        .assert()
        .failure();

    repo.ti()
        .args(["list", "--blocked-by", &ticket])
        .assert()
        .failure();
}

#[test]
fn list_blocks_rejects_unknown_ticket_reference() {
    let repo = TestRepo::new();

    repo.ti()
        .args(["list", "--blocks", "ffffffff-ffff-ffff-ffff-ffffffffffff"])
        .assert()
        .failure();
}

#[test]
fn list_blocks_view_save_and_replay_pins_full_id() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");
    let prefix = &dependent[..8];

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    repo.ti()
        .args(["list", "--all", "--blocks", prefix])
        .assert()
        .success();

    repo.ti()
        .args(["views", "save", "blockers"])
        .assert()
        .success();

    repo.ti()
        .args(["views"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("--blocks {dependent}")));

    let output = repo
        .ti()
        .args(["list", "blockers", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], blocker);
}

#[test]
fn at_sentinel_resolves_to_checked_out_ticket_in_show() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "the current one");

    repo.ti().args(["checkout", &id]).assert().success();

    let output = repo
        .ti()
        .args(["show", "@", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ticket: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(ticket["id"], id);
}

#[test]
fn at_sentinel_resolves_in_blocks_filter() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();
    // Check out the dependent; `--blocks @` should then mean `--blocks <dependent>`.
    repo.ti().args(["checkout", &dependent]).assert().success();

    let output = repo
        .ti()
        .args(["list", "--all", "--blocks", "@", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], blocker);
}

#[test]
fn at_sentinel_in_saved_view_pins_full_id() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");

    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();
    repo.ti().args(["checkout", &dependent]).assert().success();

    repo.ti()
        .args(["list", "--all", "--blocks", "@"])
        .assert()
        .success();
    repo.ti()
        .args(["views", "save", "blockers"])
        .assert()
        .success();

    // The view must pin the resolved full id, never the `@` sentinel.
    repo.ti()
        .args(["views"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("--blocks {dependent}")))
        .stdout(predicate::str::contains("--blocks @").not());
}

#[test]
fn at_sentinel_without_checkout_fails() {
    let repo = TestRepo::new();
    create_ticket(&repo, "a ticket exists but none is checked out");

    repo.ti()
        .args(["show", "@"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ti checkout"));
}

/// Build a blocker chain A depends on B depends on C and return (a, b, c).
fn deps_chain(repo: &TestRepo) -> (String, String, String) {
    let a = create_ticket(repo, "A");
    let b = create_ticket(repo, "B");
    let c = create_ticket(repo, "C");
    repo.ti()
        .args(["depends", "--ticket", &a, &b])
        .assert()
        .success();
    repo.ti()
        .args(["depends", "--ticket", &b, &c])
        .assert()
        .success();
    (a, b, c)
}

#[test]
fn deps_shows_transitive_blockers_tree() {
    let repo = TestRepo::new();
    let (a, b, c) = deps_chain(&repo);

    let output = repo
        .ti()
        .args(["deps", &a, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tree: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(tree["direction"], "blockers");
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], b);
    let grandchildren = nodes[0]["children"].as_array().unwrap();
    assert_eq!(grandchildren.len(), 1);
    assert_eq!(grandchildren[0]["id"], c);
}

#[test]
fn deps_dependents_flips_direction() {
    let repo = TestRepo::new();
    let (a, b, c) = deps_chain(&repo);

    let output = repo
        .ti()
        .args(["deps", &c, "--dependents", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tree: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(tree["direction"], "dependents");
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], b);
    assert_eq!(nodes[0]["children"][0]["id"], a);
}

#[test]
fn deps_prunes_closed_by_default_but_includes_with_all() {
    let repo = TestRepo::new();
    let (a, b, _c) = deps_chain(&repo);
    // B blocks on open C; --force closes it anyway (we're testing the walk).
    repo.ti().args(["close", &b, "--force"]).assert().success();

    // Default: closed B (and C beneath it) are pruned.
    let output = repo
        .ti()
        .args(["deps", &a, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tree: Value = serde_json::from_slice(&output).unwrap();
    assert!(tree["nodes"].as_array().unwrap().is_empty());

    // --all: B reappears and the walk continues.
    let output = repo
        .ti()
        .args(["deps", &a, "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tree: Value = serde_json::from_slice(&output).unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], b);
    assert_eq!(nodes[0]["status"], "closed");
}

#[test]
fn deps_resolves_at_sentinel() {
    let repo = TestRepo::new();
    let (a, b, _c) = deps_chain(&repo);
    repo.ti().args(["checkout", &a]).assert().success();

    let output = repo
        .ti()
        .args(["deps", "@", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tree: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(tree["ticket"], a);
    assert_eq!(tree["nodes"][0]["id"], b);
}

#[test]
fn deps_text_output_renders_tree() {
    let repo = TestRepo::new();
    let (a, _b, _c) = deps_chain(&repo);

    repo.ti()
        .args(["deps", &a])
        .assert()
        .success()
        .stdout(predicate::str::contains("A").or(predicate::str::contains("Blockers")));
}

#[test]
fn writeup_workflow_creates_versions_links_and_promotes() {
    let repo = TestRepo::new();
    let output = repo
        .ti()
        .args([
            "writeup",
            "new",
            "--title",
            "Rethink sync",
            "--body",
            "Initial notes",
            "--tags",
            "design",
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let writeup = String::from_utf8(output).unwrap().trim().to_string();
    let writeup_prefix = &writeup[..6];

    repo.ti()
        .args(["writeup", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(writeup_prefix))
        .stdout(predicate::str::contains("Rethink sync"))
        .stdout(predicate::str::contains("[design]"));

    repo.ti()
        .args(["writeup", "edit", writeup_prefix, "--body", "Second notes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Appended version 2"));

    repo.ti()
        .args(["tag", "--writeup", writeup_prefix, "review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("review"));
    repo.ti()
        .args(["tag", "--writeup", writeup_prefix, "--remove", "design"])
        .assert()
        .success();

    repo.ti()
        .args(["writeup", "show", writeup_prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Writeup: Rethink sync"))
        .stdout(predicate::str::contains("- Tags: review"))
        .stdout(predicate::str::contains("Second notes"))
        .stdout(predicate::str::contains("Initial notes").not());

    let ticket = create_ticket(&repo, "related ticket");
    repo.ti()
        .args(["writeup", "link", writeup_prefix, &ticket[..6]])
        .assert()
        .success();
    repo.ti()
        .args(["writeup", "show", writeup_prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains(&ticket));
    repo.ti()
        .args(["writeup", "unlink", writeup_prefix, &ticket[..6]])
        .assert()
        .success();

    let promoted_output = repo
        .ti()
        .args(["writeup", "promote", writeup_prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("Promoted writeup"))
        .get_output()
        .stdout
        .clone();
    let promoted_stdout = String::from_utf8(promoted_output).unwrap();
    let promoted_id = promoted_stdout
        .lines()
        .find_map(|line| line.strip_prefix("Full ticket id: "))
        .expect("promoted ticket id");
    repo.ti()
        .args(["show", promoted_id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Ticket: Rethink sync"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("Second notes"));

    repo.ti()
        .args(["writeup", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rethink sync").not());
    repo.ti()
        .args(["writeup", "close", writeup_prefix])
        .assert()
        .success();
    repo.ti()
        .args(["writeup", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rethink sync").not());
    repo.ti()
        .args(["writeup", "list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rethink sync"));
}

#[cfg(unix)]
#[test]
fn writeup_edit_editor_uses_first_line_as_title() {
    let repo = TestRepo::new();
    let output = repo
        .ti()
        .args([
            "writeup",
            "new",
            "--title",
            "Original title",
            "--body",
            "Original body",
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let writeup = String::from_utf8(output).unwrap().trim().to_string();
    let writeup_prefix = &writeup[..6];
    let editor = editor_script(&repo, "Updated title\n\nUpdated body");

    repo.ti()
        .env("EDITOR", editor)
        .args(["writeup", "edit", writeup_prefix])
        .assert()
        .success();

    repo.ti()
        .args(["writeup", "show", writeup_prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Writeup: Updated title"))
        .stdout(predicate::str::contains("Updated body"))
        .stdout(predicate::str::contains("Updated title\n\nUpdated title").not())
        .stdout(predicate::str::contains("Original body").not());
}

#[cfg(unix)]
#[test]
fn writeup_edit_editor_preserves_markdown_headings() {
    let repo = TestRepo::new();
    let output = repo
        .ti()
        .args([
            "writeup",
            "new",
            "--title",
            "Original title",
            "--body",
            "Original body",
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let writeup = String::from_utf8(output).unwrap().trim().to_string();
    let writeup_prefix = &writeup[..6];
    let editor = editor_script(
        &repo,
        "Updated title\n\n# First heading\n\nBody\n\n## Second heading",
    );

    repo.ti()
        .env("EDITOR", editor)
        .args(["writeup", "edit", writeup_prefix])
        .assert()
        .success();

    repo.ti()
        .args(["writeup", "show", writeup_prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("# First heading"))
        .stdout(predicate::str::contains("## Second heading"))
        .stdout(predicate::str::contains("Original body").not());
}

#[test]
fn list_search_filters_title_description_and_comments() {
    let repo = TestRepo::new();
    let title = create_ticket(&repo, "parser panic");
    let file = repo.state_file.path().join("description-ticket.md");
    fs::write(
        &file,
        "description ticket\n\nThis ticket explains parser recovery.\n",
    )
    .unwrap();
    let output = repo
        .ti()
        .args(["new", "-F"])
        .arg(&file)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let description: Value = serde_json::from_slice(&output).unwrap();
    let description_id = description["id"].as_str().unwrap().to_string();
    let comment = create_ticket(&repo, "comment ticket");
    repo.ti()
        .args(["comment", "-t", &comment, "parser appears in a comment"])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["list", "--search", "parser", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 3);

    let output = repo
        .ti()
        .args(["list", "--search", "title:parser", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], title);

    let output = repo
        .ti()
        .args(["list", "--search", "description:recovery", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], description_id);

    let output = repo
        .ti()
        .args(["list", "--search", "comments:appears", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], comment);
}

#[test]
fn list_search_multi_term_and() {
    let repo = TestRepo::new();
    let both = create_ticket(&repo, "fix login timeout on retry");
    let _one = create_ticket(&repo, "login screen polish");

    let output = repo
        .ti()
        .args(["list", "--search", "login timeout", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], both);
}

#[test]
fn list_search_quoted_phrase() {
    let repo = TestRepo::new();
    let phrase = create_ticket(&repo, "fix login timeout bug");
    let _apart = create_ticket(&repo, "login on the timeout screen");

    let output = repo
        .ti()
        .args(["list", "--search", "\"login timeout\"", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], phrase);
}

#[test]
fn list_search_is_unicode_case_insensitive() {
    let repo = TestRepo::new();
    let hit = create_ticket(&repo, "gestione città");

    let output = repo
        .ti()
        .args(["list", "--search", "CITTÀ", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["id"], hit);
}

#[test]
fn list_search_highlights_in_text_output_but_not_json() {
    let repo = TestRepo::new();
    create_ticket(&repo, "fix login timeout");

    // Human table output carries the bold+yellow highlight escape.
    let text = repo
        .ti()
        .args(["list", "--search", "login"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("\u{1b}[1m\u{1b}[33m"));

    // JSON output is ANSI-free.
    let json = repo
        .ti()
        .args(["list", "--search", "login", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json = String::from_utf8(json).unwrap();
    assert!(!json.contains('\u{1b}'));
}

#[test]
fn show_search_highlights_detail() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "fix login timeout");

    let text = repo
        .ti()
        .args(["show", &id, "--search", "login"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("\u{1b}[1m\u{1b}[33m"));

    // show --json stays ANSI-free even with --search.
    let json = repo
        .ti()
        .args(["show", &id, "--search", "login", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json = String::from_utf8(json).unwrap();
    assert!(!json.contains('\u{1b}'));
}

#[test]
fn list_all_includes_non_open_tickets() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "closed ticket");
    repo.ti()
        .args(["state", "resolved", "-t", &id])
        .assert()
        .success();

    repo.ti()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("closed ticket").not());

    let output = repo
        .ti()
        .args(["list", "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json.as_array().unwrap()[0]["title"], "closed ticket");
}

#[test]
#[cfg(unix)]
fn import_gh_creates_tickets_and_skips_existing_issues() {
    let repo = TestRepo::new();
    let bin = repo.state_file.path().join("bin");
    executable_script(
        &bin,
        "gh",
        r#"#!/bin/sh
cat <<'JSON'
[
  {
    "number": 7,
    "title": "first gh issue",
    "body": "Imported body",
    "url": "https://github.com/owner/repo/issues/7",
    "author": {"login": "monalisa"},
    "labels": [{"name": "bug"}],
    "assignees": [{"login": "octocat"}, {"login": "hubot"}],
    "milestone": {"title": "v1"}
  },
  {
    "number": 8,
    "title": "second gh issue",
    "body": "",
    "url": "https://github.com/owner/repo/issues/8",
    "author": {"login": "hubot"},
    "labels": [],
    "assignees": [],
    "milestone": null
  }
]
JSON
"#,
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        env::var_os("PATH").unwrap_or_default().to_string_lossy()
    );

    repo.ti()
        .env("PATH", &path)
        .args(["import", "gh", "--repo", "owner/repo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported 2 GitHub issue(s)."));

    let output = repo
        .ti()
        .args(["list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let tickets = json.as_array().unwrap();
    assert_eq!(tickets.len(), 2);

    let first = tickets
        .iter()
        .find(|ticket| ticket["title"] == "first gh issue")
        .unwrap();
    assert_eq!(first["assigned"], "octocat@users.noreply.github.com");
    assert_eq!(first["milestone"], "v1");
    assert_eq!(
        first["description"],
        "GitHub issue: https://github.com/owner/repo/issues/7\nGitHub author: monalisa\nGitHub assignees: octocat, hubot\n\nImported body"
    );
    let tags = first["tags"].as_array().unwrap();
    assert!(tags.iter().any(|tag| tag == "github"));
    assert!(tags.iter().any(|tag| tag == "github-issue-7"));
    assert!(tags.iter().any(|tag| tag == "bug"));

    repo.ti()
        .env("PATH", &path)
        .args(["import", "gh", "--repo", "owner/repo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported 0 GitHub issue(s)."))
        .stdout(predicate::str::contains(
            "Skipped 2 issue(s) that were already imported.",
        ));
}

/// Create a ticket that is a sub-issue of `parent`, returning its id.
fn create_subissue(repo: &TestRepo, parent: &str, title: &str) -> String {
    let out = repo
        .ti()
        .args(["new", "--title", title, "--subissue", parent, "--id-only"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn close_rejected_with_open_subissue_then_force_succeeds() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let child_short: String = child.chars().take(6).collect();

    // Plain close is rejected and names the open sub-issue.
    repo.ti()
        .args(["close", &parent])
        .assert()
        .failure()
        .stderr(predicate::str::contains("open sub-issue"))
        .stderr(predicate::str::contains(child_short))
        .stderr(predicate::str::contains("--force"));

    // Parent is still open.
    let output = repo
        .ti()
        .args(["show", &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "open");

    // --force overrides.
    repo.ti()
        .args(["close", &parent, "--force"])
        .assert()
        .success();
    let output = repo
        .ti()
        .args(["show", &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "closed");
}

#[test]
fn state_close_wontfix_rejected_with_open_subissue() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    create_subissue(&repo, &parent, "child");

    // Any close transition is guarded, not just resolve.
    repo.ti()
        .args(["state", "closed:wontfix", "-t", &parent])
        .assert()
        .failure()
        .stderr(predicate::str::contains("open sub-issue"));
}

#[test]
fn close_succeeds_after_subissue_resolved() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");

    repo.ti().args(["close", &child]).assert().success();
    repo.ti().args(["close", &parent]).assert().success();
}

#[test]
fn show_resolves_relationships_to_title_and_status() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent feature");
    let child = create_subissue(&repo, &parent, "child task");
    let blocker = create_ticket(&repo, "the blocker");
    repo.ti()
        .args(["depends", "--ticket", &parent, &blocker])
        .assert()
        .success();

    // Parent shows its child and dependency with title + status, not bare hex.
    repo.ti()
        .args(["show", &parent])
        .assert()
        .success()
        .stdout(predicate::str::contains("child task").and(predicate::str::contains("[open]")))
        .stdout(predicate::str::contains("the blocker"));

    // Markdown output carries the same relationships (was previously omitted).
    repo.ti()
        .args(["show", &parent, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("- Children:").and(predicate::str::contains("child task")))
        .stdout(predicate::str::contains("- Depends on:"));

    // The child points back at its parent by title.
    repo.ti()
        .args(["show", &child])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parent").and(predicate::str::contains("parent feature")));
}

#[test]
fn show_json_includes_recursive_subissues() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");

    let out = repo
        .ti()
        .args(["show", &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["id"], parent);
    assert!(v["children"].is_array()); // flat UUID array preserved
    assert_eq!(v["subissues"][0]["id"], child);
    assert_eq!(v["subissues"][0]["subissues"][0]["id"], grandchild);
}

#[test]
fn show_text_shows_subissue_tree() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    create_subissue(&repo, &parent, "child");

    let out = repo
        .ti()
        .args(["show", &parent])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Sub-issues:"), "missing header in:\n{text}");
    assert!(text.contains("child"), "missing child in:\n{text}");
}

#[test]
fn show_markdown_shows_subissue_tree() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    create_subissue(&repo, &parent, "child");

    let out = repo
        .ti()
        .args(["show", &parent, "--markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("## Sub-issues"), "missing header in:\n{text}");
    assert!(text.contains("child"), "missing child in:\n{text}");
}

#[test]
fn close_rejected_with_open_dependency_then_force_succeeds() {
    let repo = TestRepo::new();
    let dependent = create_ticket(&repo, "dependent");
    let blocker = create_ticket(&repo, "the blocker");
    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    // Closing the dependent is rejected while the blocker is open.
    repo.ti()
        .args(["close", &dependent])
        .assert()
        .failure()
        .stderr(predicate::str::contains("open dependenc"))
        .stderr(predicate::str::contains("--force"));

    // Closing the blocker itself is allowed (the normal case).
    repo.ti().args(["close", &blocker]).assert().success();
    // Now the dependent closes cleanly.
    repo.ti().args(["close", &dependent]).assert().success();
}

#[test]
fn list_parent_shows_only_direct_children() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent feature");
    let child_a = create_subissue(&repo, &parent, "child a");
    let child_b = create_subissue(&repo, &parent, "child b");
    create_ticket(&repo, "unrelated");

    let output = repo
        .ti()
        .args(["list", "--parent", &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    // Exactly the two children, surfaced despite sub-issues being hidden by default.
    let ids: BTreeSet<String> = tickets
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&child_a));
    assert!(ids.contains(&child_b));
    assert!(!ids.contains(&parent));
}

#[test]
fn new_with_depends_on_creates_dependency() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "the blocker");

    let output = repo
        .ti()
        .args([
            "new",
            "--title",
            "dependent",
            "--depends-on",
            &blocker,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    let deps = json["depends_on"].as_array().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0], blocker);

    // Reverse side is set: the blocker now blocks the new ticket.
    let new_id = json["id"].as_str().unwrap();
    let output = repo
        .ti()
        .args(["show", &blocker, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let blocker_json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(blocker_json["blocks"].as_array().unwrap()[0], new_id);
}

#[test]
fn new_with_bad_depends_on_fails_before_creating() {
    let repo = TestRepo::new();
    repo.ti()
        .args(["new", "--title", "x", "--depends-on", "nonexistent"])
        .assert()
        .failure();
    // Nothing was created.
    let output = repo
        .ti()
        .args(["list", "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert!(tickets.is_empty());
}

#[test]
fn new_subissue_under_closed_parent_is_rejected() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    repo.ti().args(["close", &parent]).assert().success();

    repo.ti()
        .args(["new", "--title", "late child", "--subissue", &parent])
        .assert()
        .failure()
        .stderr(predicate::str::contains("closed ticket"));
}

#[test]
fn writeup_show_and_list_json() {
    let repo = TestRepo::new();
    let out = repo
        .ti()
        .args([
            "writeup",
            "new",
            "--title",
            "Big idea",
            "--body",
            "first cut",
            "--id-only",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let id = String::from_utf8(out).unwrap().trim().to_string();

    let output = repo
        .ti()
        .args(["writeup", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["title"], "Big idea");
    assert_eq!(json["status"], "open");
    let versions = json["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["body"], "first cut");

    let output = repo
        .ti()
        .args(["writeup", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let arr: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"], "Big idea");
}

#[test]
fn depends_json_includes_the_dependency_counterpart() {
    let repo = TestRepo::new();
    let dependent = create_ticket(&repo, "dependent");
    let blocker = create_ticket(&repo, "the blocker");

    let output = repo
        .ti()
        .args(["depends", "--ticket", &dependent, &blocker, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ticket"]["id"], dependent);
    assert_eq!(json["dependency"]["id"], blocker);
    assert_eq!(json["dependency"]["title"], "the blocker");
}

#[test]
fn subissue_json_includes_the_parent_counterpart() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent feature");
    let child = create_ticket(&repo, "child");

    let output = repo
        .ti()
        .args(["subissue", "--ticket", &child, &parent, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ticket"]["id"], child);
    assert_eq!(json["parent"]["id"], parent);
    assert_eq!(json["parent"]["title"], "parent feature");
}

/// Fetch one field of a ticket via `ti show --json`.
fn show_field(repo: &TestRepo, id: &str, key: &str) -> Value {
    let output = repo
        .ti()
        .args(["show", id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    json[key].clone()
}

#[test]
fn field_clear_mutations_round_trip() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "fields");

    repo.ti()
        .args(["priority", "2", "-t", &id])
        .assert()
        .success();
    assert_eq!(show_field(&repo, &id, "priority"), 2);
    repo.ti()
        .args(["priority", "--clear", "-t", &id])
        .assert()
        .success();
    assert!(show_field(&repo, &id, "priority").is_null());

    repo.ti()
        .args(["milestone", "v1", "-t", &id])
        .assert()
        .success();
    assert_eq!(show_field(&repo, &id, "milestone"), "v1");
    repo.ti()
        .args(["milestone", "--clear", "-t", &id])
        .assert()
        .success();
    assert!(show_field(&repo, &id, "milestone").is_null());

    repo.ti()
        .args(["spec", "some notes", "-t", &id])
        .assert()
        .success();
    assert_eq!(show_field(&repo, &id, "spec"), "some notes");
    repo.ti()
        .args(["spec", "--clear", "-t", &id])
        .assert()
        .success();
    assert!(show_field(&repo, &id, "spec").is_null());
}

#[test]
fn code_set_clear_and_invalid_rejected() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "code ticket");

    repo.ti()
        .args(["code", "https://github.com/o/r:main", "-t", &id])
        .assert()
        .success();
    assert_eq!(
        show_field(&repo, &id, "code"),
        "https://github.com/o/r:main"
    );
    repo.ti()
        .args(["code", "--clear", "-t", &id])
        .assert()
        .success();
    assert!(show_field(&repo, &id, "code").is_null());

    // A URI without an http(s) scheme is rejected.
    repo.ti()
        .args(["code", "not-a-valid-uri", "-t", &id])
        .assert()
        .failure();
}

#[test]
fn pull_transfers_tickets_between_repos() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    // Repo A creates a ticket and pushes it.
    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let id = create_ticket(&a, "shared ticket");
    a.ti().arg("push").assert().success();

    // Repo B pulls from the same remote and sees the ticket.
    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().args(["pull", &remote_url]).assert().success();

    let output = b
        .ti()
        .args(["list", "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tickets: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert!(tickets
        .iter()
        .any(|t| t["id"] == id && t["title"] == "shared ticket"));
}

#[test]
fn pull_applies_fork_parent_change_to_op_based_ticket() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let first = create_ticket(&a, "first parent");
    let second = create_ticket(&a, "second parent");
    let child = create_subissue(&a, &first, "child");
    a.ti().arg("push").assert().success();

    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().args(["pull", &remote_url]).assert().success();
    // A local edit gives the imported ticket an operation log.
    b.ti()
        .args(["priority", "2", "-t", &child])
        .assert()
        .success();

    // The fork re-parents the child and publishes.
    a.ti()
        .args(["subissue", &second, "-t", &child])
        .assert()
        .success();
    a.ti().arg("push").assert().success();

    b.ti().args(["pull", &remote_url]).assert().success();

    let out = b
        .ti()
        .args(["show", &child, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let t: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(t["parent"], second);
    assert_eq!(t["priority"], 2);

    let out = b
        .ti()
        .args(["verify", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["ok"], true);
}

#[test]
fn history_shows_readable_entries_without_raw_ops() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "hist ticket");
    repo.ti()
        .args(["state", "in-progress", "-t", &id])
        .assert()
        .success();
    repo.ti().args(["close", "-t", &id]).assert().success();

    let out = repo.ti().args(["history", "-t", &id]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("in-progress"), "{text}");
    assert!(text.contains("closed"), "{text}");
    assert!(!text.contains("ops:"), "raw op key leaked: {text}");
    assert!(!text.contains("{\""), "raw JSON leaked: {text}");

    let json = repo
        .ti()
        .args(["history", "-t", &id, "--json"])
        .output()
        .unwrap();
    let entries: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(entries
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["field"] == "status" && e["value"] == "closed"));
}

#[test]
fn stats_counts_closed_by_close_time() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "to close");
    repo.ti().args(["close", "-t", &id]).assert().success();
    let out = repo.ti().args(["stats", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["closed_7d"], 1);
}

#[test]
fn stats_history_recent_and_mine_run() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "a ticket");

    repo.ti()
        .arg("stats")
        .assert()
        .success()
        .stdout(predicate::str::contains("STATS"));
    repo.ti().args(["history", "-t", &id]).assert().success();
    repo.ti().arg("recent").assert().success();
    // `mine` uses git user.email; the ticket isn't assigned to the tester,
    // so this is empty but must still succeed.
    repo.ti().arg("mine").assert().success();
}

/// Run `ti next --json` and parse the result.
fn next_json(repo: &TestRepo) -> Value {
    let output = repo
        .ti()
        .args(["next", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn next_skips_tickets_with_open_dependencies() {
    let repo = TestRepo::new();
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");
    repo.ti()
        .args(["depends", "--ticket", &dependent, &blocker])
        .assert()
        .success();

    // The dependent has an open dependency, so next picks the blocker.
    assert_eq!(next_json(&repo)["id"], blocker);

    // Once the blocker is resolved, the dependent becomes workable.
    repo.ti().args(["close", &blocker]).assert().success();
    assert_eq!(next_json(&repo)["id"], dependent);
}

#[test]
fn next_prefers_lower_priority_number() {
    let repo = TestRepo::new();
    let lo = create_ticket(&repo, "low priority");
    let hi = create_ticket(&repo, "high priority");
    repo.ti()
        .args(["priority", "5", "-t", &lo])
        .assert()
        .success();
    repo.ti()
        .args(["priority", "1", "-t", &hi])
        .assert()
        .success();

    assert_eq!(next_json(&repo)["id"], hi);
}

#[test]
fn next_numeric_priority_outranks_none_even_when_large() {
    let repo = TestRepo::new();
    // `_plain` is created first, so it is older and would win the created-at
    // tie-break while both are unprioritised.
    let _plain = create_ticket(&repo, "plain-older");
    let numbered = create_ticket(&repo, "numbered-newer");
    // A deliberately large number is NOT a demotion: `none` is the
    // least-important band, so any numeric priority ranks above it.
    repo.ti()
        .args(["priority", "100", "-t", &numbered])
        .assert()
        .success();

    assert_eq!(
        next_json(&repo)["id"],
        numbered,
        "a numeric priority (even 100) must rank above unprioritised tickets"
    );
}

#[test]
fn next_excludes_deferred_tag_unless_included() {
    let repo = TestRepo::new();
    // `parked` is older, so without exclusion it would win the tie-break.
    let parked = create_ticket(&repo, "parked-older");
    let active = create_ticket(&repo, "active-newer");
    repo.ti()
        .args(["tag", "-t", &parked, "deferred"])
        .assert()
        .success();

    // deferred-tagged `parked` is excluded; `active` is picked.
    assert_eq!(next_json(&repo)["id"], active);

    // --include-deferred restores it; being older it now wins the tie.
    let out = repo
        .ti()
        .args(["next", "--include-deferred", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["id"], parked);
}

#[test]
fn next_blocked_sorts_after_actionable() {
    let repo = TestRepo::new();
    // `blocked` is older and would otherwise win the created-at tie-break.
    let blocked = create_ticket(&repo, "blocked-older");
    let active = create_ticket(&repo, "active-newer");
    repo.ti()
        .args(["state", "blocked", "-t", &blocked])
        .assert()
        .success();

    // blocked is not excluded, but sorts last among states, so `active` wins.
    assert_eq!(next_json(&repo)["id"], active);
}

#[test]
fn next_prefers_open_subissue_over_parent() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");

    // The parent is a container: its open sub-issue is picked instead.
    assert_eq!(next_json(&repo)["id"], child);
}

#[test]
fn next_walks_subissues_then_returns_parent() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let grandchild = create_subissue(&repo, &child, "grandchild");

    assert_eq!(next_json(&repo)["id"], grandchild);
    repo.ti().args(["close", &grandchild]).assert().success();
    assert_eq!(next_json(&repo)["id"], child);
    repo.ti().args(["close", &child]).assert().success();
    // All sub-issues done: the parent itself is now workable.
    assert_eq!(next_json(&repo)["id"], parent);
}

#[test]
fn next_json_includes_recursive_subissues_of_leaf() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    repo.ti().args(["close", &child]).assert().success();

    // Parent is next again; closed sub-issues stay out of the tree.
    let v = next_json(&repo);
    assert_eq!(v["id"], parent);
    assert_eq!(v["subissues"], serde_json::json!([]));
}

#[test]
fn next_json_is_null_when_nothing_workable() {
    let repo = TestRepo::new();
    let output = repo
        .ti()
        .args(["next", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json["next"].is_null());
}

#[test]
fn users_add_list_and_remove() {
    let repo = TestRepo::new();
    repo.ti()
        .args(["users", "add", "scott", "scott@example.com"])
        .assert()
        .success();
    repo.ti()
        .args(["users", "add", "scott", "chacon@example.com"])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["users", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["scott"].as_array().unwrap().len(), 2);

    // Remove one email.
    repo.ti()
        .args(["users", "rm", "scott", "chacon@example.com"])
        .assert()
        .success();
    let output = repo
        .ti()
        .args(["users", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["scott"].as_array().unwrap().len(), 1);

    // Remove the whole user.
    repo.ti().args(["users", "rm", "scott"]).assert().success();
    let output = repo
        .ti()
        .args(["users", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json.get("scott").is_none());
}

#[test]
fn assign_resolves_user_nick_to_email() {
    let repo = TestRepo::new();
    repo.ti()
        .args(["users", "add", "scott", "scott@example.com"])
        .assert()
        .success();
    let id = create_ticket(&repo, "task");

    repo.ti()
        .args(["assign", "scott", "-t", &id])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["assigned"], "scott@example.com");
}

#[test]
fn hook_install_check_and_uninstall_git_target() {
    let repo = TestRepo::new();
    repo.ti()
        .args(["hook", "check", "--target", "git"])
        .assert()
        .failure();

    repo.ti()
        .args(["hook", "install", "--target", "git"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pre-push"));
    let hook = repo.dir.path().join(".git/hooks/pre-push");
    let text = fs::read_to_string(&hook).unwrap();
    assert!(text.starts_with("#!/usr/bin/env sh\n"));
    assert!(text.contains("ti hook run pre-push"));

    repo.ti()
        .args(["hook", "check", "--target", "git"])
        .assert()
        .success();
    repo.ti()
        .args(["hook", "install", "--target", "git"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&hook).unwrap(), text);

    repo.ti()
        .args(["hook", "uninstall", "--target", "git"])
        .assert()
        .success();
    assert!(!hook.exists());
}

#[test]
fn hook_install_detects_husky_and_keeps_existing_steps() {
    let repo = TestRepo::new();
    let husky = repo.dir.path().join(".husky");
    fs::create_dir_all(&husky).unwrap();
    fs::write(husky.join("pre-push"), "#!/usr/bin/env sh\nnpm test\n").unwrap();

    repo.ti().args(["hook", "install"]).assert().success();
    let text = fs::read_to_string(husky.join("pre-push")).unwrap();
    assert!(text.find("npm test").unwrap() < text.find("ti hook run").unwrap());

    repo.ti().args(["hook", "uninstall"]).assert().success();
    assert_eq!(
        fs::read_to_string(husky.join("pre-push")).unwrap(),
        "#!/usr/bin/env sh\nnpm test\n"
    );
}

#[test]
fn hook_run_warns_without_failing_and_respects_the_recursion_guard() {
    let repo = TestRepo::new();
    let line = format!(
        "refs/heads/main {} refs/heads/main {}\n",
        "1".repeat(40),
        "0".repeat(40)
    );
    // No remote configured: nothing to sync, still exits 0 and stays quiet.
    repo.ti()
        .args(["hook", "run", "pre-push", "origin", "url"])
        .write_stdin(line.clone())
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    repo.ti()
        .args(["hook", "run", "pre-push", "origin", "url"])
        .env("TI_SYNC_IN_PROGRESS", "1")
        .write_stdin(line)
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[test]
fn migrate_dry_run_reports_current_tickets_as_json() {
    let repo = TestRepo::new();
    create_ticket(&repo, "a ticket");

    let output = repo
        .ti()
        .args(["migrate", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    // A freshly-created ticket is already stamped at the current format, so a
    // dry run finds nothing to change.
    assert_eq!(json["applied"], false);
    assert_eq!(json["total"], 1);
    assert_eq!(json["changed"], 0);
    assert_eq!(json["current_format"], 2);
    assert!(json["tickets"].as_array().unwrap().len() == 1);
}

#[test]
fn verify_reports_ok_for_healthy_repo_as_json() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "a ticket");
    repo.ti()
        .args(["priority", "2", "-t", &id])
        .assert()
        .success();

    let output = repo
        .ti()
        .args(["verify", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["failed"], 0);
    assert!(json["total"].as_u64().unwrap() >= 1);
}

#[test]
fn concurrent_edits_to_different_fields_both_survive_across_clones() {
    // The op-log's headline: two clones editing DIFFERENT scalar fields of the
    // same ticket both keep their edit after a sync, instead of one clobbering
    // the other under last-write-wins.
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    // Clone A creates the ticket and seeds the remote.
    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let id = create_ticket(&a, "shared ticket");
    a.ti().arg("push").assert().success();

    // Clone B pulls the ticket down.
    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().arg("sync").assert().success();

    // Concurrent edits: A sets priority, B sets milestone (different fields).
    a.ti().args(["priority", "1", "-t", &id]).assert().success();
    b.ti()
        .args(["milestone", "v1", "-t", &id])
        .assert()
        .success();

    // Gossip to the fixpoint. The op-log is a CRDT: convergence is guaranteed
    // once both op sets have propagated, which takes a couple of sync rounds
    // (a clone can push its edit before it has received the other's).
    a.ti().arg("push").assert().success();
    b.ti().arg("sync").assert().success();
    a.ti().arg("sync").assert().success();
    b.ti().arg("sync").assert().success();

    // Both clones now see BOTH edits — neither clobbered the other.
    for repo in [&a, &b] {
        let output = repo
            .ti()
            .args(["show", &id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["priority"], 1, "priority edit must survive the merge");
        assert_eq!(
            json["milestone"], "v1",
            "milestone edit must survive the merge"
        );
    }
}

// ---------------------------------------------------------------------------
// Linked git worktrees must share a single ticket store (git-meta.sqlite lives
// under the common git-dir, not the per-worktree git-dir), and `ti reindex`
// must be able to re-project that store from refs/meta/*.
// ---------------------------------------------------------------------------

/// A linked worktree reads the same tickets as the main checkout.
#[test]
fn linked_worktree_sees_main_checkout_tickets() {
    let repo = TestRepo::new();
    create_ticket(&repo, "main-tkt");

    let wt = tempfile::tempdir().expect("worktree tempdir");
    let wt_path = wt.path().join("linked");
    git(
        repo.dir.path(),
        &["worktree", "add", "-q", wt_path.to_str().unwrap(), "HEAD"],
    );

    repo.ti_in(&wt_path)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("main-tkt"));
}

/// A ticket created inside a linked worktree is visible from the main checkout,
/// even after the worktree is removed — because both share one store.
#[test]
fn ticket_created_in_worktree_is_visible_in_main_checkout() {
    let repo = TestRepo::new();
    create_ticket(&repo, "main-tkt");

    let wt = tempfile::tempdir().expect("worktree tempdir");
    let wt_path = wt.path().join("linked");
    git(
        repo.dir.path(),
        &["worktree", "add", "-q", wt_path.to_str().unwrap(), "HEAD"],
    );

    let wt_output = repo
        .ti_in(&wt_path)
        .args(["new", "--title", "wt-tkt", "--id-only"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let wt_id = String::from_utf8(wt_output).unwrap().trim().to_string();

    git(
        repo.dir.path(),
        &["worktree", "remove", "--force", wt_path.to_str().unwrap()],
    );

    repo.ti()
        .args(["show", &wt_id, "--json"])
        .assert()
        .success();
    repo.ti()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wt-tkt"));
}

/// `ti reindex` rebuilds the local SQLite store from the git-meta refs after the
/// store file is lost, recovering tickets that still live in refs/meta/*.
#[test]
fn reindex_rebuilds_store_from_meta_refs() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    create_ticket(&repo, "durable ticket");
    // Push to create the remote ref, then sync so the tracking ref
    // refs/meta/remotes/main exists locally.
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Lose the local projection, leaving only refs/meta/*.
    let git_dir = repo.dir.path().join(".git");
    for name in [
        "git-meta.sqlite",
        "git-meta.sqlite-wal",
        "git-meta.sqlite-shm",
    ] {
        let _ = fs::remove_file(git_dir.join(name));
    }

    repo.ti().arg("reindex").assert().success();

    repo.ti()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("durable ticket"));
}

/// `ti reindex` self-heals the blobless-partial-clone failure mode: when a
/// ticket referenced by the remote meta ref cannot be materialized because its
/// objects are missing locally, reindex backfills them from the meta remote and
/// re-materializes, recovering the ticket instead of aborting. (It still never
/// silently under-counts — anything it cannot recover is reported as skipped.)
#[test]
fn reindex_backfills_missing_objects_and_recovers_the_ticket() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    create_ticket(&repo, "alpha ticket");
    let b_id = create_ticket(&repo, "beta ticket");
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Simulate a store whose meta objects for ticket B are absent locally:
    // delete B's (loose) blob objects from the odb. A small fetch keeps objects
    // loose by default (fetch.unpackLimit), so they are individually removable.
    let listing = git_output(
        repo.dir.path(),
        &[
            "ls-tree",
            "-r",
            &format!("refs/meta/remotes/main:project/ticgit/tickets/{b_id}"),
        ],
    );
    let objects_dir = repo.dir.path().join(".git/objects");
    let mut deleted = 0;
    for line in listing.lines() {
        // "<mode> blob <oid>\t<path>"
        if let Some(oid) = line.split_whitespace().nth(2) {
            let path = objects_dir.join(&oid[..2]).join(&oid[2..]);
            if fs::remove_file(&path).is_ok() {
                deleted += 1;
            }
        }
    }
    assert!(deleted > 0, "test must delete at least one of B's objects");

    // Lose the local projection so reindex rebuilds from refs/meta/* — where B
    // now has missing objects.
    let git_dir = repo.dir.path().join(".git");
    for name in [
        "git-meta.sqlite",
        "git-meta.sqlite-wal",
        "git-meta.sqlite-shm",
    ] {
        let _ = fs::remove_file(git_dir.join(name));
    }

    // Reindex now backfills B's objects from origin (the bare remote still has
    // them) and re-materializes, so it succeeds and both tickets are present —
    // no manual fetch, no abort.
    repo.ti().arg("reindex").assert().success();
    repo.ti().args(["list"]).assert().success().stdout(
        predicate::str::contains("alpha ticket").and(predicate::str::contains("beta ticket")),
    );
}

/// When a ticket's objects are missing AND cannot be backfilled (no reachable
/// meta remote), `ti reindex` fails loudly pointing at the manual backfill
/// command, rather than silently producing a store that under-counts. (It
/// cannot keep only the healthy tickets — git-meta's materialize is
/// all-or-nothing — so loud failure is the safe outcome.)
#[test]
fn reindex_fails_loudly_when_a_ticket_is_unrecoverable() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    create_ticket(&repo, "alpha ticket");
    let b_id = create_ticket(&repo, "beta ticket");
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Delete B's blob objects locally.
    let listing = git_output(
        repo.dir.path(),
        &[
            "ls-tree",
            "-r",
            &format!("refs/meta/remotes/main:project/ticgit/tickets/{b_id}"),
        ],
    );
    let objects_dir = repo.dir.path().join(".git/objects");
    let mut deleted = 0;
    for line in listing.lines() {
        if let Some(oid) = line.split_whitespace().nth(2) {
            let path = objects_dir.join(&oid[..2]).join(&oid[2..]);
            if fs::remove_file(&path).is_ok() {
                deleted += 1;
            }
        }
    }
    assert!(deleted > 0, "test must delete at least one of B's objects");
    for name in [
        "git-meta.sqlite",
        "git-meta.sqlite-wal",
        "git-meta.sqlite-shm",
    ] {
        let _ = fs::remove_file(repo.dir.path().join(".git").join(name));
    }

    // Remove the remote so no backfill source remains — B is unrecoverable.
    git(repo.dir.path(), &["remote", "remove", "origin"]);

    // With no way to recover B's objects, reindex fails loudly and points at
    // the manual backfill command rather than silently dropping tickets.
    repo.ti()
        .arg("reindex")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--refetch").and(predicate::str::contains("blob:limit")));
}

// ---------------------------------------------------------------------------
// Push guard: a push must never implicitly delete remote tickets as a side
// effect of serializing an incomplete local store (ticket fe3981 / parent
// 81f1c6). Removal of remote data is allowed only via an explicit --allow-deletes.
// ---------------------------------------------------------------------------

/// A bare `ti push` from a stranded store (tickets still in refs/meta/* but
/// missing from the local projection) must refuse rather than clobber the
/// remote. `ti push` does not pull first, so nothing heals the store before the
/// push — the guard is the only thing standing between it and data loss.
#[test]
fn push_from_stranded_store_refuses_to_clobber_remote_tickets() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    create_ticket(&repo, "alpha ticket");
    create_ticket(&repo, "beta ticket");
    // Seed the remote and fetch the tracking ref refs/meta/remotes/main.
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Strand the store: drop the local projection but keep refs/meta/* and the
    // local serialization ref, so a serialize sees an empty store.
    let git_dir = repo.dir.path().join(".git");
    for name in [
        "git-meta.sqlite",
        "git-meta.sqlite-wal",
        "git-meta.sqlite-shm",
    ] {
        let _ = fs::remove_file(git_dir.join(name));
    }

    // The buggy behavior: push serializes the empty store and deletes both
    // remote tickets. The guard must abort.
    repo.ti()
        .arg("push")
        .assert()
        .failure()
        .stderr(predicate::str::contains("reindex"));

    // Prove the remote data survived: reindex and both tickets reappear.
    repo.ti().arg("reindex").assert().success();
    repo.ti().args(["list"]).assert().success().stdout(
        predicate::str::contains("alpha ticket").and(predicate::str::contains("beta ticket")),
    );
}

/// A `ti sync` from a stranded store self-heals: the pull notices the index is
/// missing tickets that still live in refs/meta/* and reindexes them, instead
/// of reporting "no new tickets" and then pushing an empty store. No abort, no
/// data loss — the store is made honest.
#[test]
fn sync_from_stranded_store_self_heals_via_pull_reindex() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    create_ticket(&repo, "alpha ticket");
    create_ticket(&repo, "beta ticket");
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Strand the store.
    let git_dir = repo.dir.path().join(".git");
    for name in [
        "git-meta.sqlite",
        "git-meta.sqlite-wal",
        "git-meta.sqlite-shm",
    ] {
        let _ = fs::remove_file(git_dir.join(name));
    }

    // Sync succeeds by recovering the stranded tickets during the pull.
    repo.ti().arg("sync").assert().success();
    repo.ti().args(["list"]).assert().success().stdout(
        predicate::str::contains("alpha ticket").and(predicate::str::contains("beta ticket")),
    );
}

/// Every overwriting push first snapshots the prior remote tip to a local
/// backup ref (outside refs/meta/* so reindex never replays it), and the
/// snapshots are pruned to a fixed bound. This is what made the real incident
/// recoverable — now the tool does it automatically.
#[test]
fn sync_snapshots_remote_tip_before_overwriting_and_prunes() {
    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    let id = create_ticket(&repo, "snapshot me");
    repo.ti().arg("push").assert().success();
    repo.ti().arg("sync").assert().success();

    // Many overwriting edit+sync cycles. Each pull refreshes the tracking ref
    // to the current remote tip; the following push snapshots it before
    // overwriting.
    for i in 1..=14 {
        repo.ti()
            .args(["milestone", &format!("v{i}"), "-t", &id])
            .assert()
            .success();
        repo.ti().arg("sync").assert().success();
    }

    let names = git_output(
        repo.dir.path(),
        &["for-each-ref", "--format=%(refname)", "refs/ticgit-backup/"],
    );
    let count = names.lines().count();
    assert!(
        count >= 1,
        "overwriting syncs must leave recoverable snapshots"
    );
    assert!(
        count <= 10,
        "snapshots must be pruned to the bound, got {count}: {names}"
    );

    // A snapshot is a real git-meta commit whose tree still holds the ticket —
    // i.e. the prior remote state is genuinely recoverable from it.
    let snap = names.lines().next().unwrap();
    let tree = git_output(
        repo.dir.path(),
        &[
            "ls-tree",
            "--name-only",
            &format!("{snap}:project/ticgit/tickets"),
        ],
    );
    assert!(
        tree.contains(&id),
        "snapshot {snap} must contain the ticket tree; got {tree}"
    );
}

/// `ti sync` runs `ti verify` before pushing and refuses a store that fails
/// integrity, so a corrupt store can never overwrite the shared remote. The
/// --no-verify escape hatch lets intentional recovery flows through.
#[test]
fn sync_refuses_to_push_a_store_that_fails_verify() {
    use ticgit_lib::{Session, Target, TicketStore};

    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    let id = create_ticket(&repo, "corruptible");
    repo.ti().arg("push").assert().success();

    // Corrupt an op in the store so it no longer replays to a valid state:
    // overwrite one op key's value with an envelope whose id matches neither
    // the key hash nor its own recomputed content-id.
    {
        let session = Session::open(repo.dir.path().join(".git")).expect("open session");
        let store = TicketStore::from_session(session).expect("from_session");
        let p = store.session().target(&Target::project());
        let pairs = p.get_all_values(Some("ticgit:tickets")).expect("scan");
        let (key, _) = pairs
            .into_iter()
            .find(|(k, _)| k.contains(":ops:"))
            .expect("an op key");
        p.set(
            &key,
            "{\"id\":\"deadbeef\",\"lamport\":0,\"author\":\"x@y.z\",\
             \"created_at\":\"1970-01-01T00:00:00Z\",\"format_version\":1,\
             \"kind\":\"set-field\",\"payload\":{\"fields\":{\"title\":\"evil\"}}}",
        )
        .expect("tamper op");
    }

    // Sync refuses, naming verify and the offending ticket.
    repo.ti()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicate::str::contains("verify").and(predicate::str::contains(&id[..6])));

    // The escape hatch lets it through.
    repo.ti().args(["sync", "--no-verify"]).assert().success();
}

/// A plain `ti push` must share `ti sync`'s integrity gate: a store that fails
/// `ti verify` cannot overwrite the shared remote, and `--no-verify` is the
/// only way through. `ti push` does not pull first, so without this gate it is
/// the easiest path to clobber the remote with a corrupt store.
#[test]
fn push_refuses_to_push_a_store_that_fails_verify() {
    use ticgit_lib::{Session, Target, TicketStore};

    let repo = TestRepo::new();
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    git(repo.dir.path(), &["remote", "add", "origin", &remote_url]);
    repo.ti().arg("init").assert().success();

    let id = create_ticket(&repo, "corruptible");
    repo.ti().arg("push").assert().success();

    // Corrupt an op so the ticket no longer replays to a valid state.
    {
        let session = Session::open(repo.dir.path().join(".git")).expect("open session");
        let store = TicketStore::from_session(session).expect("from_session");
        let p = store.session().target(&Target::project());
        let pairs = p.get_all_values(Some("ticgit:tickets")).expect("scan");
        let (key, _) = pairs
            .into_iter()
            .find(|(k, _)| k.contains(":ops:"))
            .expect("an op key");
        p.set(
            &key,
            "{\"id\":\"deadbeef\",\"lamport\":0,\"author\":\"x@y.z\",\
             \"created_at\":\"1970-01-01T00:00:00Z\",\"format_version\":1,\
             \"kind\":\"set-field\",\"payload\":{\"fields\":{\"title\":\"evil\"}}}",
        )
        .expect("tamper op");
    }

    // A bare push refuses, naming verify and the offending ticket.
    repo.ti()
        .arg("push")
        .assert()
        .failure()
        .stderr(predicate::str::contains("verify").and(predicate::str::contains(&id[..6])));

    // The escape hatch lets it through.
    repo.ti().args(["push", "--no-verify"]).assert().success();
}

/// A real deletion still propagates when the operator opts in with
/// --allow-deletes; without the flag the same push is refused.
#[test]
fn push_refuses_deletion_without_flag_but_allows_it_with_flag() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    // Clone A seeds three tickets on the remote.
    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    create_ticket(&a, "keep me");
    let doomed = create_ticket(&a, "delete me");
    let gone = create_ticket(&a, "vanish me");
    a.ti().arg("push").assert().success();
    a.ti().arg("sync").assert().success();

    // `ti delete` is an operation that keeps the ticket's keys, so it never
    // trips the implicit-deletion guard.
    a.ti().args(["delete", &doomed, "-y"]).assert().success();
    a.ti().arg("push").assert().success();

    // A ticket whose keys vanished (a stranded store, or an older ti that
    // removed keys) looks like an implicit deletion and is refused with
    // actionable guidance.
    remove_ticket_keys(&a, &gone);
    a.ti()
        .arg("push")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--allow-deletes"));

    // With the opt-in flag the key removal propagates to the remote.
    a.ti().args(["push", "--allow-deletes"]).assert().success();

    // A fresh clone lists "keep me" only; "delete me" is there but deleted.
    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().arg("sync").assert().success();
    b.ti().args(["list", "--all"]).assert().success().stdout(
        predicate::str::contains("keep me")
            .and(predicate::str::contains("delete me").not())
            .and(predicate::str::contains("vanish me").not()),
    );
    b.ti()
        .args(["list", "--deleted"])
        .assert()
        .success()
        .stdout(predicate::str::contains("delete me"));
}

/// Remove every key of a ticket directly in the store, the way a stranded
/// index or a `ti delete` from ti <= 0.4.2 left it.
fn remove_ticket_keys(repo: &TestRepo, id: &str) {
    use ticgit_lib::{Session, Target, TicketStore};
    let session = Session::open(repo.dir.path().join(".git")).expect("open session");
    let store = TicketStore::from_session(session).expect("from_session");
    let p = store.session().target(&Target::project());
    let id = uuid::Uuid::parse_str(id).expect("uuid");
    for (key, _) in p
        .get_all_values(Some(&ticgit_lib::keys::ticket_prefix(&id)))
        .expect("ticket keys")
    {
        p.remove(&key).expect("remove key");
    }
}

// ---------------------------------------------------------------------------
// Residue tolerance: a `ti new` that fails after stamping the `format-version`
// marker (e.g. an unknown assignee) leaves an op-less "shell" record in the
// store. verify must treat that as deletion/creation residue, not corruption,
// and reindex must prune it rather than letting it survive forever.
// ---------------------------------------------------------------------------

/// A create that aborts after writing `format-version` (here, an unknown
/// assignee nick) leaves a shell ticket with no `create` op. `ti verify` must
/// not flag it as corrupt — it is not a ticket — and the real ticket stays.
#[test]
fn verify_ignores_create_failure_residue() {
    let repo = TestRepo::new();
    repo.ti().arg("init").assert().success();
    create_ticket(&repo, "real ticket");

    // Fails after the format-version marker is written → shell record left.
    repo.ti()
        .args(["new", "-t", "ghosty", "-a", "zzz-no-such-nick"])
        .assert()
        .failure();

    // The shell must not be reported as a verification failure.
    repo.ti().arg("verify").assert().success();
    repo.ti().args(["list"]).assert().success().stdout(
        predicate::str::contains("real ticket").and(predicate::str::contains("ghosty").not()),
    );
}

/// `ti reindex` is additive (ADR 0003): it gains a ticket the index lost from
/// refs/meta/* and leaves everything the index holds alone, so a valid
/// local-only ticket and an unsynced comment on a pushed ticket survive it.
/// (Pruning ghost residue belongs to the wholesale `reindex_reset`.)
#[test]
fn reindex_gains_lost_tickets_and_keeps_unsynced_local_work() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();
    let repo = clone_of(&remote_url);

    // Titles are kept short so the list table does not truncate them.
    let backed = create_ticket(&repo, "backedtkt");
    repo.ti()
        .args(["comment", "-t", &backed, "pushed comment"])
        .assert()
        .success();
    let lost = create_ticket(&repo, "losttkt");
    repo.ti().arg("sync").assert().success();

    create_ticket(&repo, "localtkt");
    repo.ti()
        .args(["comment", "-t", &backed, "unsynced comment"])
        .assert()
        .success();
    strand_ticket(&repo, &lost);

    repo.ti().arg("reindex").assert().success();

    repo.ti().arg("verify").assert().success();
    repo.ti().args(["list"]).assert().success().stdout(
        predicate::str::contains("backedtkt")
            .and(predicate::str::contains("losttkt"))
            .and(predicate::str::contains("localtkt")),
    );
    assert_eq!(
        comment_bodies(&show_json(&repo, &backed)),
        vec!["pushed comment", "unsynced comment"]
    );
}

#[test]
fn sync_retries_when_remote_advances_between_pull_and_push() {
    // Concurrency regression: clone B pushes in the window between clone A's
    // pull (fetch) and A's push. git rejects A's push as non-fast-forward.
    // ticgit used to swallow that rejection and print "Push: N ticket(s)
    // synced. Done." with exit 0, so A's edit never reached the remote and a
    // third clone syncing afterwards never saw it. The push must instead
    // merge the remote tip and retry, landing A's edit.
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let id = create_ticket(&a, "shared ticket");
    a.ti().arg("sync").assert().success();

    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().arg("sync").assert().success();

    // A's pre-push hook runs inside A's `git push`, i.e. after A's pull has
    // already fetched: that is exactly the race window. The first time it
    // fires it makes B edit and sync, advancing the remote tip underneath A.
    let ti_bin = assert_cmd::cargo::cargo_bin("ti");
    let fired = a.dir.path().join("hook-fired");
    let hook = format!(
        "#!/usr/bin/env bash\n\
         [ -e '{fired}' ] && exit 0\n\
         touch '{fired}'\n\
         cd '{b_dir}'\n\
         export TICGIT_STATE_FILE='{b_state}'\n\
         '{ti}' priority 2 -t {id} >/dev/null\n\
         '{ti}' sync >/dev/null\n\
         exit 0\n",
        fired = fired.display(),
        b_dir = b.dir.path().display(),
        b_state = b.state_file.path().join("state.json").display(),
        ti = ti_bin.display(),
    );
    executable_script(&a.dir.path().join(".git/hooks"), "pre-push", &hook);

    a.ti()
        .args(["state", "in-progress", "-t", &id])
        .assert()
        .success();
    a.ti()
        .arg("sync")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "remote advanced during the push; merged and retried (1x)",
        ));
    assert!(fired.exists(), "the race hook must have fired");

    // A's edit must be on the remote tip, merged with B's.
    let c = TestRepo::new();
    git(c.dir.path(), &["remote", "add", "origin", &remote_url]);
    c.ti().arg("init").assert().success();
    c.ti().arg("sync").assert().success();
    let output = c
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        json["state"], "in-progress",
        "A's edit was lost by the race"
    );
    assert_eq!(json["priority"], 2, "B's edit was clobbered by the retry");
}

#[test]
fn push_with_stale_tracking_ref_merges_instead_of_silently_failing() {
    // `ti push` against a remote that moved since the last fetch: the push
    // must land the local edit (merged), never report success while git
    // rejected it as non-fast-forward.
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let id = create_ticket(&a, "shared ticket");
    a.ti().arg("sync").assert().success();

    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().arg("sync").assert().success();
    b.ti().args(["priority", "2", "-t", &id]).assert().success();
    b.ti().arg("sync").assert().success();

    // A's tracking ref is now behind the remote.
    a.ti()
        .args(["state", "in-progress", "-t", &id])
        .assert()
        .success();
    let output = a
        .ti()
        .args(["push", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        json["push_retries"], 1,
        "the push must report the merge+retry"
    );

    // B's pull reports the existing ticket as updated, not "no new tickets".
    let output = b
        .ti()
        .args(["sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["pulled"].as_array().unwrap().len(), 0);
    assert_eq!(json["updated"][0]["id"], id);
    assert_eq!(json["push_retries"], 0);
    let output = b
        .ti()
        .args(["show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        json["state"], "in-progress",
        "A's push was silently rejected"
    );
    assert_eq!(json["priority"], 2);
}

// ---------------------------------------------------------------------------
// Half-present relations (ticket 458402): a Sub-issue or Dependency whose two
// denormalized sides disagree — e.g. after a sync dropped one side. Removal
// must be idempotent so the CLI can repair it, and `ti verify` must flag it.
// ---------------------------------------------------------------------------

/// Remove `member` from the set at `ticgit:tickets:<id>:<field>` directly in
/// the store, leaving the other side of the relation in place.
fn drop_relation_side(repo: &TestRepo, id: &str, field: &str, member: &str) {
    use ticgit_lib::{Session, Target, TicketStore};
    let session = Session::open(repo.dir.path().join(".git")).expect("open session");
    let store = TicketStore::from_session(session).expect("from_session");
    let p = store.session().target(&Target::project());
    let id = uuid::Uuid::parse_str(id).expect("uuid");
    p.set_remove(&ticgit_lib::keys::ticket_field(&id, field), member)
        .expect("drop relation side");
}

fn show_json(repo: &TestRepo, id: &str) -> Value {
    let out = repo
        .ti()
        .args(["show", id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("show json")
}

fn json_strings(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn subissue_clear_and_relink_repair_a_half_present_subissue() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    drop_relation_side(&repo, &parent, "children", &child);
    assert_eq!(show_json(&repo, &child)["parent"], parent.as_str());
    assert!(json_strings(&show_json(&repo, &parent)["children"]).is_empty());

    repo.ti()
        .args(["subissue", "--clear", "-t", &child])
        .assert()
        .success();
    assert!(show_json(&repo, &child)["parent"].is_null());

    // Re-break it, then repair by relinking to the same parent.
    repo.ti()
        .args(["subissue", &parent, "-t", &child])
        .assert()
        .success();
    drop_relation_side(&repo, &parent, "children", &child);
    repo.ti()
        .args(["subissue", &parent, "-t", &child])
        .assert()
        .success();
    assert_eq!(show_json(&repo, &child)["parent"], parent.as_str());
    assert_eq!(
        json_strings(&show_json(&repo, &parent)["children"]),
        vec![child]
    );
}

#[test]
fn depends_remove_repairs_a_half_present_dependency() {
    let repo = TestRepo::new();
    let a = create_ticket(&repo, "dependent");
    let b = create_ticket(&repo, "blocker");
    repo.ti().args(["depends", &b, "-t", &a]).assert().success();
    drop_relation_side(&repo, &b, "blocks", &a);
    assert_eq!(
        json_strings(&show_json(&repo, &a)["depends_on"]),
        vec![b.clone()]
    );
    assert!(json_strings(&show_json(&repo, &b)["blocks"]).is_empty());

    repo.ti()
        .args(["depends", &b, "-t", &a, "--remove"])
        .assert()
        .success();
    assert!(json_strings(&show_json(&repo, &a)["depends_on"]).is_empty());
    assert!(json_strings(&show_json(&repo, &b)["blocks"]).is_empty());

    // The mirror case: only the blocks side survived.
    repo.ti().args(["depends", &b, "-t", &a]).assert().success();
    drop_relation_side(&repo, &a, "depends_on", &b);
    repo.ti()
        .args(["depends", &b, "-t", &a, "--remove"])
        .assert()
        .success();
    assert!(json_strings(&show_json(&repo, &a)["depends_on"]).is_empty());
    assert!(json_strings(&show_json(&repo, &b)["blocks"]).is_empty());
}

#[test]
fn verify_reports_each_half_present_relation_naming_both_tickets() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let a = create_ticket(&repo, "dependent");
    let b = create_ticket(&repo, "blocker");
    repo.ti().args(["depends", &b, "-t", &a]).assert().success();
    repo.ti().arg("verify").assert().success();

    drop_relation_side(&repo, &parent, "children", &child);
    drop_relation_side(&repo, &b, "blocks", &a);

    let out = repo
        .ti()
        .args(["verify", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&out).expect("verify json");
    assert_eq!(report["ok"], false);
    let issues: Vec<String> = report["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| json_strings(&t["issues"]))
        .collect();
    assert_eq!(issues.len(), 2, "one issue per asymmetry: {issues:?}");
    let names_both = |x: &str, y: &str| {
        issues
            .iter()
            .any(|i| i.contains(&x[..6]) && i.contains(&y[..6]))
    };
    assert!(names_both(&parent, &child), "{issues:?}");
    assert!(names_both(&a, &b), "{issues:?}");

    // Human output goes to stderr with the same issues and a non-zero exit.
    repo.ti()
        .arg("verify")
        .assert()
        .failure()
        .stderr(predicate::str::contains(&child[..6]).and(predicate::str::contains(&a[..6])));

    // Repairing both relations through the CLI makes verify clean again.
    repo.ti()
        .args(["subissue", &parent, "-t", &child])
        .assert()
        .success();
    repo.ti().args(["depends", &b, "-t", &a]).assert().success();
    repo.ti().arg("verify").assert().success();
}

#[test]
fn verify_reports_a_parent_listing_a_sub_issue_that_has_no_parent() {
    use ticgit_lib::{Session, Target, TicketStore};
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let sub = create_ticket(&repo, "sub-issue");
    {
        let session = Session::open(repo.dir.path().join(".git")).expect("open session");
        let store = TicketStore::from_session(session).expect("from_session");
        let p = store.session().target(&Target::project());
        let pid = uuid::Uuid::parse_str(&parent).unwrap();
        p.set_add(&ticgit_lib::keys::ticket_field(&pid, "children"), &sub)
            .expect("add stray children member");
    }

    repo.ti()
        .arg("verify")
        .assert()
        .failure()
        .stderr(predicate::str::contains(&parent[..6]).and(predicate::str::contains(&sub[..6])));

    // Documented repair: point it at the parent named in the issue, then clear.
    repo.ti()
        .args(["subissue", &parent, "-t", &sub])
        .assert()
        .success();
    repo.ti()
        .args(["subissue", "--clear", "-t", &sub])
        .assert()
        .success();
    assert!(show_json(&repo, &sub)["parent"].is_null());
    assert!(json_strings(&show_json(&repo, &parent)["children"]).is_empty());
    repo.ti().arg("verify").assert().success();
}

// ---------------------------------------------------------------------------
// Deletion is an operation (ticket 88f85c, ADR 0003)
// ---------------------------------------------------------------------------

fn list_ids(repo: &TestRepo, extra: &[&str]) -> BTreeSet<String> {
    let mut args = vec!["list", "--json"];
    args.extend_from_slice(extra);
    let out = repo
        .ti()
        .args(&args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list: Value = serde_json::from_slice(&out).expect("list json");
    list.as_array()
        .expect("array")
        .iter()
        .map(|t| t["id"].as_str().expect("id").to_string())
        .collect()
}

#[test]
fn deleted_ticket_is_hidden_from_list_and_list_deleted_shows_only_it() {
    let repo = TestRepo::new();
    let doomed = create_ticket(&repo, "doomed");
    let alive = create_ticket(&repo, "alive");
    repo.ti()
        .args(["delete", &doomed, "--yes"])
        .assert()
        .success();

    assert_eq!(list_ids(&repo, &[]), BTreeSet::from([alive.clone()]));
    assert_eq!(list_ids(&repo, &["--all"]), BTreeSet::from([alive]));
    assert_eq!(
        list_ids(&repo, &["--deleted"]),
        BTreeSet::from([doomed.clone()])
    );
    repo.ti()
        .args(["list", "--deleted"])
        .assert()
        .success()
        .stdout(predicate::str::contains("doomed"))
        .stdout(predicate::str::contains("alive").not());
}

#[test]
fn delete_json_reports_the_deletion_and_a_second_delete_fails() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "twice");
    let out = repo
        .ti()
        .args(["delete", &id, "--yes", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v[0]["deleted"]["by"], "tester@example.com");
    repo.ti()
        .args(["delete", &id, "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already deleted"));
}

#[test]
fn show_on_deleted_ticket_exits_zero_with_a_deleted_banner() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "ghost");
    repo.ti().args(["delete", &id, "--yes"]).assert().success();

    repo.ti()
        .args(["show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted"))
        .stdout(predicate::str::contains("tester@example.com"))
        .stdout(predicate::str::contains("ti undelete"));
    repo.ti()
        .args(["show", &id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("**Deleted**"))
        .stdout(predicate::str::contains("tester@example.com"));

    let json = show_json(&repo, &id);
    assert_eq!(json["deleted"]["by"], "tester@example.com");
    let at = json["deleted"]["at"].as_str().expect("deleted.at string");
    assert!(
        at.contains('T') && at.len() >= 20,
        "rfc3339 timestamp: {at}"
    );
}

#[test]
fn live_ticket_show_has_no_deleted_banner() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "alive and well");
    repo.ti()
        .args(["show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted").not());
    repo.ti()
        .args(["show", &id, "--markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("**Deleted**").not());
}

#[test]
fn checkout_refuses_a_deleted_ticket() {
    let repo = TestRepo::new();
    let id = create_ticket(&repo, "ghost");
    repo.ti().args(["delete", &id, "--yes"]).assert().success();
    repo.ti()
        .args(["checkout", &id])
        .assert()
        .failure()
        .stdout(predicate::eq(""))
        .stderr(predicate::str::contains("deleted"))
        .stderr(predicate::str::contains("ti undelete"));
    // Nothing was checked out.
    repo.ti()
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("none checked out"));
}

#[test]
fn delete_writes_no_set_changes_on_parent_or_dependency_tickets() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let blocker = create_ticket(&repo, "blocker");
    let dependent = create_ticket(&repo, "dependent");
    repo.ti()
        .args(["depends", "-t", &child, &blocker])
        .assert()
        .success();
    repo.ti()
        .args(["depends", "-t", &dependent, &child])
        .assert()
        .success();
    let before: Vec<Value> = [&parent, &blocker, &dependent]
        .iter()
        .map(|id| show_json(&repo, id))
        .collect();

    repo.ti()
        .args(["delete", &child, "--yes"])
        .assert()
        .success();

    let after: Vec<Value> = [&parent, &blocker, &dependent]
        .iter()
        .map(|id| show_json(&repo, id))
        .collect();
    // `subissues` is a derived, open-only view; the stored sets must not move.
    for (b, a) in before.iter().zip(&after) {
        for field in ["children", "depends_on", "blocks", "parent", "deleted"] {
            assert_eq!(b[field], a[field], "{field} of {} changed", b["title"]);
        }
    }
}

#[test]
fn undelete_restores_the_ticket_with_comments_tags_and_relations_intact() {
    let repo = TestRepo::new();
    let parent = create_ticket(&repo, "parent");
    let child = create_subissue(&repo, &parent, "child");
    let blocker = create_ticket(&repo, "blocker");
    repo.ti()
        .args(["depends", "-t", &child, &blocker])
        .assert()
        .success();
    repo.ti()
        .args(["comment", "-t", &child, "keep this note"])
        .assert()
        .success();
    repo.ti()
        .args(["tag", "-t", &child, "important"])
        .assert()
        .success();
    let before = show_json(&repo, &child);

    repo.ti()
        .args(["delete", &child, "--yes"])
        .assert()
        .success();
    let out = repo
        .ti()
        .args(["undelete", &child, "--json"])
        .assert()
        .success()
        .stderr(predicate::eq(""))
        .get_output()
        .stdout
        .clone();
    let restored: Value = serde_json::from_slice(&out).expect("undelete json");
    assert_eq!(restored["id"], child.as_str());
    assert!(restored["deleted"].is_null());

    let after = show_json(&repo, &child);
    assert_eq!(after, before);
    assert_eq!(after["comments"][0]["body"], "keep this note");
    assert_eq!(json_strings(&after["tags"]), vec!["important".to_string()]);
    assert_eq!(after["parent"], parent.as_str());
    assert_eq!(json_strings(&after["depends_on"]), vec![blocker.clone()]);
    assert_eq!(
        json_strings(&show_json(&repo, &parent)["children"]),
        vec![child.clone()]
    );
    assert_eq!(
        json_strings(&show_json(&repo, &blocker)["blocks"]),
        vec![child.clone()]
    );
    assert!(list_ids(&repo, &["--subissues"]).contains(&child));

    // Undeleting a live ticket is refused (a stray undelete would outrank a
    // concurrent delete from another clone).
    repo.ti()
        .args(["undelete", &child])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not deleted"));
}

#[test]
fn deletion_travels_to_other_clones_and_keeps_edits_made_before_they_sync() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = TestRepo::new();
    git(a.dir.path(), &["remote", "add", "origin", &remote_url]);
    a.ti().arg("init").assert().success();
    let id = create_ticket(&a, "shared ticket");
    let keep = create_ticket(&a, "bystander");
    a.ti().arg("sync").assert().success();

    let b = TestRepo::new();
    git(b.dir.path(), &["remote", "add", "origin", &remote_url]);
    b.ti().arg("init").assert().success();
    b.ti().arg("sync").assert().success();
    assert!(list_ids(&b, &[]).contains(&id));

    // A deletes and syncs; B, not yet synced, comments on the ticket.
    a.ti().args(["delete", &id, "--yes"]).assert().success();
    a.ti().arg("sync").assert().success();
    b.ti()
        .args(["comment", "-t", &id, "written before B synced"])
        .assert()
        .success();
    b.ti().arg("sync").assert().success();

    // B now hides the ticket; the comment landed on the hidden ticket.
    assert_eq!(list_ids(&b, &[]), BTreeSet::from([keep.clone()]));
    assert_eq!(list_ids(&b, &["--deleted"]), BTreeSet::from([id.clone()]));
    let hidden = show_json(&b, &id);
    assert!(hidden["deleted"].is_object(), "deleted on B: {hidden}");
    assert_eq!(hidden["comments"][0]["body"], "written before B synced");

    // Undelete (on A, after it picks up B's comment): visible everywhere,
    // comment intact.
    a.ti().arg("sync").assert().success();
    a.ti().args(["undelete", &id]).assert().success();
    a.ti().arg("sync").assert().success();
    b.ti().arg("sync").assert().success();
    for clone in [&a, &b] {
        assert!(list_ids(clone, &[]).contains(&id));
        let t = show_json(clone, &id);
        assert!(t["deleted"].is_null());
        assert_eq!(t["comments"][0]["body"], "written before B synced");
    }
}

// ---------------------------------------------------------------------------
// Pull self-heal is additive (ticket f4ceb3, incident 215ee3, ADR 0003)
// ---------------------------------------------------------------------------

/// A fresh clone with `origin` pointing at `remote_url` and `ti init` run.
fn clone_of(remote_url: &str) -> TestRepo {
    let repo = TestRepo::new();
    git(repo.dir.path(), &["remote", "add", "origin", remote_url]);
    repo.ti().arg("init").assert().success();
    repo
}

/// Drop every index row of ticket `id` from `repo`'s `git-meta.sqlite`, the
/// way a lost or partially rebuilt index loses it: no tombstone, no log entry,
/// and `refs/meta/*` untouched.
fn strand_ticket(repo: &TestRepo, id: &str) {
    let db = rusqlite::Connection::open(repo.dir.path().join(".git/git-meta.sqlite"))
        .expect("open git-meta.sqlite");
    let pattern = format!("ticgit:tickets:{id}:%");
    let rows = "SELECT rowid FROM metadata WHERE key LIKE ?1";
    db.execute(
        &format!("DELETE FROM list_values WHERE metadata_id IN ({rows})"),
        [&pattern],
    )
    .expect("drop list rows");
    db.execute(
        &format!("DELETE FROM set_values WHERE metadata_id IN ({rows})"),
        [&pattern],
    )
    .expect("drop set rows");
    let dropped = db
        .execute("DELETE FROM metadata WHERE key LIKE ?1", [&pattern])
        .expect("drop metadata rows");
    assert!(dropped > 0, "ticket {id} had no index rows");
}

fn comment_bodies(ticket: &Value) -> Vec<String> {
    ticket["comments"]
        .as_array()
        .expect("comments array")
        .iter()
        .map(|c| c["body"].as_str().expect("body").to_string())
        .collect()
}

/// Incident 215ee3: a clone with unsynced comments, Sub-issues, Dependencies
/// and Deletions runs `ti sync` against a remote that still holds the state of
/// its previous sync, while its index has lost a ticket the remote holds, so
/// the pull's stranded-index self-heal runs. Nothing local may be dropped, no
/// deleted ticket may come back, and the stranded ticket is recovered, on this
/// clone and on the next one to sync.
#[test]
fn sync_keeps_every_unsynced_local_change_from_the_incident() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = clone_of(&remote_url);
    let map = create_ticket(&a, "map ticket");
    let old_sub = create_subissue(&a, &map, "old sub-issue");
    let talked = create_ticket(&a, "talked about");
    a.ti()
        .args(["comment", "-t", &talked, "first remote comment"])
        .assert()
        .success();
    let dependent = create_ticket(&a, "dependent");
    let blocker = create_ticket(&a, "blocker");
    let doomed_one = create_ticket(&a, "doomed one");
    let doomed_two = create_ticket(&a, "doomed two");
    let stranded = create_ticket(&a, "stranded");
    a.ti()
        .args(["comment", "-t", &stranded, "kept by the remote"])
        .assert()
        .success();
    a.ti().arg("sync").assert().success();

    let b = clone_of(&remote_url);
    b.ti().arg("sync").assert().success();

    // Unsynced work in A.
    a.ti()
        .args(["comment", "-t", &talked, "second, local-only comment"])
        .assert()
        .success();
    let new_sub_one = create_subissue(&a, &map, "new sub-issue one");
    let new_sub_two = create_subissue(&a, &map, "new sub-issue two");
    a.ti()
        .args(["depends", "-t", &dependent, &blocker])
        .assert()
        .success();
    a.ti()
        .args(["depends", "-t", &talked, &blocker])
        .assert()
        .success();
    a.ti()
        .args(["delete", &doomed_one, &doomed_two, "--yes"])
        .assert()
        .success();
    // The index loses a ticket the remote still holds, so this pull runs the
    // stranded-index self-heal while all of the work above is unsynced.
    strand_ticket(&a, &stranded);
    assert!(!list_ids(&a, &[]).contains(&stranded));

    a.ti().arg("sync").assert().success();
    b.ti().arg("sync").assert().success();

    for clone in [&a, &b] {
        let t = show_json(clone, &talked);
        assert_eq!(
            comment_bodies(&t),
            vec!["first remote comment", "second, local-only comment"],
            "comments: {t}"
        );
        assert_eq!(json_strings(&t["depends_on"]), vec![blocker.clone()]);

        let children: BTreeSet<String> = json_strings(&show_json(clone, &map)["children"])
            .into_iter()
            .collect();
        assert_eq!(
            children,
            BTreeSet::from([old_sub.clone(), new_sub_one.clone(), new_sub_two.clone()])
        );
        assert_eq!(
            json_strings(&show_json(clone, &dependent)["depends_on"]),
            vec![blocker.clone()]
        );
        let blocks: BTreeSet<String> = json_strings(&show_json(clone, &blocker)["blocks"])
            .into_iter()
            .collect();
        assert_eq!(blocks, BTreeSet::from([dependent.clone(), talked.clone()]));

        let live = list_ids(clone, &[]);
        assert!(!live.contains(&doomed_one) && !live.contains(&doomed_two));
        assert_eq!(
            list_ids(clone, &["--deleted"]),
            BTreeSet::from([doomed_one.clone(), doomed_two.clone()])
        );
        assert_eq!(
            comment_bodies(&show_json(clone, &stranded)),
            vec!["kept by the remote"]
        );
        clone.ti().arg("verify").assert().success();
    }
}

/// A Stranded index the remote has moved on from: the merge brings in only
/// what another clone appended to the lost ticket since, so the ticket is
/// partly indexed. The pull still restores its earlier operations and
/// comments, keeps this clone's unsynced edit, and the push leaves every
/// operation on the remote.
#[test]
fn sync_restores_a_stranded_ticket_the_remote_appended_to() {
    let remote = tempfile::tempdir().expect("bare remote tempdir");
    git(remote.path(), &["init", "--bare", "--quiet"]);
    let remote_url = remote.path().to_string_lossy().to_string();

    let a = clone_of(&remote_url);
    let lost = create_ticket(&a, "lost then appended");
    a.ti()
        .args(["comment", "-t", &lost, "from before the loss"])
        .assert()
        .success();
    let other = create_ticket(&a, "edited meanwhile");
    a.ti()
        .args(["comment", "-t", &other, "synced in A"])
        .assert()
        .success();
    a.ti().arg("sync").assert().success();

    let b = clone_of(&remote_url);
    b.ti().arg("sync").assert().success();
    b.ti()
        .args(["priority", "1", "-t", &lost])
        .assert()
        .success();
    b.ti().arg("sync").assert().success();

    strand_ticket(&a, &lost);
    a.ti()
        .args(["comment", "-t", &other, "unsynced in A"])
        .assert()
        .success();
    a.ti().arg("sync").assert().success();
    b.ti().arg("sync").assert().success();

    for clone in [&a, &b] {
        let t = show_json(clone, &lost);
        assert_eq!(t["title"], "lost then appended", "{t}");
        assert_eq!(t["priority"], 1, "{t}");
        assert_eq!(comment_bodies(&t), vec!["from before the loss"]);
        assert_eq!(
            comment_bodies(&show_json(clone, &other)),
            vec!["synced in A", "unsynced in A"]
        );
        clone.ti().arg("verify").assert().success();
    }
}
