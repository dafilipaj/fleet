// `cargo_bin` is marked deprecated in recent assert_cmd but remains the
// canonical entry point for integration tests on a standard cargo layout;
// the replacement lives in a separate crate we don't want to pull in.
#![allow(deprecated)]

mod common;

/// `fleet` with an absent config — never the user's real one.
fn fleet() -> assert_cmd::Command {
    common::Env::new().cmd()
}

// `list --json` must always exit 0 and emit a JSON array, even with no sessions.
#[test]
fn list_json_is_valid_array() {
    let out = fleet().args(["list", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert!(v.is_array());
}

// Bare `fleet` opens the TUI on a terminal, but a piped stdout (this test, a
// script, an agent) must get the one-shot list and *exit* — not the watch loop.
// Enforced with a timeout so a regression fails the suite instead of hanging it.
#[test]
fn bare_invocation_is_one_shot_when_piped() {
    // A real one-shot `list`: discovery, tmux, the registry — all of it the test's own.
    let env = common::Env::new();
    let mut child = env
        .std_cmd()
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success());
                break;
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("bare `fleet` did not exit — it fell into the watch loop while piped");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("FLEET"));
}

// The new `watch` flags have to be discoverable — `--rows`/`--mouse`/`--no-mouse`
// are how a phone session pins the layout it wants.
#[test]
fn watch_help_documents_the_layout_flags() {
    let out = fleet().args(["watch", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--rows", "--mouse", "--no-mouse", "--interval", "--quiet"] {
        assert!(
            text.contains(flag),
            "`watch --help` is missing {flag}:\n{text}"
        );
    }
    // The value hints matter as much as the flag.
    assert!(
        text.contains("auto"),
        "--rows should offer 1/2/auto:\n{text}"
    );
}

// The `name` verb is the scriptable half of the `N`/`Ctrl-N` keys — a skill or a
// shell has to be able to discover its flags.
#[test]
fn name_help_documents_the_flags() {
    let out = fleet().args(["name", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--all",
        "--apply",
        "--dry-run",
        "--no-tmux-sync",
        "--refresh",
    ] {
        assert!(
            text.contains(flag),
            "`name --help` is missing {flag}:\n{text}"
        );
    }
}

// A cached name that should never have been cached used to be escapable only by
// hand-editing `~/.claude/fleet-names.json`.
#[test]
fn name_offers_a_way_past_the_cache() {
    let out = fleet()
        .args(["name", "--refresh", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "--refresh is not accepted");
    // `--no-cache` says the same thing and is the name people reach for.
    let out = fleet()
        .args(["name", "--no-cache", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "--no-cache is not accepted");
}

// The waiting hold is the right default, but a session that is essentially
// always waiting on a prompt must not become unrenameable by every path there is.
#[test]
fn rename_help_documents_the_force_escape_hatch() {
    let out = fleet().args(["rename", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--force"), "{text}");
    assert!(text.contains("--no-tmux-sync"), "{text}");
}

// A target that matches nothing must fail loudly rather than renaming whatever
// happens to be first.
#[test]
fn name_with_an_unknown_target_exits_non_zero() {
    let out = fleet()
        .args(["name", "definitely-not-a-live-session"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("no live session matches"), "{text}");
}

// …and so must no target at all: `name` with neither a session nor --all is a
// mistake, not "name everything".
#[test]
fn name_needs_a_target_or_all() {
    let out = fleet().args(["name"]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--all"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// The whole feature has to be inert under a fixture: suggestions still come out
// (from the branch/title heuristic), but no `claude` child is spawned and no
// tmux session is touched. Guarded by a timeout — a real model call is ~8s, so
// anything near that means the LLM path leaked into a demo run.
#[test]
fn name_dry_run_under_a_fixture_suggests_without_calling_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"app-9d","cwd":"/tmp/wt/api-server",
             "status":"idle","tmux_session":"api-server","backend":"tmux","handle":"%99",
             "name_source":"derived","title":"make the cache warmup idempotent"},
            {"pid":43,"session_id":"fixture-2","name":"chosen-by-hand","cwd":"/tmp","status":"idle",
             "tmux_session":"other","backend":"tmux","handle":"%98","name_source":"user",
             "title":"something else"}]"#,
    )
    .unwrap();

    let started = std::time::Instant::now();
    let out = fleet()
        .args(["name", "--all", "--dry-run"])
        .env("FLEET_FIXTURE", &path)
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("naming is inert"), "{text}");
    // The derived-name session gets a suggestion from its title…
    assert!(text.contains("app-9d"), "{text}");
    assert!(text.contains("heuristic"), "{text}");
    assert!(text.contains("make-the-cache-warmup"), "{text}");
    // …and the one a human already named is left out of `--all` entirely.
    assert!(!text.contains("chosen-by-hand"), "{text}");
    // Nowhere near a model call's ~8s.
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

// Applying under a fixture must still refuse at the backend, exactly like
// `send`/`rename` do — a canned handle points at whatever really answers to it.
#[test]
fn name_apply_under_a_fixture_still_refuses_to_drive_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"app-9d","cwd":"/tmp","status":"idle",
             "tmux_session":"demo","backend":"tmux","handle":"%99","name_source":"derived",
             "title":"add retry to the uploader"}]"#,
    )
    .unwrap();
    let out = fleet()
        .args(["name", "--all", "--apply"])
        .env("FLEET_FIXTURE", &path)
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("fixture mode"), "{text}");
}

