// `fleet stack` against a temp FLEET_STACKS_DIR and a canned fleet (FLEET_FIXTURE): ensure /
// new / show / set (human-edit rules) / add / remove / sync / rm, `list --json` carrying
// `stack`, and `stack spawn` under dry-run. `--no-llm` everywhere (a fixture never calls the
// model anyway); no ssh, no tmux is ever touched.
#![allow(deprecated)]

mod common;

use common::{Env, stderr, stdout};
use serde_json::{Value, json};

const A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const B: &str = "bbbbbbbb-1111-2222-3333-444444444444";

struct T {
    env: Env,
}

impl T {
    fn new() -> Self {
        let env = Env::new();
        env.write_config(&common::two_hosts());
        std::fs::create_dir_all(env.path("project")).unwrap();
        let t = T { env };
        t.fleet(&[A, B]);
        t
    }
    fn cwd(&self) -> String {
        self.env.path("project").display().to_string()
    }
    /// The canned fleet: the given sessions (of A and B) are live.
    fn fleet(&self, live: &[&str]) {
        let cwd = self.cwd();
        let all = [
            json!({
                "pid": 42, "session_id": A, "name": "login-redirect", "cwd": cwd, "status": "idle",
                "backend": "tmux", "handle": "%99", "tmux_session": "fleet-test-stack-a",
                "name_source": "user", "title": "Fix the login redirect loop"
            }),
            json!({
                "pid": 43, "session_id": B, "name": "login-tests", "cwd": cwd, "status": "busy",
                "backend": "tmux", "handle": "%98", "tmux_session": "fleet-test-stack-b",
                "name_source": "user", "title": "Write the login tests"
            }),
        ];
        let rows: Vec<&Value> = all
            .iter()
            .filter(|r| live.contains(&r["session_id"].as_str().unwrap()))
            .collect();
        common::fixture(self.env.dir.path(), &json!(rows).to_string());
    }
    fn cmd(&self) -> assert_cmd::Command {
        let mut c = self.env.cmd();
        c.env("FLEET_STACKS_DIR", self.env.path("stacks"))
            .env("FLEET_BRIEFS_DIR", self.env.path("briefs"))
            .env(
                "FLEET_FIXTURE",
                self.env.dir.path().join("fleet-fixture.json"),
            )
            .env("HOME", self.env.dir.path())
            .env_remove("XDG_STATE_HOME")
            .env("FLEET_CMD", "claude")
            .timeout(std::time::Duration::from_secs(20));
        c
    }
    fn json(&self, args: &[&str]) -> Value {
        let out = self.cmd().args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn stack_files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.env.path("stacks"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}

#[test]
fn ensure_creates_once_with_the_skeleton() {
    let t = T::new();
    let v = t.json(&["stack", "ensure", "login-redirect", "--no-llm", "--json"]);
    assert_eq!(v["created"], true);
    assert_eq!(v["generated"], false);
    assert_eq!(v["warning"], Value::Null);
    let id = v["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("st-") && id.len() == 11, "{id}");
    assert_eq!(v["host"], "laptop");
    assert_eq!(v["label"], "login-redirect");
    assert_eq!(
        v["cwd"],
        t.cwd()
            .replace(&t.env.dir.path().display().to_string(), "~")
    );
    assert_eq!(v["absCwd"], t.cwd().as_str());
    // `created` (bool) shadows the StackView timestamp: that is `createdAt` and `stack.created`.
    assert!(v["createdAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(v["stack"]["created"], v["createdAt"]);
    assert_eq!(v["stack"]["id"], id.as_str());
    let path = v["path"].as_str().unwrap();
    assert!(path.ends_with(&format!("stacks/{id}.md")), "{path}");
    assert_eq!(
        v["contextLine"],
        format!("You're running in the session stack with shared context: {path}.")
    );
    let m = &v["members"][0];
    assert_eq!(
        (&m["session"], &m["live"], &m["status"], &m["closed"]),
        (&json!(A), &json!(true), &json!("idle"), &Value::Null)
    );
    assert_eq!(m["briefExists"], false);
    assert_eq!(
        v["parsed"]["summary"],
        "Started from login-redirect in ~/project."
    );
    assert_eq!(v["parsed"]["resources"][0]["kind"], "Folder");
    assert!(
        v["markdown"]
            .as_str()
            .unwrap()
            .starts_with("---\nstack: st-")
    );
    assert!(v["body"].as_str().unwrap().starts_with("> Shared context"));
    // The file is private.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let dmode = std::fs::metadata(t.env.path("stacks"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dmode & 0o777, 0o700);

    // Again: the same stack, not a new one.
    let again = t.json(&["stack", "ensure", A, "--no-llm", "--json"]);
    assert_eq!(again["created"], false);
    assert_eq!(again["id"], id.as_str());
    assert_eq!(t.stack_files().len(), 1);

    // `new` refuses a session that already has one, and says which.
    let out = t
        .cmd()
        .args(["stack", "new", "--from", "login-redirect", "--no-llm"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains(&id), "{}", stderr(&out));
}

#[test]
fn new_with_a_label_and_list_json_carries_stack() {
    let t = T::new();
    let v = t.json(&[
        "stack",
        "new",
        "--from",
        A,
        "--label",
        "Login fix",
        "--no-llm",
        "--json",
    ]);
    assert_eq!(v["created"], true);
    assert_eq!(v["label"], "Login fix");
    let id = v["id"].as_str().unwrap().to_string();
    let rows = t.json(&["list", "--json"]);
    let by = |sid: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["session_id"] == sid)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by(A)["stack"],
        json!({ "id": id, "label": "Login fix", "host": "laptop" })
    );
    assert!(by(B).get("stack").unwrap().is_null(), "null, not missing");
    // Listing is read-only: nothing rewritten.
    let before = std::fs::read_to_string(t.env.path(&format!("stacks/{id}.md"))).unwrap();
    t.json(&["list", "--json"]);
    let after = std::fs::read_to_string(t.env.path(&format!("stacks/{id}.md"))).unwrap();
    assert_eq!(before, after);

    // `stack list`
    let l = t.json(&["stack", "list", "--json"]);
    assert_eq!(l["host"], "laptop");
    assert_eq!(l["stacks"][0]["id"], id.as_str());
    let out = t.cmd().args(["stack", "list"]).output().unwrap();
    assert!(stdout(&out).contains("Login fix"), "{}", stdout(&out));
}

#[test]
fn show_set_and_the_human_edit_rules() {
    let t = T::new();
    let v = t.json(&["stack", "ensure", A, "--no-llm", "--json"]);
    let id = v["id"].as_str().unwrap().to_string();
    let updated = v["updated"].as_str().unwrap().to_string();

    let out = t.cmd().args(["stack", "show", &id]).output().unwrap();
    let body = stdout(&out);
    assert!(body.starts_with("> Shared context for the session stack **login-redirect**"));
    assert!(!body.contains("members:"), "no frontmatter: {body}");
    let out = t
        .cmd()
        .args(["stack", "show", "login", "--path"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out).trim(), v["path"].as_str().unwrap());

    let edit = "---\nlabel: Login redirect fix\ncreated: 1999\n---\nA line above everything.\n\n## Summary\nThe login stream.\n\n## Resources\n- PR: [o/r#12](https://github.com/o/r/pull/12)\n\n## Sessions\n- hand-written junk\n\n## Notes\nkeep the flag\n\n## Decisions\n- no retries\n";
    // Stale --expect-updated: exit 3, nothing written.
    let out = t
        .cmd()
        .args([
            "stack",
            "set",
            &id,
            "--expect-updated",
            "2020-01-01T00:00:00.000Z",
        ])
        .write_stdin(edit)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("not saved"));
    // With --json the conflict also reports the stored `updated` on stdout (web PUT → 409).
    let out = t
        .cmd()
        .args([
            "stack",
            "set",
            &id,
            "--expect-updated",
            "2020-01-01T00:00:00.000Z",
            "--json",
        ])
        .write_stdin(edit)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["updated"].as_str(), Some(updated.as_str()));
    assert_eq!(report["id"].as_str(), Some(id.as_str()));
    assert!(report["error"].as_str().unwrap().contains("not saved"));

    let s = t.json_stdin(
        &["stack", "set", &id, "--expect-updated", &updated, "--json"],
        edit,
    );
    assert_eq!(s["label"], "Login redirect fix");
    assert_eq!(s["created"], v["createdAt"], "machine keys stay");
    assert!(s["editedAt"].is_string());
    assert_eq!(s["parsed"]["summary"], "The login stream.");
    assert_eq!(s["parsed"]["resources"][0]["kind"], "PR");
    assert_eq!(s["parsed"]["notes"], "keep the flag");
    assert_eq!(s["members"].as_array().unwrap().len(), 1);
    let md = s["markdown"].as_str().unwrap();
    assert!(
        !md.contains("hand-written junk"),
        "Sessions is regenerated: {md}"
    );
    assert!(md.contains("## Decisions\n- no retries\n"));
    assert!(md.contains("A line above everything."));
    assert!(md.contains("> Shared context for the session stack **Login redirect fix**"));
    assert!(
        md.contains("- **login-redirect** (`aaaaaaaa`, live)"),
        "{md}"
    );

    // A machine write (sync after a member left) keeps every human section.
    t.cmd().args(["stack", "add", &id, B]).assert().success();
    t.fleet(&[A]);
    let sync = t.json(&["stack", "sync", "--json"]);
    assert_eq!(sync["changed"], json!([id]));
    let md = sync["stacks"][0]["markdown"].as_str().unwrap();
    for keep in [
        "The login stream.",
        "- PR: [o/r#12]",
        "## Notes\nkeep the flag",
        "## Decisions",
        "A line above everything.",
    ] {
        assert!(md.contains(keep), "{keep}: {md}");
    }
    assert!(
        md.contains("- **login-tests** (`bbbbbbbb`, closed "),
        "{md}"
    );

    // Empty stdin saves nothing.
    let out = t
        .cmd()
        .args(["stack", "set", &id])
        .write_stdin("  \n")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

impl T {
    fn json_stdin(&self, args: &[&str], input: &str) -> Value {
        let out = self.cmd().args(args).write_stdin(input).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

#[test]
fn sync_marks_gone_members_closed_and_writes_only_on_change() {
    let t = T::new();
    let id = t.json(&["stack", "ensure", A, "--no-llm", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = t.json(&["stack", "add", &id, "login-tests", "--json"]);
    assert_eq!(v["members"].as_array().unwrap().len(), 2);
    assert_eq!(v["members"][1]["session"], B);

    let file = t.env.path(&format!("stacks/{id}.md"));
    let before = std::fs::read_to_string(&file).unwrap();
    let s = t.json(&["stack", "sync", "--json"]);
    assert_eq!(s["changed"], json!([]));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "no change, no write"
    );

    t.fleet(&[A]);
    let s = t.json(&["stack", "sync", "--json"]);
    assert_eq!(s["host"], "laptop");
    assert_eq!(s["changed"], json!([id]));
    let m = &s["stacks"][0]["members"][1];
    assert_eq!(m["live"], false);
    assert!(m["closed"].is_string());
    assert!(m["status"].is_null());
    let closed = m["closed"].clone();
    // Closed once: a second sync leaves it be.
    let s = t.json(&["stack", "sync", "--json"]);
    assert_eq!(s["changed"], json!([]));
    assert_eq!(s["stacks"][0]["members"][1]["closed"], closed);

    // A gone member is removed by its full session id; the last one leaves an empty stack.
    let v = t.json(&["stack", "remove", &id, B, "--json"]);
    assert_eq!(v["members"].as_array().unwrap().len(), 1);
    let v = t.json(&["stack", "remove", &id, "login-redirect", "--json"]);
    assert_eq!(v["members"], json!([]));
    assert!(file.exists(), "the file stays");
    assert!(v["body"].as_str().unwrap().contains("(no sessions yet)"));
}

#[test]
fn add_refuses_a_session_in_another_stack() {
    let t = T::new();
    let a = t.json(&["stack", "ensure", A, "--no-llm", "--label", "One", "--json"]);
    let b = t.json(&["stack", "ensure", B, "--no-llm", "--label", "Two", "--json"]);
    let out = t
        .cmd()
        .args(["stack", "add", "One", "login-tests"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("already in stack Two") && err.contains(b["id"].as_str().unwrap()),
        "{err}"
    );
    let _ = a;
}

#[test]
fn stack_targets_resolve_and_ambiguity_is_exit_2() {
    let t = T::new();
    t.json(&[
        "stack",
        "ensure",
        A,
        "--no-llm",
        "--label",
        "Login fix",
        "--json",
    ]);
    t.json(&[
        "stack",
        "ensure",
        B,
        "--no-llm",
        "--label",
        "Login tests",
        "--json",
    ]);
    let out = t.cmd().args(["stack", "show", "login"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("matches 2 stacks"),
        "{}",
        stderr(&out)
    );
    // A member session target resolves to its stack.
    let v = t.json(&["stack", "show", "bbbbbbbb", "--json"]);
    assert_eq!(v["label"], "Login tests");
    let out = t
        .cmd()
        .args(["stack", "show", "zzz-nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    // A session target that matches two sessions is ambiguous too (exit 2), never a guess.
    let out = t
        .cmd()
        .args(["-n", "stack", "spawn", "login", "hi", "--no-wait"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn rm_needs_force_with_json() {
    let t = T::new();
    let id = t.json(&["stack", "ensure", A, "--no-llm", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = t
        .cmd()
        .args(["stack", "rm", &id, "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(t.stack_files().len(), 1);
    let v = t.json(&["stack", "rm", &id, "-f", "--json"]);
    assert_eq!(v, json!({ "removed": id }));
    assert!(t.stack_files().is_empty());
}

#[test]
fn spawn_under_dry_run_prints_the_launch_and_writes_nothing() {
    let t = T::new();
    let out = t
        .cmd()
        .env("FLEET_DRY_RUN", "1")
        .args([
            "stack",
            "spawn",
            "login-redirect",
            "Write the e2e test",
            "--backend",
            "tmux",
            "--no-wait",
            "--no-llm",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("would create stack"), "{text}");
    assert!(
        text.contains(&format!("cd '{}' && fleet_prompt=\"$(cat '", t.cwd())),
        "{text}"
    );
    assert!(text.contains("-stack-"), "{text}");
    assert!(
        text.contains("You're running in the session stack with shared context: ")
            && text.trim_end().ends_with(".md. Write the e2e test"),
        "{text}"
    );
    assert!(t.stack_files().is_empty(), "dry-run writes no stack");
    assert!(
        !t.env.path(".claude/fleet-handoffs").exists(),
        "nor a prompt file"
    );

    // --json under dry-run: the shape the web server can read.
    let v = t.json(&[
        "-n",
        "stack",
        "spawn",
        A,
        "--backend",
        "tmux",
        "--no-wait",
        "--no-llm",
        "--json",
    ]);
    assert_eq!(v["dryRun"], true);
    assert_eq!(v["created"], true);
    assert_eq!(v["session"], Value::Null);
    assert_eq!(v["spawned"]["dir"], t.cwd().as_str());
    assert!(
        v["spawned"]["promptFile"]
            .as_str()
            .unwrap()
            .contains("/.claude/fleet-handoffs/")
    );
    assert_eq!(
        v["prompt"], v["contextLine"],
        "no prompt → the context line alone"
    );
    assert!(t.stack_files().is_empty());

    // An existing stack is reused (not re-created) and the sibling gets its path.
    let id = t.json(&["stack", "ensure", A, "--no-llm", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = t.json(&[
        "-n",
        "stack",
        "spawn",
        A,
        "go",
        "--no-wait",
        "--backend",
        "tmux",
        "--json",
    ]);
    assert_eq!(v["created"], false);
    assert_eq!(v["stack"]["id"], id.as_str());
    assert!(
        v["prompt"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{id}.md. go"))
    );
}

#[test]
fn edit_under_dry_run_and_remote_dispatch() {
    let t = T::new();
    let id = t.json(&["stack", "ensure", A, "--no-llm", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = t.cmd().args(["-n", "stack", "edit", &id]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("dry-run: would open"),
        "{}",
        stdout(&out)
    );
    // -H dispatches every stack command to that host over ssh.
    let out = t
        .cmd()
        .args(["-n", "-H", "workstation", "stack", "list", "--json"])
        .output()
        .unwrap();
    let text = stdout(&out) + &stderr(&out);
    assert!(
        text.contains("devbox") && text.contains("stack list --json"),
        "{text}"
    );
    let out = t
        .cmd()
        .args(["-n", "-H", "workstation", "stack", "edit", "x"])
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("stack show x --json"), "{text}");
}

/// A `/api/fleet`-shaped body: `workstation` answered with these rows (a `list --json` row
/// shape, as the web server merges them).
fn fleet_body(ws_rows: &[Value], ok: bool) -> String {
    json!({ "hosts": [
        { "name": "laptop", "ok": true, "sessions": [] },
        { "name": "workstation", "ok": ok, "sessions": ws_rows }
    ] })
    .to_string()
}

const C: &str = "cccccccc-1111-2222-3333-444444444444";

#[test]
fn members_on_another_host_join_through_input_and_are_judged_only_with_it() {
    let t = T::new();
    let id = t.json(&["stack", "ensure", A, "--no-llm", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ws = json!({
        "pid": 77, "session_id": C, "name": "login-e2e", "cwd": "/home/ws/Code/project",
        "status": "busy", "backend": "tmux", "handle": "%5", "tmux_session": "login-e2e",
        "title": "Write the e2e test"
    });
    // `--on` without `--input` would ssh to `devbox`; with `--input` the row comes from there.
    let out = t
        .cmd()
        .args([
            "stack",
            "add",
            &id,
            "login-e2e",
            "--on",
            "workstation",
            "--input",
            "-",
            "--json",
        ])
        .write_stdin(fleet_body(std::slice::from_ref(&ws), true))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let m = &v["members"][1];
    assert_eq!(m["session"], C);
    assert_eq!(m["host"], "workstation");
    assert_eq!(m["live"], true);
    assert_eq!(m["status"], "busy");
    assert_eq!(m["cwd"], "/home/ws/Code/project");
    assert!(m["briefPath"].is_null(), "the brief is on the workstation");
    assert_eq!(v["members"][0]["host"], "laptop");
    assert!(
        v["body"]
            .as_str()
            .unwrap()
            .contains("(`cccccccc`, live, on workstation)"),
        "{}",
        v["body"]
    );
    assert_eq!(
        v["remoteContextLine"],
        format!(
            "You're running in the session stack with shared context: stack {id} on host laptop. Read it with `fleet -H laptop stack show {id}` and update it with `fleet -H laptop stack set {id}` (the whole markdown on stdin; `fleet -H laptop stack show {id} --json` has it under `markdown`)."
        )
    );
    // A host the input has no rows for is refused; this machine's name means a local target.
    let out = t
        .cmd()
        .args([
            "stack",
            "add",
            &id,
            "login-e2e",
            "--on",
            "office",
            "--input",
            "-",
            "--json",
        ])
        .write_stdin(fleet_body(std::slice::from_ref(&ws), true))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no sessions for host 'office'"),
        "{}",
        stderr(&out)
    );
    let v = t.json(&[
        "stack",
        "add",
        &id,
        "login-tests",
        "--on",
        "laptop",
        "--json",
    ]);
    assert_eq!(v["members"][2]["host"], "laptop");

    // A plain sync (this host only) never touches the workstation member…
    let s = t.json(&["stack", "sync", "--json"]);
    assert_eq!(s["changed"], json!([]));
    assert_eq!(
        s["stacks"][0]["members"][1]["live"], true,
        "unknown here: from `closed`"
    );
    assert!(s["stacks"][0]["members"][1]["status"].is_null());
    // …nor does one whose input says the workstation did not answer…
    let out = t
        .cmd()
        .args(["stack", "sync", "--input", "-", "--json"])
        .write_stdin(fleet_body(&[], false))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(s["changed"], json!([]));
    // …but one that answered without it marks it closed, and brings it back when it returns.
    let out = t
        .cmd()
        .args(["stack", "sync", "--input", "-", "--json"])
        .write_stdin(fleet_body(&[], true))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(s["changed"], json!([id]));
    assert_eq!(s["stacks"][0]["members"][1]["live"], false);
    assert!(s["stacks"][0]["members"][1]["closed"].is_string());
    let out = t
        .cmd()
        .args(["stack", "sync", "--input", "-", "--json"])
        .write_stdin(fleet_body(&[ws], true))
        .output()
        .unwrap();
    let s: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(s["changed"], json!([id]));
    assert!(s["stacks"][0]["members"][1]["closed"].is_null());
    // The local member is still judged by discovery, not by the input's (empty) laptop entry.
    assert_eq!(s["stacks"][0]["members"][0]["live"], true);
    assert_eq!(s["stacks"][0]["members"][0]["status"], "idle");
}

#[test]
fn spawn_on_another_host_under_dry_run_shows_the_ssh_line_and_the_remote_context() {
    let t = T::new();
    let out = t
        .cmd()
        .env("FLEET_DRY_RUN", "1")
        .args([
            "stack",
            "spawn",
            "login-redirect",
            "Write the e2e test",
            "--on",
            "workstation",
            "--name",
            "login-e2e",
            "--no-llm",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("would create stack"), "{text}");
    assert!(
        text.contains("would spawn a sibling on workstation"),
        "{text}"
    );
    assert!(text.contains("ssh ") && text.contains(" devbox "), "{text}");
    // The same place over there, home-relative; the prompt is the remote context line.
    assert!(
        text.contains("--dir ") && text.contains("~/project"),
        "{text}"
    );
    assert!(text.contains("--name login-e2e"), "{text}");
    assert!(
        text.contains("shared context: stack st-")
            && text.contains("on host laptop. Read it with `fleet -H laptop stack show st-"),
        "{text}"
    );
    assert!(
        text.trim_end()
            .ends_with("under `markdown`). Write the e2e test"),
        "{text}"
    );
    assert_eq!(t.stack_files(), Vec::<String>::new(), "nothing written");
    // Naming this machine means a plain local spawn.
    let out = t
        .cmd()
        .env("FLEET_DRY_RUN", "1")
        .args([
            "stack",
            "spawn",
            "login-redirect",
            "--on",
            "laptop",
            "--backend",
            "tmux",
            "--no-llm",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("would spawn a tmux sibling"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_host_with_a_web_url_and_no_ssh_is_reached_over_its_web_server() {
    let t = T::new();
    // The sheds know each other by web url only: `-H` goes to that server's stack routes.
    let mut cfg = common::two_hosts();
    cfg["hosts"]["workstation"] = json!({ "ssh": null, "web": "http://100.x.y.z:7777" });
    t.env.write_config(&cfg);
    let run = |args: &[&str]| {
        let out = t
            .cmd()
            .env("FLEET_DRY_RUN", "1")
            .args(args)
            .output()
            .unwrap();
        (out.status.success(), stdout(&out), stderr(&out))
    };
    let (ok, text, err) = run(&["-H", "workstation", "stack", "show", "st-1a2b3c4d"]);
    assert!(ok, "{err}");
    assert!(
        text.contains("curl ")
            && text.contains("http://100.x.y.z:7777/api/hosts/workstation/stacks/st-1a2b3c4d"),
        "{text}"
    );
    assert!(text.contains("-X GET"), "{text}");
    let (ok, text, _) = run(&["-H", "workstation", "stack", "show", "1a2b3c4d", "--json"]);
    assert!(ok && text.contains("/stacks/st-1a2b3c4d"), "{text}");
    let out = t
        .cmd()
        .env("FLEET_DRY_RUN", "1")
        .args([
            "-H",
            "workstation",
            "stack",
            "set",
            "st-1a2b3c4d",
            "--expect-updated",
            "2026-01-01T00:00:00.000Z",
        ])
        .write_stdin("## Summary\nnew\n")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("-X PUT") && stdout(&out).contains("--data-binary @-"),
        "{}",
        stdout(&out)
    );
    let (ok, text, _) = run(&["-H", "workstation", "stack", "list"]);
    assert!(
        ok && text.contains("/api/hosts/workstation/stacks"),
        "{text}"
    );
    let (ok, text, _) = run(&["-H", "workstation", "stack", "rm", "st-1a2b3c4d", "-f"]);
    assert!(ok && text.contains("-X DELETE"), "{text}");
    // Only an id addresses a stack over the web; other forms still need ssh.
    let (ok, _, err) = run(&["-H", "workstation", "stack", "show", "Login fix"]);
    assert!(!ok && err.contains("addressed by its id"), "{err}");
    let (ok, _, err) = run(&["-H", "workstation", "stack", "spawn", "login-redirect"]);
    assert!(
        !ok && err.contains("no ssh destination") && err.contains("stack list / show / set"),
        "{err}"
    );
    // Naming this machine is the local form, as before.
    let (ok, text, _) = run(&["-H", "laptop", "stack", "list"]);
    assert!(ok && !text.contains("curl"), "{text}");
}

#[test]
fn rename_changes_the_label_everywhere_and_refuses_an_empty_one() {
    let t = T::new();
    let v = t.json(&["stack", "ensure", A, "--no-llm", "--json"]);
    let id = v["id"].as_str().unwrap().to_string();
    let r = t.json(&["stack", "rename", &id, "  **Auth rework**  ", "--json"]);
    assert_eq!(r["label"], "Auth rework", "cleaned like --label");
    assert!(r["editedAt"].is_string(), "a human change");
    let file = std::fs::read_to_string(t.env.path(&format!("stacks/{id}.md"))).unwrap();
    assert!(file.contains("\nlabel: Auth rework\n"), "{file}");
    assert!(
        file.contains("**Auth rework** (`"),
        "the header follows: {file}"
    );
    // Rows and the label rung of `<stack>` see the new name.
    let rows = t.json(&["list", "--json"]);
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == A)
        .unwrap();
    assert_eq!(row["stack"]["label"], "Auth rework");
    assert_eq!(
        t.json(&["stack", "show", "auth", "--json"])["id"],
        id.as_str()
    );
    // Empty / whitespace: exit 1, nothing written.
    let out = t
        .cmd()
        .args(["stack", "rename", &id, "   "])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("one non-empty line"),
        "{}",
        stderr(&out)
    );
    let out = t
        .cmd()
        .args(["stack", "rename", "nope", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}
