// `fleet brief` against a temp FLEET_BRIEFS_DIR and a canned fleet (FLEET_FIXTURE): print,
// --json, --prompt, --set (human-edit rules), gone sessions, and `spawn --from` under dry-run.
// No model, no ssh, no tmux is ever touched.
#![allow(deprecated)]

mod common;

use common::{Env, stderr, stdout};

const SID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const GONE: &str = "bbbbbbbb-1111-2222-3333-444444444444";
const SAMPLE: &str = include_str!("../../../testdata/briefs/sample.md");
const CONTINUE: &str = include_str!("../../../testdata/briefs/sample.continue.txt");

struct T {
    env: Env,
}

impl T {
    fn new() -> Self {
        let env = Env::new();
        env.write_config(&common::two_hosts());
        std::fs::create_dir_all(env.path("briefs")).unwrap();
        std::fs::create_dir_all(env.path("project")).unwrap();
        let cwd = env.path("project").display().to_string();
        let fleet = serde_json::json!([{
            "pid": 42, "session_id": SID, "name": "app-9d", "cwd": cwd, "status": "idle",
            "backend": "tmux", "handle": "%99", "tmux_session": "fleet-test-brief",
            "name_source": "user", "title": "fix the login loop"
        }]);
        common::fixture(env.dir.path(), &fleet.to_string());
        T { env }
    }
    fn brief_path(&self, id: &str) -> std::path::PathBuf {
        self.env.path("briefs").join(format!("{id}.md"))
    }
    fn write_brief(&self, id: &str, text: &str) {
        std::fs::write(self.brief_path(id), text).unwrap();
    }
    fn read_brief(&self, id: &str) -> String {
        std::fs::read_to_string(self.brief_path(id)).unwrap()
    }
    fn cmd(&self) -> assert_cmd::Command {
        let mut c = self.env.cmd();
        c.env("FLEET_BRIEFS_DIR", self.env.path("briefs"))
            .env(
                "FLEET_FIXTURE",
                self.env.dir.path().join("fleet-fixture.json"),
            )
            .env("HOME", self.env.dir.path())
            .env("FLEET_CMD", "claude")
            .timeout(std::time::Duration::from_secs(20));
        c
    }
    fn cwd(&self) -> String {
        self.env.path("project").display().to_string()
    }
}

#[test]
fn prints_the_body_without_frontmatter() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let out = t.cmd().args(["brief", "app-9d"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("A note before the first heading.\n\n## Summary\n"),
        "{text}"
    );
    assert!(!text.contains("generatedThrough"), "{text}");
    assert!(text.contains("- [ ] open the PR"));
}