// `FLEET_FIXTURE` feeds the views a canned fleet: the golden-buffer tests use
// it, and it doubles as a demo mode when there's nothing running.
#[test]
fn fixture_mode_replaces_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"cache-warmup-scheduler",
             "cwd":"/tmp/wt/api-server","status":"waiting","waiting_for":"input needed",
             "tmux_session":"api-server","backend":"tmux","name_source":"user",
             "title":"make the cache warmup idempotent"}]"#,
    )
    .unwrap();

    let out = fleet()
        .args(["list"])
        .env("FLEET_FIXTURE", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("1 live session"), "{text}");
    assert!(text.contains("cache-warmup-scheduler"), "{text}");
    // The tmux session is surfaced as its own column now.
    assert!(text.contains("⧉ api-server"), "{text}");
    assert!(text.contains("needs you"), "{text}");

    // …and round-trips through `list --json`, including the new fields.
    let out = fleet()
        .args(["list", "--json"])
        .env("FLEET_FIXTURE", &path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["tmux_session"], "api-server");
    assert_eq!(v[0]["name_source"], "user");
    assert_eq!(v[0]["waiting_for"], "input needed");
    // The one title, computed in core: the chosen Claude name.
    assert_eq!(v[0]["display_title"], "cache-warmup-scheduler");
}

// A derived Claude name never becomes the display title while something better
// is known — and `rename --json` in fixture mode reports without typing.
#[test]
fn display_title_skips_a_derived_name_and_rename_json_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"app-9d","name_source":"derived",
             "cwd":"/tmp/app","status":"waiting","backend":"tmux","handle":"%99",
             "title":"why is the statusline blank"}]"#,
    )
    .unwrap();
    let out = fleet()
        .args(["list", "--json"])
        .env("FLEET_FIXTURE", &path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["display_title"], "why-is-the-statusline-blank");
    assert_eq!(v[0]["name"], "app-9d");

    // Waiting on a prompt: held, nothing typed, exit 3, and the report says why.
    let out = fleet()
        .args(["rename", "fixture-1", "Fix statusline", "--json"])
        .env("FLEET_FIXTURE", &path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["ok"], false);
    assert_eq!(r["result"], "held");
    assert_eq!(r["held"], "waiting");
    assert_eq!(r["title"], "Fix statusline");
    assert!(
        r["message"].as_str().unwrap().contains("waiting on you"),
        "{r}"
    );
}

