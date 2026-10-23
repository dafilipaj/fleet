//! Spawn launch options, using dry runs and isolated homes; no agent or tmux starts.
mod common;

use common::{Env, stderr, stdout};
use serde_json::Value;

fn spawn(env: &Env, args: &[&str]) -> (std::process::Output, Value) {
    let out = env
        .cmd()
        .args(["spawn", "--backend", "tmux", "--json"])
        .args(args)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {} / {}", stdout(&out), stderr(&out)));
    (out, value)
}

#[test]
fn spawn_launch_options_preserve_order_and_the_original_command() {
    let env = Env::new();
    for (harness, base, effort, permissions) in [
        (
            "claude",
            "claude --model 'example-model' -n 'example'",
            " --effort high",
            " --dangerously-skip-permissions",
        ),
        (
            "codex",
            "codex --model 'example-model'",
            " -c model_reasoning_effort=high",
            " --dangerously-bypass-approvals-and-sandbox",
        ),
        ("pi", "pi --model 'example-model' -n 'example'", "", ""),
    ] {
        for (flags, added) in [
            (vec![], String::new()),
            (vec!["--effort", "high"], effort.to_string()),
            (vec!["--skip-permissions"], permissions.to_string()),
            (
                vec!["--effort", "high", "--skip-permissions"],
                format!("{effort}{permissions}"),
            ),
        ] {
            if harness == "pi" && !flags.is_empty() {
                continue;
            }
            let mut args = vec![
                "-n",
                "--dir",
                "/tmp",
                "--harness",
                harness,
                "--model",
                "example-model",
                "--name",
                "example",
            ];
            args.extend(flags);
            args.extend(["--", "-explain this"]);
            let (out, v) = spawn(&env, &args);
            assert!(out.status.success(), "{}", stderr(&out));
            let prompt = if harness == "pi" {
                " ' -explain this'"
            } else {
                " -- '-explain this'"
            };
            assert_eq!(v["command"], format!("cd '/tmp' && {base}{added}{prompt}"));
            assert_eq!(v["dry_run"], true);
        }
    }
}

#[test]
fn spawn_launch_options_fail_as_usage_before_any_launch_work() {
    let env = Env::new();
    let mut cases = Vec::new();
    for bad in [
        "",
        "High",
        "high1",
        "high low",
        "high;id",
        "é",
        "abcdefghijklmnopq",
    ] {
        cases.push(vec!["--effort", bad]);
    }
    for flags in [vec!["--effort", "high"], vec!["--skip-permissions"]] {
        for conflict in [
            vec!["--harness", "pi"],
            vec!["--resume", "example-session"],
            vec!["--from", "example-session"],
        ] {
            let mut args = flags.clone();
            args.extend(conflict);
            cases.push(args);
        }
    }
    for mut args in cases {
        // A nonexistent dir must never take precedence over these usage errors.
        args.extend(["--dir", "/nonexistent/fleet-test-dir"]);
        let (out, v) = spawn(&env, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        assert_eq!(v["kind"], "usage", "{args:?}: {v}");
        let error = v["error"].as_str().unwrap();
        assert!(!error.is_empty());
        assert!(!error.contains("unexpected argument"), "{args:?}: {v}");
        assert_eq!(v["partial"], Value::Null);
        assert!(!env.path("locks").exists());
        assert!(!env.path("home/.claude").exists());
    }
}

#[test]
fn remote_spawn_launch_options_fail_as_usage_before_dispatch() {
    let env = Env::new();
    env.write_config(&common::two_hosts());
    let mut cases = vec![(vec!["--effort", "High"], "invalid effort level")];
    for flags in [vec!["--effort", "high"], vec!["--skip-permissions"]] {
        for (conflict, error) in [
            (vec!["--harness", "pi"], "require Claude Code or Codex"),
            (vec!["--resume", "example-session"], "cannot be used with"),
            (vec!["--from", "example-session"], "cannot be used with"),
        ] {
            let mut args = flags.clone();
            args.extend(conflict);
            cases.push((args, error));
        }
    }
    for (args, error) in cases {
        let out = env
            .cmd()
            .args([
                "-n",
                "-H",
                "workstation",
                "spawn",
                "--json",
                "--dir",
                "/tmp",
            ])
            .args(&args)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {} / {}", stdout(&out), stderr(&out)));
        assert_eq!(out.status.code(), Some(1), "{args:?}: {v}");
        assert_eq!(v["ok"], false, "{args:?}: {v}");
        assert_eq!(v["kind"], "usage", "{args:?}: {v}");
        assert!(
            v["error"].as_str().unwrap().contains(error),
            "{args:?}: {v}"
        );
        assert_eq!(v["host"], "workstation", "{args:?}: {v}");
        assert_eq!(v["command"], Value::Null, "{args:?}: {v}");
        assert!(!env.path("locks").exists());
        assert!(!env.path("home/.claude").exists());
    }
}

#[test]
fn remote_spawn_forwards_launch_options() {
    let env = Env::new();
    env.write_config(&common::two_hosts());
    let out = env
        .cmd()
        .args([
            "-n",
            "-H",
            "workstation",
            "spawn",
            "--json",
            "--dir",
            "/tmp",
            "--effort",
            "high",
            "--skip-permissions",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let command = v["command"].as_str().unwrap();
    assert!(command.contains("--effort"), "{command}");
    assert!(command.contains("high"), "{command}");
    assert!(command.contains("--skip-permissions"), "{command}");
}

#[test]
fn spawn_accepts_future_effort_words_without_a_model_catalog_check() {
    let env = Env::new();
    for harness in ["claude", "codex"] {
        let (out, v) = spawn(
            &env,
            &[
                "-n",
                "--dir",
                "/tmp",
                "--harness",
                harness,
                "--effort",
                "abcdefghijklmnop",
            ],
        );
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(v["command"].as_str().unwrap().contains("abcdefghijklmnop"));
    }
}