#[test]
fn json_has_the_web_api_shape() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let out = t.cmd().args(["brief", SID, "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["id"], SID);
    assert_eq!(v["host"], "laptop");
    assert_eq!(v["exists"], true);
    assert_eq!(v["updated"], "2026-01-02T03:04:05.000Z");
    assert_eq!(v["editedAt"], serde_json::Value::Null);
    assert_eq!(v["generatedThrough"], 48213);
    assert!(
        v["markdown"]
            .as_str()
            .unwrap()
            .starts_with("---\nsession: ")
    );
    assert_eq!(v["parsed"]["resources"][0]["kind"], "PR");
    assert_eq!(v["parsed"]["resources"][0]["label"], "owner/repo#12");
    assert_eq!(v["parsed"]["todos"][2]["done"], false);
    assert_eq!(
        v["parsed"]["plan"], v["parsed"]["todos"],
        "deprecated alias, one release"
    );
    let git = &v["parsed"]["resources"][3];
    assert_eq!(
        (&git["kind"], &git["branch"], &git["linked"]),
        (&"Git".into(), &"fix-login".into(), &true.into())
    );
    assert_eq!(v["absCwd"], t.cwd().as_str(), "the live cwd, absolute");
    assert!(v["gitRoot"].is_null(), "a temp dir is not a checkout");
    // A live session's cwd wins over the brief's (as in the server's view).
    let prompt = v["continuePrompt"].as_str().unwrap();
    assert!(
        prompt.starts_with(&format!(
            "Continue the work of session {SID} on laptop in {}.",
            t.cwd()
        )),
        "{prompt}"
    );
    for k in ["parsed", "markdown", "body", "path", "generatedAt"] {
        assert!(v.get(k).is_some(), "missing {k}");
    }
}

#[test]
fn prompt_only_is_the_continue_prompt() {
    let t = T::new();
    // A gone session: its brief is still served by full id, with the brief's own cwd.
    t.write_brief(GONE, &SAMPLE.replace(SID, GONE));
    let out = t.cmd().args(["brief", GONE, "--prompt"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("{}\n", CONTINUE.replace(SID, GONE)));
    // A prefix of a gone session is not enough: nothing live matches it.
    let out = t.cmd().args(["brief", "bbbbbbbb"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn missing_brief_prints_the_skeleton_and_says_so() {
    let t = T::new();
    let out = t.cmd().args(["brief", "app-9d"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(stdout(&out), "## Summary\n\n## Resources\n\n## Todos\n");
    assert!(stderr(&out).contains("no brief yet"));
    let v: serde_json::Value = serde_json::from_slice(
        &t.cmd()
            .args(["brief", "app-9d", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(v["exists"], false);
    assert!(!t.brief_path(SID).exists(), "reading never writes");
}

#[test]
fn set_is_a_human_edit() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let body = "## Summary\nDone, in review.\n\n## Resources\n- File: `src/login.ts`\n\n## Plan\n- [x] fix\n";
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--set"])
        .write_stdin(body)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("dismissed"), "{}", stdout(&out));
    let saved = t.read_brief(SID);
    // Machine keys kept, unknown keys kept, editedAt stamped, cwd from the live session.
    assert!(saved.contains("generatedThrough: 48213\n"), "{saved}");
    assert!(saved.contains("todos: 3f2a9c01b7de\n"));
    assert!(saved.contains("zeta: kept as is\n"));
    assert!(saved.contains("alpha: {\"nested\":[1,2],\"b\":\"x\"}\n"));
    assert!(saved.contains("editedAt: "));
    assert!(saved.contains(&format!("cwd: {}\n", t.cwd())));
    assert!(!saved.contains("updated: 2026-01-02T03:04:05.000Z"));
    // Every removed resource line is dismissed; the old dismissed entry stays first.
    assert!(saved.contains(
        "dismissed: [\"https://github.com/owner/repo/pull/9\",\"https://github.com/owner/repo/pull/12\",\"branch:fix-login\""
    ), "{saved}");
    assert!(saved.ends_with("## Summary\nDone, in review.\n\n## Resources\n- File: `src/login.ts`\n\n## Todos\n- [x] fix\n"));
    // No temp files next to it.
    assert_eq!(std::fs::read_dir(t.env.path("briefs")).unwrap().count(), 1);
}

#[test]
fn set_refuses_a_stale_edit_and_empty_input() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let out = t
        .cmd()
        .args([
            "brief",
            "app-9d",
            "--set",
            "--expect-updated",
            "2020-01-01T00:00:00.000Z",
        ])
        .write_stdin("## Summary\nx\n")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("changed since it was opened"));
    assert_eq!(t.read_brief(SID), SAMPLE, "nothing written");

    let ok = t
        .cmd()
        .args([
            "brief",
            "app-9d",
            "--set",
            "--json",
            "--expect-updated",
            "2026-01-02T03:04:05.000Z",
        ])
        .write_stdin("## Summary\nx\n")
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", stderr(&ok));
    let v: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(v["parsed"]["summary"], "x");
    assert!(v["editedAt"].is_string());

    let empty = t
        .cmd()
        .args(["brief", "app-9d", "--set"])
        .write_stdin("  \n")
        .output()
        .unwrap();
    assert!(!empty.status.success());
}

#[test]
fn set_under_dry_run_writes_nothing() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let out = t
        .cmd()
        .args(["-n", "brief", "app-9d", "--set"])
        .write_stdin("## Summary\nchanged\n")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("## Summary\nchanged\n"));
    assert_eq!(t.read_brief(SID), SAMPLE);
}

#[test]
fn edit_uses_the_editor_on_the_body() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    // A scripted "editor": rewrites the summary line in place.
    let editor = t.env.path("ed.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\ngrep -q '^---' \"$1\" && exit 9\nsed -i.bak 's/^Fixing the login redirect loop.*/Edited by hand./' \"$1\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--edit"])
        .env("VISUAL", &editor)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let saved = t.read_brief(SID);
    assert!(saved.contains("## Summary\nEdited by hand.\n"), "{saved}");
    assert!(saved.contains("editedAt: "));
    assert!(saved.contains("generatedThrough: 48213\n"));

    // An editor that changes nothing saves nothing.
    let before = t.read_brief(SID);
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--edit"])
        .env("VISUAL", "true")
        .output()
        .unwrap();
    assert!(stdout(&out).contains("no changes"));
    assert_eq!(t.read_brief(SID), before);
}

#[test]
fn edit_on_another_host_goes_over_ssh() {
    let t = T::new();
    let out = t
        .cmd()
        .args(["-n", "-H", "workstation", "brief", "app-9d", "--edit"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ssh "), "{text}");
    assert!(text.contains("devbox"), "{text}");
    assert!(text.contains("brief app-9d --json"), "{text}");
}

#[test]
fn regenerate_asks_the_web_server() {
    let t = T::new();
    let out = t
        .cmd()
        .args(["-n", "brief", "app-9d", "--regenerate"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("curl "), "{text}");
    assert!(
        text.contains(&format!(
            "http://127.0.0.1:7777/api/hosts/laptop/sessions/{SID}/brief/regenerate"
        )),
        "{text}"
    );
    // Nothing listening: a clear error, not a hang.
    let mut cfg = common::two_hosts();
    cfg["hosts"]["laptop"]["web"] = serde_json::json!("http://127.0.0.1:9");
    t.env.write_config(&cfg);
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--regenerate"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("cannot reach the web server at http://127.0.0.1:9"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn spawn_from_honours_dry_run_and_uses_the_sessions_cwd() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    let out = t
        .cmd()
        .args([
            "-n",
            "spawn",
            "--from",
            "app-9d",
            "--backend",
            "tmux",
            "and add a test",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("cd '{}' && fleet_prompt=", t.cwd())),
        "{text}"
    );
    assert!(
        text.contains(r#"&& claude -- "${fleet_prompt%.}""#),
        "{text}"
    );
    assert!(text.contains("Continue the work of session"), "{text}");
    assert!(
        text.ends_with("tell me briefly what you found first.\n\nand add a test\n"),
        "{text}"
    );
    assert!(
        !t.env.path(".claude/fleet-handoffs").exists(),
        "dry-run writes no prompt file"
    );

    // No brief → nothing to continue from.
    let t2 = T::new();
    let out = t2
        .cmd()
        .args(["-n", "spawn", "--from", "app-9d"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no brief yet"));
}

#[test]
fn plain_spawn_honours_dry_run() {
    let t = T::new();
    let out = t
        .cmd()
        .args([
            "-n",
            "spawn",
            "--dir",
            &t.cwd(),
            "--backend",
            "tmux",
            "hello",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("dry-run: would spawn a tmux session"),
        "{}",
        stdout(&out)
    );
}

/// A one-shot HTTP server on 127.0.0.1 answering every request with `status` + `body`;
/// returns its base URL and the request it saw.
fn fake_web(status: &'static str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap_or(0);
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(reply.as_bytes()).unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    (url, h)
}

#[test]
fn regenerate_reports_started_and_the_hourly_cap() {
    let t = T::new();
    let (url, seen) = fake_web(
        "202 Accepted",
        r#"{"host":"laptop","id":"x","started":true,"queued":false,"generating":true}"#,
    );
    let mut cfg = common::two_hosts();
    cfg["hosts"]["laptop"]["web"] = serde_json::json!(url);
    t.env.write_config(&cfg);
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--regenerate"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("regenerating the brief"),
        "{}",
        stdout(&out)
    );
    let req = seen.join().unwrap();
    assert!(
        req.starts_with(&format!(
            "POST /api/hosts/laptop/sessions/{SID}/brief/regenerate HTTP/1.1"
        )),
        "{req}"
    );

    let (url, _seen) = fake_web(
        "429 Too Many Requests",
        r#"{"error":"brief model calls are capped at 12/hour","retryAfterMs":600000}"#,
    );
    cfg["hosts"]["laptop"]["web"] = serde_json::json!(url);
    t.env.write_config(&cfg);
    let out = t
        .cmd()
        .args(["brief", "app-9d", "--regenerate"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("capped at 12/hour (retry in 10 min)"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn open_runs_the_editor_on_the_cwd_or_over_remote_ssh() {
    let t = T::new();
    t.write_brief(SID, SAMPLE);
    // Here: `code <dir>` (not a git checkout → the cwd).
    let out = t
        .cmd()
        .args(["-n", "brief", "app-9d", "--open"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), format!("code {}", t.cwd()));
    // Another host: its `fleet brief --json` over ssh, then Remote-SSH with the host's ssh dest.
    let out = t
        .cmd()
        .args(["-n", "-H", "workstation", "brief", "app-9d", "--open"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("devbox"), "{text}");
    assert!(text.contains("brief app-9d --json"), "{text}");
    assert!(text.contains("code --remote ssh-remote+devbox "), "{text}");
}

// `--json` answers a target that does not resolve with one object, kind and all.
#[test]
fn json_reports_a_target_that_does_not_resolve() {
    let t = T::new();
    for (target, kind) in [
        (
            "id:cccccccc-1111-2222-3333-444444444444",
            "target_not_found",
        ),
        ("pid:4242", "target_not_found"),
        ("pid:abc", "target_invalid"),
    ] {
        let out = t.cmd().args(["brief", target, "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{target}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{target}: {e}: {}", stdout(&out)));
        assert_eq!(v["ok"], false, "{target}");
        assert_eq!(v["kind"], kind, "{target}: {v}");
    }
}

// The archive fallback (a gone session's brief by its id) is for "nothing live matched"
// only: two live sessions sharing the id is a question, even with a brief on disk.
#[test]
fn an_ambiguous_id_never_falls_back_to_the_archive() {
    let t = T::new();
    let cwd = t.cwd();
    let row = |pid: i64| {
        serde_json::json!({ "pid": pid, "session_id": SID, "name": format!("twin-{pid}"),
            "cwd": cwd, "status": "idle", "backend": "tmux", "handle": "%99" })
    };
    common::fixture(
        t.env.dir.path(),
        &serde_json::json!([row(42), row(43)]).to_string(),
    );
    t.write_brief(SID, SAMPLE);
    let out = t
        .cmd()
        .args(["brief", &format!("id:{SID}"), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "target_ambiguous", "{v}");
    // `spawn --from` resolves the same way.
    let out = t
        .cmd()
        .args(["-n", "spawn", "--from", &format!("id:{SID}")])
        .output()
        .unwrap();
    assert!(
        stderr(&out).contains("matches 2 sessions"),
        "{}",
        stderr(&out)
    );
}

// `id:` matches any case; a gone session's brief is stored under its (lowercase) id, and is
// found — and reported — as that, whatever case was typed.
#[test]
fn an_uppercase_id_finds_the_archived_brief_by_its_canonical_id() {
    let t = T::new();
    t.write_brief(GONE, &SAMPLE.replace(SID, GONE));
    let out = t
        .cmd()
        .args(["brief", &format!("id:{}", GONE.to_uppercase()), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["id"], GONE, "{v}");
}
