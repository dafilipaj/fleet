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

// Every command whose `--json` failures are one object answers a command line clap rejects
// with that object too: `usage`, its host included, wherever `--json` and the bad argument
// stand — an unknown flag long or short, alone or in a cluster, before the target or after.
#[test]
fn every_json_usage_error_is_the_same_one_object() {
    let env = common::Env::new();
    // (command, the arguments it needs, the exit code its usage errors get)
    let commands: [(&str, &[&str], i32); 7] = [
        ("messages", &["pid:1"], 2),
        ("send", &["pid:1", "hello"], 2),
        ("answer", &["pid:1"], 2),
        ("peek", &["pid:1"], 2),
        ("keys", &["pid:1", "Enter"], 2),
        ("close", &["pid:1"], 1),
        ("spawn", &["hi"], 1),
    ];
    for (cmd, needs, code) in commands {
        for bad in ["--bogus", "--bogus=1", "-x", "-xy"] {
            let mut orders: Vec<Vec<&str>> = vec![
                [&[cmd, bad, "--json"][..], needs].concat(),
                [&[cmd, "--json", bad][..], needs].concat(),
                [&[cmd][..], needs, &[bad, "--json"]].concat(),
                [&[cmd][..], needs, &["--json", bad]].concat(),
                [&["-n", cmd][..], needs, &["--json", bad]].concat(),
            ];
            orders.dedup();
            for args in orders {
                let out = env.cmd().args(&args).output().unwrap();
                let stdout = common::stdout(&out);
                assert_eq!(out.status.code(), Some(code), "{args:?}: {stdout}");
                assert_eq!(stdout.trim().lines().count(), 1, "{args:?}: {stdout}");
                let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
                let mut keys: Vec<&str> =
                    v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
                keys.sort();
                // `spawn`'s own shape; `answer`'s, whose failures all carry `detail` and
                // `pane` (`null` here: no screen check ran).
                let want: &[&str] = match cmd {
                    "spawn" => &["error", "host", "kind", "ok", "partial"],
                    "answer" => &["detail", "error", "host", "kind", "ok", "pane"],
                    _ => &["error", "host", "kind", "ok"],
                };
                assert_eq!(keys, want, "{args:?}: {v}");
                assert_eq!(
                    (&v["ok"], &v["kind"]),
                    (&false.into(), &"usage".into()),
                    "{args:?}"
                );
                assert!(
                    v["host"].as_str().is_some_and(|h| !h.is_empty()),
                    "{args:?}: {v}"
                );
                let flag = bad.split('=').next().unwrap();
                let shown = if flag == "-xy" { "-x" } else { flag };
                assert!(
                    v["error"].as_str().unwrap().contains(shown),
                    "{args:?}: {v}"
                );
            }
        }
        // Without --json, clap answers as always: nothing on stdout.
        let out = env
            .cmd()
            .args([&[cmd, "--bogus"][..], needs].concat())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{cmd}");
        assert_eq!(common::stdout(&out), "", "{cmd}");
    }
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

// `FLEET_PATH_ONLY=1`: a harness installed only in a fallback dir (`~/.local/bin`) is not
// found — PATH is all a sandbox lets fleet see. Without it, the fallback dirs still count.
#[test]
fn path_only_hides_binaries_outside_path() {
    use std::os::unix::fs::PermissionsExt;
    let env = common::Env::new();
    let codex = env.path("home/.local/bin/codex");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    std::fs::write(&codex, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let empty_path = env.path("empty-bin");
    std::fs::create_dir_all(&empty_path).unwrap();

    let installed = |path_only: Option<&str>| {
        let mut c = env.cmd();
        c.args(["harness", "list", "--json", "--no-models"])
            .env("PATH", &empty_path);
        if let Some(v) = path_only {
            c.env("FLEET_PATH_ONLY", v);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", common::stderr(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let h = v
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == "codex")
            .unwrap()
            .clone();
        h["installed"].as_bool().unwrap()
    };

    assert!(installed(None));
    assert!(installed(Some("0")));
    assert!(!installed(Some("1")));
}

/// A `PATH` of stand-ins that only log that they ran, to `<env>/ran`: `ssh`, `osascript`,
/// `node`, `npm`, `launchctl`, `agent` (a launcher), `sh` (what a tmux pane would start with),
/// and `zsh` — a login shell that answers "not found" to the launcher probe. Proves a refusal
/// ran nothing.
struct Stubs {
    bin: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl Stubs {
    fn new(env: &common::Env) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let bin = env.path("stub-bin");
        let log = env.path("ran");
        std::fs::create_dir_all(&bin).unwrap();
        for name in [
            "ssh",
            "osascript",
            "node",
            "npm",
            "launchctl",
            "agent",
            "sh",
            "zsh",
        ] {
            let answer = if name == "zsh" {
                "echo fleet:missing\n"
            } else {
                ""
            };
            let p = bin.join(name);
            std::fs::write(
                &p,
                format!(
                    "#!/bin/sh\necho \"{name} $*\" >> '{}'\n{answer}",
                    log.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Stubs { bin, log }
    }

    fn ran(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// `fleet` under `FLEET_PATH_ONLY=1` (or without it), `PATH` the stubs alone.
    fn cmd(&self, env: &common::Env, path_only: bool) -> assert_cmd::Command {
        let mut c = env.cmd();
        c.env("PATH", &self.bin)
            .env("SHELL", self.bin.join("zsh"))
            .env("FLEET_CMD", self.bin.join("agent"))
            .timeout(std::time::Duration::from_secs(30));
        if path_only {
            c.env("FLEET_PATH_ONLY", "1");
        }
        c
    }
}

/// The refusal every out-of-scope feature gives under `FLEET_PATH_ONLY`: exit 1, the message
/// naming the switch.
fn assert_refused(out: &std::process::Output, what: &str) {
    let err = common::stderr(out);
    assert_eq!(out.status.code(), Some(1), "{what}: {err}");
    assert!(
        err.contains(&format!("{what} is not available with FLEET_PATH_ONLY set")),
        "{what}: {err}"
    );
}

// Under FLEET_PATH_ONLY a launcher not on PATH is missing at once: no login shell is asked
// (its profile would look past PATH). Without the switch the shell is still asked.
#[test]
fn path_only_never_probes_a_login_shell_for_the_launcher() {
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    let dir = env.path("work");
    std::fs::create_dir_all(&dir).unwrap();
    let spawn = |path_only: bool| {
        let out = stubs
            .cmd(&env, path_only)
            .env("FLEET_CMD", "fleet-test-no-such-agent")
            .args(["spawn", "--backend", "tmux", "--json", "--dir"])
            .arg(&dir)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", common::stderr(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["kind"], "harness_missing", "{v}");
        v["error"].as_str().unwrap().to_string()
    };

    let err = spawn(true);
    assert!(err.contains("not on PATH (FLEET_PATH_ONLY"), "{err}");
    assert_eq!(stubs.ran(), "", "nothing ran, the login shell least of all");

    spawn(false);
    assert!(stubs.ran().starts_with("zsh -l -i -c "), "{}", stubs.ran());
}

// `fleet web serve`, every service install and the iTerm backend reach past PATH by design:
// under FLEET_PATH_ONLY they are refused (kind `usage`) before anything runs — even a dry
// run or `--print`.
#[test]
fn path_only_refuses_the_web_server_service_installs_and_iterm() {
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    let refused: [(&[&str], &str); 4] = [
        (&["-n", "web", "serve"], "`fleet web serve`"),
        (
            &["web", "install-service", "--print"],
            "`fleet web install-service`",
        ),
        (
            &["jobs", "install-service", "--print"],
            "`fleet jobs install-service`",
        ),
        (
            &["repos", "install-service", "--print"],
            "`fleet repos install-service`",
        ),
    ];
    for (args, what) in refused {
        let out = stubs.cmd(&env, true).args(args).output().unwrap();
        assert_refused(&out, what);
    }

    // iTerm: refused as a spawn's backend, with the `--json` object's kind…
    let dir = env.path("work");
    std::fs::create_dir_all(&dir).unwrap();
    let out = stubs
        .cmd(&env, true)
        .args(["spawn", "--backend", "iterm", "--json", "--dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_refused(&out, "the iTerm backend");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "usage", "{v}");
    assert_eq!(v["partial"], serde_json::Value::Null, "{v}");
    // …and never the default one: a spawn nobody pointed anywhere lands in tmux.
    let out = stubs
        .cmd(&env, true)
        .args(["-n", "spawn", "--json", "--dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["backend"], "tmux", "{v}");

    assert_eq!(stubs.ran(), "", "a refusal runs nothing");
}

// Under FLEET_PATH_ONLY another host is refused, however it is named — `-H`, `FLEET_HOST`, the
// config's `defaultHost`, an ad-hoc ssh destination, a host reached only through its web
// server — and nothing is forwarded. Fan-outs skip each such host with the same refusal.
// This machine (`-H local`) still works.
#[test]
fn path_only_refuses_other_hosts() {
    let env = common::Env::new();
    let mut cfg = common::two_hosts();
    cfg["hosts"]["shed"] = serde_json::json!({ "ssh": null, "web": "http://127.0.0.1:9/" });
    env.write_config(&cfg);
    let stubs = Stubs::new(&env);

    let refused: [(&[&str], Option<&str>, &str); 5] = [
        (&["-H", "workstation", "list"], None, "workstation"),
        (&["list"], Some("workstation"), "workstation"),
        (&["tmux", "list"], None, "workstation"),
        (&["-H", "some-alias", "peek", "x"], None, "some-alias"),
        (&["-H", "shed", "stack", "list"], None, "shed"),
    ];
    for (args, fleet_host, host) in refused {
        let mut c = stubs.cmd(&env, true);
        if let Some(h) = fleet_host {
            c.env("FLEET_HOST", h);
        }
        let out = c.args(args).output().unwrap();
        assert_refused(&out, &format!("another host (`{host}`)"));
    }

    // `spawn --json` still answers with one object.
    let out = stubs
        .cmd(&env, true)
        .args(["-H", "workstation", "spawn", "--json", "--dir", "~/x", "hi"])
        .output()
        .unwrap();
    assert_refused(&out, "another host (`workstation`)");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (&v["kind"], &v["host"]),
        (&"usage".into(), &"workstation".into())
    );

    // A fan-out reports the host as refused and lists this machine.
    let out = stubs
        .cmd(&env, true)
        .args(["list", "--all-hosts", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    assert!(
        common::stderr(&out).contains("workstation: another host (`workstation`) is not available"),
        "{}",
        common::stderr(&out)
    );

    let out = stubs
        .cmd(&env, true)
        .args(["-H", "local", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    assert_eq!(stubs.ran(), "", "nothing went over ssh");

    // Without the switch `-H` is forwarded over ssh as ever.
    let out = stubs
        .cmd(&env, false)
        .args(["-H", "workstation", "list"])
        .output()
        .unwrap();
    assert!(stubs.ran().starts_with("ssh "), "{}", common::stderr(&out));
}

/// `out`'s stdout as the one `--json` failure object of a FLEET_PATH_ONLY refusal of `what`.
fn json_refusal(out: &std::process::Output, what: &str) -> serde_json::Value {
    assert_refused(out, what);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{what}: {e}: {}", common::stdout(out)));
    assert_eq!(
        (&v["ok"], &v["kind"]),
        (&false.into(), &"usage".into()),
        "{v}"
    );
    assert!(v["error"].as_str().unwrap().contains(what), "{v}");
    v
}

// A host refusal is a usage error the command itself would print as JSON: with `--json`, one
// failure object on stdout, whichever command and however the host is named.
#[test]
fn path_only_host_refusals_answer_json() {
    let env = common::Env::new();
    env.write_config(&common::two_hosts());
    let stubs = Stubs::new(&env);
    let refused: [(&[&str], Option<&str>); 8] = [
        (&["-H", "workstation", "peek", "x", "--json"], None),
        (&["-H", "workstation", "send", "x", "hi", "--json"], None),
        (&["-H", "workstation", "keys", "x", "Enter", "--json"], None),
        (&["-H", "workstation", "close", "x", "--json"], None),
        (&["-H", "workstation", "messages", "x", "--json"], None),
        (&["-H", "workstation", "list", "--json"], None),
        (&["peek", "x", "--json"], Some("workstation")),
        // The default host (`tmux` commands) is the remote one.
        (&["tmux", "list", "--json"], None),
    ];
    for (args, fleet_host) in refused {
        let mut c = stubs.cmd(&env, true);
        if let Some(h) = fleet_host {
            c.env("FLEET_HOST", h);
        }
        let out = c.args(args).output().unwrap();
        let v = json_refusal(&out, "another host (`workstation`)");
        assert_eq!(v["host"], "laptop", "{args:?}: {v}");
    }
    // Without `--json`, the plain error alone.
    let out = stubs
        .cmd(&env, true)
        .args(["-H", "workstation", "peek", "x"])
        .output()
        .unwrap();
    assert_refused(&out, "another host (`workstation`)");
    assert_eq!(common::stdout(&out), "");
    // `answer`'s failure object carries `detail` and `pane` there too, both `null`.
    let out = stubs
        .cmd(&env, true)
        .args(["-H", "workstation", "answer", "x", "--list", "--json"])
        .output()
        .unwrap();
    let v = json_refusal(&out, "another host (`workstation`)");
    for k in ["detail", "pane"] {
        assert_eq!(v.get(k), Some(&serde_json::Value::Null), "{k}: {v}");
    }
    assert_eq!(stubs.ran(), "", "nothing went over ssh");
}

// Under FLEET_PATH_ONLY an iTerm spawn is refused before anything is written for it — a dry
// run included, which would otherwise promise an iTerm spawn: no handoff brief, no resume stamp.
#[test]
fn path_only_refuses_iterm_before_writing_anything() {
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    let dir = env.path("work");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.display().to_string();
    let iterm = "the iTerm backend";

    let out = stubs
        .cmd(&env, true)
        .args(["-n", "spawn", "--backend", "iterm", "--json", "--dir", &dir])
        .output()
        .unwrap();
    let v = json_refusal(&out, iterm);
    assert_eq!(v["partial"], serde_json::Value::Null, "{v}");

    let out = stubs
        .cmd(&env, true)
        .args(["handoff", "take the login bug", "--backend", "iterm"])
        .args(["--no-wait", "--dir", &dir])
        .output()
        .unwrap();
    assert_refused(&out, iterm);
    let handoffs = env.path("home/.claude/fleet-handoffs");
    assert!(
        std::fs::read_dir(&handoffs).map_or(true, |mut d| d.next().is_none()),
        "a handoff brief was written"
    );

    let out = stubs
        .cmd(&env, true)
        .args(["spawn", "--resume", "0b7e3c1a-5d2f-4e6a-9c8b-1f2e3d4c5b6a"])
        .args(["--backend", "iterm", "--json", "--dir", &dir])
        .output()
        .unwrap();
    json_refusal(&out, iterm);
    assert!(
        std::fs::read_dir(env.path("locks")).map_or(true, |mut d| d.next().is_none()),
        "a resume was reserved"
    );
    assert_eq!(stubs.ran(), "", "a refusal runs nothing");
}

// `fleet exec` and `fleet ssh` on this machine run `$SHELL -l`, whose profile looks past PATH:
// refused under FLEET_PATH_ONLY, dry runs included. Without it, a dry run shows the command.
#[test]
fn path_only_refuses_a_local_exec_and_ssh() {
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    for (args, what) in [
        (&["-n", "exec", "--", "true"][..], "`fleet exec`"),
        (&["exec", "--", "true"], "`fleet exec`"),
        (&["-n", "ssh"], "`fleet ssh`"),
        (&["-n", "ssh", "true"], "`fleet ssh`"),
    ] {
        let out = stubs.cmd(&env, true).args(args).output().unwrap();
        assert_refused(&out, what);
    }
    assert_eq!(stubs.ran(), "", "no shell ran");

    let out = stubs
        .cmd(&env, false)
        .args(["-n", "exec", "--", "true"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    assert!(
        common::stdout(&out).contains(" -l -c "),
        "{}",
        common::stdout(&out)
    );
}

// Under FLEET_PATH_ONLY a PATH that is unset or empty is refused before the command runs: a
// bare-name exec would fall back to the C library's default path. A `--json` command still
// answers with its failure object.
#[test]
fn path_only_needs_a_path() {
    let env = common::Env::new();
    let dir = env.path("work");
    std::fs::create_dir_all(&dir).unwrap();
    let what = "PATH is unset or empty";
    for path in [None, Some("")] {
        let cmd = || {
            let mut c = env.cmd();
            c.env("FLEET_PATH_ONLY", "1");
            match path {
                Some(p) => c.env("PATH", p),
                None => c.env_remove("PATH"),
            };
            c
        };
        let out = cmd().args(["list"]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{path:?}");
        assert!(
            common::stderr(&out).contains(what),
            "{}",
            common::stderr(&out)
        );
        assert_eq!(common::stdout(&out), "");

        let out = cmd().args(["peek", "x", "--json"]).output().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["kind"], "usage", "{v}");
        assert!(v["error"].as_str().unwrap().contains(what), "{v}");

        let out = cmd()
            .args(["spawn", "--json", "--dir"])
            .arg(&dir)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            (&v["kind"], &v["partial"]),
            (&"usage".into(), &serde_json::Value::Null)
        );

        // Without the switch, no PATH is just no PATH.
        let mut c = env.cmd();
        match path {
            Some(p) => c.env("PATH", p),
            None => c.env_remove("PATH"),
        };
        let out = c.args(["list", "--json"]).output().unwrap();
        assert!(out.status.success(), "{}", common::stderr(&out));
    }
}

// Under FLEET_PATH_ONLY a PATH with a relative or empty entry is refused at startup too: tmux
// would resolve it against each pane's own directory.
#[test]
fn path_only_refuses_a_relative_path_entry() {
    let env = common::Env::new();
    for (path, named) in [("bin", "`bin`"), ("/usr/bin::/bin", "an empty entry")] {
        let out = env
            .cmd()
            .env("FLEET_PATH_ONLY", "1")
            .env("PATH", path)
            .args(["peek", "x", "--json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{path}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["kind"], "usage", "{v}");
        let error = v["error"].as_str().unwrap();
        assert!(error.contains(named), "{v}");
        assert!(error.contains("not an absolute directory"), "{v}");
    }
}

// `doctor` under FLEET_PATH_ONLY says each other host is refused, and does not count that as a
// problem: this machine alone decides its exit code.
#[test]
fn path_only_doctor_skips_other_hosts_without_failing() {
    let env = common::Env::new();
    env.write_config(&common::two_hosts());
    let stubs = Stubs::new(&env);
    let out = stubs.cmd(&env, true).arg("doctor").output().unwrap();
    let text = common::stdout(&out);
    assert!(out.status.success(), "{text}\n{}", common::stderr(&out));
    assert!(
        text.contains("another host (`workstation`) is not available with FLEET_PATH_ONLY set"),
        "{text}"
    );
    assert!(!stubs.ran().contains("ssh "), "{}", stubs.ran());
}

/// The session id of [`iterm_session`].
const ITERM_ID: &str = "1f2e3d4c-5b6a-4e6a-9c8b-0b7e3c1a5d2f";

/// A Claude session in iTerm, as discovery sees one: a registered process with a terminal of
/// its own, whose environment carries `ITERM_SESSION_ID` and no `TMUX`. It is a `tmux wait-for`
/// client idling in a pane of the test's private server, which gives it the terminal; unlike a
/// platform binary's (`sleep`, `sh`) on macOS, its environment is one `ps` can read. `false`
/// when tmux is not installed. It goes with the server when the `Env` drops.
fn iterm_session(env: &common::Env) -> bool {
    if fleet::core::tools::find_binary("tmux").is_none() {
        return false;
    }
    let wrapper = env.tmux_wrapper().display().to_string();
    let out = env.tmux(&[
        "new-session",
        "-d",
        "-s",
        "fleet-test-iterm",
        "-P",
        "-F",
        "#{pane_pid}",
        "/usr/bin/env",
        "-u",
        "TMUX",
        "-u",
        "TMUX_PANE",
        "ITERM_SESSION_ID=w0t0p0:FLEET-TEST-ITERM",
        &wrapper,
        "wait-for",
        "fleet-test-never",
    ]);
    assert!(out.status.success(), "{}", common::stderr(&out));
    let pid: i64 = common::stdout(&out).trim().parse().unwrap();
    // Until `env` and the wrapper have exec'd the tmux client, its environment is not the one
    // to read.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ps = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        if common::stdout(&ps).trim().ends_with("tmux") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no iTerm stand-in");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let sessions = env.path("home/.claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let reg = serde_json::json!({
        "pid": pid, "sessionId": ITERM_ID, "cwd": env.dir.path(), "name": "iterm-tab",
        "nameSource": "user", "status": "idle", "kind": "interactive", "entrypoint": "cli",
        "procStart": common::fake::proc_start(pid),
    });
    std::fs::write(sessions.join(format!("{pid}.json")), reg.to_string()).unwrap();
    true
}

/// [`Stubs::cmd`] with the system dirs after the stubs, for `ps` (discovery).
fn with_ps(stubs: &Stubs, env: &common::Env, path_only: bool) -> assert_cmd::Command {
    let mut c = stubs.cmd(env, path_only);
    c.env("PATH", format!("{}:/usr/bin:/bin", stubs.bin.display()));
    c
}

// Under FLEET_PATH_ONLY anything that drives an iTerm session is refused (kind `usage`)
// before `osascript` runs; `list` leaves the tab title out rather than ask iTerm for it.
// Without the switch the very same `list` asks (the stub runs): the setup reaches the gate.
#[test]
fn path_only_never_drives_iterm() {
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    if !iterm_session(&env) {
        return;
    }
    let target = format!("id:{ITERM_ID}");

    let out = with_ps(&stubs, &env, true)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows[0]["backend"], "iterm", "{rows}");
    assert_eq!(rows[0]["tab"], serde_json::Value::Null, "{rows}");

    for args in [
        &["peek", &target, "--json"][..],
        &["send", &target, "hi", "--json"],
        &["keys", &target, "Enter", "--json"],
    ] {
        let out = with_ps(&stubs, &env, true).args(args).output().unwrap();
        json_refusal(&out, "the iTerm backend");
    }
    assert_eq!(stubs.ran(), "", "osascript ran");

    let out = with_ps(&stubs, &env, false)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    assert!(stubs.ran().starts_with("osascript "), "{}", stubs.ran());
}

// `claude` installed only in `~/.claude/local` (a hard-coded install location) is not there
// under FLEET_PATH_ONLY: the default launcher is `harness_missing`.
#[test]
fn path_only_ignores_claude_in_its_install_dir() {
    use std::os::unix::fs::PermissionsExt;
    let env = common::Env::new();
    let stubs = Stubs::new(&env);
    let local = env.path("home/.claude/local");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("claude"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(local.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = env.path("work");
    std::fs::create_dir_all(&dir).unwrap();
    let out = stubs
        .cmd(&env, true)
        .env_remove("FLEET_CMD")
        .args(["spawn", "--backend", "tmux", "--json", "--dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", common::stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "harness_missing", "{v}");
    assert!(v["error"].as_str().unwrap().contains("`claude`"), "{v}");
    assert_eq!(stubs.ran(), "");
}

// `fleet tmux new` under FLEET_PATH_ONLY gives its session fleet's PATH and the switch (tmux
// `-e`), over the server's environment, as a spawn does.
#[test]
fn path_only_tmux_new_hands_the_session_fleets_path() {
    use std::os::unix::fs::PermissionsExt;
    if fleet::core::tools::find_binary("tmux").is_none() {
        return;
    }
    let env = common::Env::new();
    let bin = env.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let idle = bin.join("idle");
    std::fs::write(&idle, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&idle, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    // A server whose own environment says otherwise.
    let out = env.tmux(&[
        "new-session",
        "-d",
        "-s",
        "fleet-test-base",
        "/bin/sleep",
        "30",
    ]);
    assert!(out.status.success(), "{}", common::stderr(&out));
    let session = |name: &str, path_only: bool| {
        let mut c = env.cmd();
        c.env("PATH", &path).env("SHELL", "/bin/sh");
        if path_only {
            c.env("FLEET_PATH_ONLY", "1");
        }
        let out = c
            .args(["tmux", "new", "-d", name, "--"])
            .arg(&idle)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", common::stderr(&out));
        let shown = |var: &str| {
            common::stdout(&env.tmux(&["show-environment", "-t", name, var]))
                .trim()
                .to_string()
        };
        (shown("PATH"), shown("FLEET_PATH_ONLY"))
    };
    assert_eq!(
        session("fleet-test-p", true),
        (format!("PATH={path}"), "FLEET_PATH_ONLY=1".to_string())
    );
    // Without the switch the session adds nothing of its own.
    assert_eq!(
        session("fleet-test-q", false),
        (String::new(), String::new())
    );
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
