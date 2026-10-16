//! `pid:` / `id:` against live fake agents on the test's private tmux server: the exact lookup
//! finds one session without discovering the fleet, and refuses an entry whose pid now belongs
//! to a newer process.

mod common;

use common::fake::FakeFleet;
use common::{stderr, stdout};

const ONE: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const TWO: &str = "bbbbbbbb-1111-2222-3333-444444444444";
const OLD: &str = "cccccccc-1111-2222-3333-444444444444";

fn idle(f: &FakeFleet, name: &str, sid: &str) -> i64 {
    f.agent(name, sid, 80, 24, "printf 'ready\\n'; exec sleep 300")
}

#[test]
fn exact_targets_find_the_live_session_they_name() {
    let Some(f) = FakeFleet::new() else { return };
    let one = idle(&f, "one", ONE);
    let _two = idle(&f, "two", TWO);
    for (target, name) in [
        (format!("pid:{one}"), "one"),
        (format!("id:{TWO}"), "two"),
        (format!("ID:{}", TWO.to_uppercase()), "two"),
    ] {
        let out = f.fleet(&["peek", &target]);
        assert!(out.status.success(), "{target}: {}", stderr(&out));
        assert!(
            stdout(&out).contains(&format!("▼ {name} (tmux, idle)")),
            "{target}: {}",
            stdout(&out)
        );
    }
    // An entry with no `procStart` (older Claude) is taken on liveness alone.
    f.register(one, "one", ONE, None);
    let out = f.fleet(&["peek", &format!("pid:{one}")]);
    assert!(out.status.success(), "{}", stderr(&out));
    // A headless run (`claude -p`) is never a target, exact or not.
    let mut reg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.env.path(&format!("home/.claude/sessions/{one}.json"))).unwrap(),
    )
    .unwrap();
    reg["entrypoint"] = "sdk-cli".into();
    std::fs::write(
        f.env.path(&format!("home/.claude/sessions/{one}.json")),
        reg.to_string(),
    )
    .unwrap();
    let out = f.fleet(&["peek", &format!("pid:{one}")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no live session has pid"),
        "{}",
        stderr(&out)
    );
}

// The registry still names the pid, the pid is alive — but its process started later than
// the one Claude registered: the pid was reused, and `send pid:…` must not type into it.
#[test]
fn an_exact_target_refuses_a_reused_pid() {
    let Some(f) = FakeFleet::new() else { return };
    let pid = idle(&f, "old", OLD);
    f.register(pid, "old", OLD, Some("Mon Jan  1 00:00:00 2001"));
    for target in [format!("pid:{pid}"), format!("id:{OLD}")] {
        let out = f.fleet(&["send", &target, "hello"]);
        assert_eq!(out.status.code(), Some(1), "{target}");
        assert!(
            stderr(&out).contains("no live session has"),
            "{target}: {}",
            stderr(&out)
        );
    }
    // The limit, documented: a bare pid walks the full discovery, which checks liveness only.
    let out = f.fleet(&["peek", &pid.to_string()]);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A Claude transcript for `sid` in the fake fleet's cwd, its first prompt `prompt`.
fn transcript(f: &FakeFleet, sid: &str, prompt: &str) {
    let cwd = f.env.dir.path().display().to_string();
    let enc: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let dir = f.env.path(&format!("home/.claude/projects/{enc}"));
    std::fs::create_dir_all(&dir).unwrap();
    let line =
        serde_json::json!({ "type": "user", "message": { "role": "user", "content": prompt } });
    std::fs::write(dir.join(format!("{sid}.jsonl")), format!("{line}\n")).unwrap();
}

// The exact lookup skips the fleet's transcripts, not the chosen session's title: `name`
// works from it, so `name id:X` must suggest what `name X` does.
#[test]
fn an_exact_target_still_carries_its_title() {
    let Some(f) = FakeFleet::new() else { return };
    f.env
        .write_config(&serde_json::json!({ "naming": { "enabled": false } }));
    let pid = idle(&f, "one", ONE);
    // Claude's derived name, and a second pane so the tmux session name is not adopted.
    f.register(
        pid,
        "project-a1b2",
        ONE,
        Some(&common::fake::proc_start(pid)),
    );
    let mut reg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap(),
    )
    .unwrap();
    reg["nameSource"] = "derived".into();
    std::fs::write(
        f.env.path(&format!("home/.claude/sessions/{pid}.json")),
        reg.to_string(),
    )
    .unwrap();
    f.tmux(&["split-window", "-d", "-t", "fleet-test-one", "sleep 300"]);
    transcript(&f, ONE, "Fix the flaky login redirect test");
    let suggest = |target: &str| {
        let out = f.fleet(&["name", target]);
        assert!(out.status.success(), "{target}: {}", stderr(&out));
        stdout(&out)
    };
    let bare = suggest(ONE);
    assert!(bare.contains("fix-the-flaky-login-redirect"), "{bare}");
    assert_eq!(suggest(&format!("id:{ONE}")), bare);
    assert_eq!(suggest(&format!("pid:{pid}")), bare);
}