// Fixture mode is advertised as a demo/screenshot mode, so it has to be inert:
// the handles in a canned fleet are fabricated, and driving tmux/AppleScript with
// them pokes whatever really answers to them.
#[test]
fn fixture_mode_does_not_drive_the_backends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"demo","cwd":"/tmp","status":"idle",
             "tmux_session":"demo","backend":"tmux","handle":"%99"}]"#,
    )
    .unwrap();

    for args in [
        vec!["peek", "demo"],
        vec!["send", "demo", "hello"],
        vec!["rename", "demo", "other"],
    ] {
        let out = fleet()
            .args(&args)
            .env("FLEET_FIXTURE", &path)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{args:?} should refuse: {text}");
        assert!(text.contains("fixture mode"), "{args:?}: {text}");
    }
}

// …and it must not leave the watch state file behind for sessions that never
// existed — a fixture run used to write transitions into ~/.claude.
#[test]
fn fixture_mode_does_not_write_the_watch_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    std::fs::write(
        &path,
        r#"[{"pid":42,"session_id":"fixture-1","name":"demo","cwd":"/tmp","status":"idle",
             "tmux_session":"demo","backend":"tmux","handle":"%99"}]"#,
    )
    .unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let env = common::Env::new();
    let mut child = env
        .std_cmd()
        // Piped stdout takes the notify-only path, which polls on the same tick.
        .args(["watch", "--interval", "1", "--stuck", "0"])
        .env("FLEET_FIXTURE", &path)
        .env("HOME", &home)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();

    // It really did run a pass over the fixture…
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("demo"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    // …without persisting anything about it.
    assert!(!home.join(".claude/fleet-watch-state.json").exists());
}