/// `ps` on PATH that logs each call to `<env>/ps.log`; returns the PATH to run fleet with.
fn counting_ps(f: &FakeFleet) -> String {
    let dir = f.env.path("shim");
    std::fs::create_dir_all(&dir).unwrap();
    let log = f.env.path("ps.log");
    let ps = dir.join("ps");
    std::fs::write(
        &ps,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec /bin/ps \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ps, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn ps_calls(f: &FakeFleet) -> Vec<String> {
    let log = f.env.path("ps.log");
    let calls = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_file(log);
    calls
}

// What a poll by exact target costs, pinned: one `ps` for a Claude session however many
// others run, and for a Codex one the harness listing plus the one process it names.
#[test]
fn an_exact_poll_inspects_only_the_session_it_names() {
    let Some(f) = FakeFleet::new() else { return };
    let one = idle(&f, "one", ONE);
    idle(&f, "two", TWO);
    idle(&f, "three", OLD);
    // Two Codex sessions, each with its rollout.
    let day = f.env.path("home/.codex/sessions/2026/10/16");
    std::fs::create_dir_all(&day).unwrap();
    let mut codex = Vec::new();
    for (i, cid) in [
        "c0dec0de-0000-0000-0000-000000000001",
        "c0dec0de-0000-0000-0000-000000000002",
    ]
    .iter()
    .enumerate()
    {
        let cwd = f.env.path(&format!("codex{i}"));
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = std::fs::canonicalize(&cwd).unwrap();
        let pid = f.agent(
            &format!("codex{i}"),
            "unused",
            80,
            24,
            &format!(
                "cd '{}' && exec /bin/bash -c 'exec -a codex sleep 300'",
                cwd.display()
            ),
        );
        // Not a Claude session: no registry entry.
        std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
        let now = chrono_now();
        let meta = serde_json::json!({ "type": "session_meta",
            "payload": { "id": cid, "cwd": cwd, "timestamp": now } });
        std::fs::write(
            day.join(format!("rollout-2026-10-16T10-00-0{i}-{cid}.jsonl")),
            format!("{meta}\n"),
        )
        .unwrap();
        codex.push((pid, cid.to_string()));
    }
    let path = counting_ps(&f);
    // The header names the session and its cwd: `▼ <title> (tmux, <status>) — <dir>`.
    let peek = |target: &str| {
        let out = f
            .env
            .cmd()
            .env("PATH", &path)
            .args(["peek", target])
            .output()
            .unwrap();
        assert!(out.status.success(), "{target}: {}", stderr(&out));
        stdout(&out).lines().nth(1).unwrap_or_default().to_string()
    };
    ps_calls(&f);
    let header = peek(&format!("pid:{one}"));
    assert!(header.contains("▼ one (tmux"), "{header}");
    let header = peek(&format!("id:{ONE}"));
    assert!(header.contains("▼ one (tmux"), "{header}");
    let calls = ps_calls(&f);
    assert_eq!(
        calls.len(),
        2,
        "one batched ps per Claude lookup: {calls:?}"
    );

    let (_, cid) = &codex[1];
    let pid = codex[1].0;
    for target in [format!("id:{cid}"), format!("pid:{pid}")] {
        let header = peek(&target);
        assert!(header.trim_end().ends_with("codex1"), "{target}: {header}");
        let calls = ps_calls(&f);
        // The harness listing (`ps -axww`) and the named process's environment — not one
        // `ps` per Claude session and per Codex session as a full discovery runs.
        assert_eq!(calls.len(), 2, "{target}: {calls:?}");
    }
}

fn chrono_now() -> String {
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