// clap only checks a command's definition when that command is parsed, so a
// clash (a subcommand flag shadowing a global one) hides until someone runs it.
#[test]
fn every_subcommand_parses_its_help() {
    for sub in [
        "list", "peek", "send", "rename", "name", "spawn", "handoff", "watch", "tmux", "enter",
        "last", "new", "exec", "ssh", "doctor", "init", "config", "install", "web", "skill",
    ] {
        let out = fleet().args([sub, "--help"]).output().unwrap();
        assert!(
            out.status.success(),
            "{sub}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for sub in ["list", "enter", "last", "new", "kill", "rename", "stale"] {
        let out = fleet().args(["tmux", sub, "--help"]).output().unwrap();
        assert!(
            out.status.success(),
            "tmux {sub}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for sub in [
        ["web", "serve"],
        ["web", "install-service"],
        ["config", "set"],
        ["skill", "install"],
    ] {
        let out = fleet().args(sub).arg("--help").output().unwrap();
        assert!(
            out.status.success(),
            "{sub:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// `fleet group`: model off in config → the repository fallback, persisted only
// with --apply, read back by --cached; `-n` with the model on calls nothing.
#[test]
fn group_falls_back_persists_with_apply_and_reads_back_cached() {
    let env = common::Env::new();
    env.write_config(&serde_json::json!({
        "version": 1, "self": "laptop", "grouping": { "enabled": false }
    }));
    let state = env.path("state/groups.json");
    let input = env.path("sessions.json");
    std::fs::write(
        &input,
        serde_json::json!({ "hosts": [
            { "name": "laptop", "ok": true, "sessions": [
                { "session_id": "a1", "name": "board", "cwd": "~/Code/project" },
                { "session_id": "a2", "name": "api", "cwd": "~/Code/project/.worktrees/api" }
            ]},
            { "name": "workstation", "ok": true, "sessions": [
                { "session_id": "b1", "name": "notes", "cwd": "~/Code/notes" }
            ]},
            { "name": "other", "ok": false, "error": "unreachable", "sessions": [] }
        ]})
        .to_string(),
    )
    .unwrap();
    let group = |args: &[&str]| {
        let out = env
            .cmd()
            .env("FLEET_GROUPS_STATE", &state)
            .arg("group")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("json")
    };
    let inp = input.to_str().unwrap();

    let v = group(&["--input", inp, "--json"]);
    assert_eq!(v["applied"], false);
    assert!(!state.exists(), "no --apply, no write");
    assert_eq!(v["lastRun"]["mode"], "fallback");
    assert_eq!(v["lastRun"]["modelCalls"], 0);
    assert_eq!(v["groups"][0]["id"], "repo-project");
    assert_eq!(v["groups"][0]["members"].as_array().unwrap().len(), 2);
    assert_eq!(v["hosts"]["other"], "unreachable");

    let v = group(&["--input", inp, "--json", "--apply"]);
    assert_eq!(v["applied"], true);
    assert!(state.exists());

    let v = group(&["--cached", "--json"]);
    assert_eq!(v["groups"].as_array().unwrap().len(), 2);
    assert_eq!(v["lastRun"]["classified"], 3);

    // Model on, dry run: prompts are counted, nothing is called or written.
    env.write_config(&serde_json::json!({ "version": 1, "self": "laptop" }));
    let before = std::fs::read_to_string(&state).unwrap();
    let v = group(&["-n", "--input", inp, "--json", "--apply"]);
    assert_eq!(v["lastRun"]["mode"], "dry-run");
    assert_eq!(v["lastRun"]["modelCalls"], 0);
    assert!(
        v["lastRun"]["note"]
            .as_str()
            .unwrap()
            .contains("would make 1 model call")
    );
    assert_eq!(std::fs::read_to_string(&state).unwrap(), before);
}

// `fleet group` after a reboot: a session dormant in this machine's snapshot keeps its
// group (not pruned) and is flagged `dormant: true`; one gone for good is pruned.
#[test]
fn group_keeps_dormant_sessions() {
    let env = common::Env::new();
    env.write_config(&serde_json::json!({
        "version": 1, "self": "laptop", "grouping": { "enabled": false }
    }));
    let state = env.path("state/groups.json");
    let input = env.path("sessions.json");
    let write_input = |ids: &[&str]| {
        let rows: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({ "session_id": id, "name": id, "cwd": "~/Code/project" }))
            .collect();
        std::fs::write(
            &input,
            serde_json::json!({ "hosts": [{ "name": "laptop", "ok": true, "sessions": rows }] })
                .to_string(),
        )
        .unwrap();
    };
    let group = || {
        let out = env
            .cmd()
            .env("FLEET_GROUPS_STATE", &state)
            .env("FLEET_BOOT_ID", "boot-2")
            .args(["group", "--apply", "--json", "--input"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("json")
    };
    write_input(&["a1", "a2", "a3"]);
    let v = group();
    assert_eq!(v["groups"][0]["members"].as_array().unwrap().len(), 3);

    std::fs::write(
        env.path("snapshot.json"),
        serde_json::json!({
            "version": 1, "host": "laptop", "bootId": "boot-2",
            "tmux": [], "iterm": [],
            "dormant": { "tmux": [], "iterm": [ { "sessionId": "a2", "name": "a2", "flags": [] } ] }
        })
        .to_string(),
    )
    .unwrap();
    write_input(&["a1"]);
    let v = group();
    assert_eq!(v["lastRun"]["pruned"], 1, "a3 only");
    let members = v["groups"][0]["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    let a2 = members.iter().find(|m| m["id"] == "a2").unwrap();
    assert_eq!(a2["dormant"], true);
    let a1 = members.iter().find(|m| m["id"] == "a1").unwrap();
    assert!(a1.get("dormant").is_none());
}

// `pid:` / `id:` are exact: a pid nobody has any more is "not found", never the session
// whose title carries those digits — while the bare pid still walks the ladder onto it.
#[test]
fn exact_targets_never_fall_through() {
    let env = common::Env::new();
    let path = common::fixture(
        env.dir.path(),
        r#"[{"pid":42,"session_id":"aaaaaaaa-1111-2222-3333-444444444444","name":"retry-4242-uploads",
             "name_source":"user","cwd":"/tmp","status":"idle","backend":"tmux","handle":"%99"}]"#,
    );
    let run = |args: &[&str]| {
        env.cmd()
            .args(args)
            .env("FLEET_FIXTURE", &path)
            .env("HOME", env.path("home"))
            .output()
            .unwrap()
    };
    // Bare: resolves to the title (and stops at the inert fixture backend).
    let out = run(&["send", "4242", "hello"]);
    assert!(
        common::stderr(&out).contains("fixture mode"),
        "{}",
        common::stderr(&out)
    );
    // Exact: nothing has pid 4242.
    for args in [
        vec!["send", "pid:4242", "hello"],
        vec!["peek", "pid:4242"],
        vec!["rename", "pid:4242", "x"],
        vec!["name", "pid:4242"],
        vec!["send", "id:aaaaaaaa", "hello"],
    ] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = common::stderr(&out);
        assert!(err.contains("no live session has"), "{args:?}: {err}");
        assert!(
            !err.contains("fixture mode"),
            "{args:?} reached a backend: {err}"
        );
    }
    // The exact forms reach the session they name.
    for target in ["pid:42", "id:AAAAAAAA-1111-2222-3333-444444444444"] {
        let out = run(&["send", target, "hello"]);
        assert!(common::stderr(&out).contains("fixture mode"), "{target}");
    }
    // `--json`: one failure object on stdout.
    let out = run(&["rename", "pid:4242", "x", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "target_not_found");
    assert!(v["error"].as_str().unwrap().contains("pid 4242"), "{v}");
    let out = run(&["rename", "pid:x", "x", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "target_invalid");
}

// Claude's default model option names what Claude Code would start with: `ANTHROPIC_MODEL`,
// else `model` in `$CLAUDE_CONFIG_DIR/settings.json` (else `~/.claude/settings.json`).
#[test]
fn harness_list_labels_claude_default_with_the_configured_model() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    let env = common::Env::new();
    let claude = env.path("bin/claude");
    std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
    std::fs::write(&claude, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = env.path("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    let home_claude = env.path("home/.claude");
    std::fs::create_dir_all(&home_claude).unwrap();

    let default_label = |config_dir: Option<&Path>, model: Option<&str>| {
        let mut c = env.cmd();
        c.args(["harness", "list", "--json"])
            .env("FLEET_CMD", &claude);
        if let Some(d) = config_dir {
            c.env("CLAUDE_CONFIG_DIR", d);
        }
        if let Some(m) = model {
            c.env("ANTHROPIC_MODEL", m);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", common::stderr(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let h = v
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == "claude")
            .unwrap()
            .clone();
        assert_eq!(h["models"][0]["id"], "", "{h}");
        h["models"][0]["label"].as_str().unwrap().to_string()
    };

    // Nothing configured.
    assert_eq!(default_label(Some(&config), None), "Default");
    // `~/.claude/settings.json` when CLAUDE_CONFIG_DIR is unset.
    std::fs::write(home_claude.join("settings.json"), r#"{"model":"sonnet"}"#).unwrap();
    assert_eq!(default_label(None, None), "Default (sonnet)");
    // CLAUDE_CONFIG_DIR replaces `~/.claude`.
    assert_eq!(default_label(Some(&config), None), "Default");
    std::fs::write(config.join("settings.json"), r#"{"model":"opus[1m]"}"#).unwrap();
    assert_eq!(default_label(Some(&config), None), "Default (opus[1m])");
    // ANTHROPIC_MODEL wins over both.
    assert_eq!(
        default_label(Some(&config), Some("claude-haiku-5-5")),
        "Default (claude-haiku-5-5)"
    );
}
