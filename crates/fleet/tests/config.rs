//! `fleet init` and `fleet config …` against temp config files.

mod common;

use common::{Env, stderr, stdout, two_hosts};
use serde_json::json;

#[test]
fn config_path_honours_fleet_config() {
    let env = Env::new();
    let out = env.cmd().args(["config", "path"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), env.config_path().display().to_string());
}

#[test]
fn config_path_falls_back_to_xdg() {
    let env = Env::new();
    let out = env
        .cmd()
        .env_remove("FLEET_CONFIG")
        .env("XDG_CONFIG_HOME", env.path("xdg"))
        .args(["config", "path"])
        .output()
        .unwrap();
    assert_eq!(
        stdout(&out).trim(),
        env.path("xdg/fleet/config.json").display().to_string()
    );
}

#[test]
fn init_non_interactive_writes_the_documented_shape() {
    let env = Env::new();
    let out = env
        .cmd()
        .args([
            "init",
            "--yes",
            "--self",
            "laptop",
            "--add-host",
            "laptop,web=http://100.x.y.z:7777",
            "--add-host",
            "workstation,ssh=devbox,web=http://100.x.y.z:7777",
            "--spawn-dir",
            "Work=laptop:~/Code/app,workstation:~/src/app",
            "--web-port",
            "7788",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let c = env.read_config();
    assert_eq!(c["version"], 1);
    assert_eq!(c["self"], "laptop");
    assert_eq!(
        c["defaultHost"], "workstation",
        "first remote is the default"
    );
    assert_eq!(c["hosts"]["laptop"]["ssh"], serde_json::Value::Null);
    assert_eq!(c["hosts"]["workstation"]["ssh"], "devbox");
    assert_eq!(c["web"]["port"], 7788);
    assert_eq!(c["web"]["bind"], "0.0.0.0");
    assert_eq!(c["spawnDirs"][0]["label"], "Work");
    assert_eq!(c["spawnDirs"][0]["paths"]["workstation"], "~/src/app");
    assert!(stdout(&out).contains("fleet install --host workstation"));
}

#[test]
fn init_never_overwrites_without_force_and_keeps_unknown_keys() {
    let env = Env::new();
    let mut cfg = two_hosts();
    cfg["web"]["quickReplies"] = json!(["yes", "continue"]);
    cfg["hosts"]["workstation"]["fleetBin"] = json!("~/bin/fleet");
    cfg["claude"] = json!("claude --fast");
    cfg["somethingElse"] = json!({ "keep": true });
    env.write_config(&cfg);
    let before = std::fs::read_to_string(env.config_path()).unwrap();

    let out = env
        .cmd()
        .args([
            "init",
            "--yes",
            "--self",
            "laptop",
            "--add-host",
            "workstation,ssh=devbox2",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--force"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(env.config_path()).unwrap(), before);

    let out = env
        .cmd()
        .args([
            "init",
            "--yes",
            "--force",
            "--self",
            "laptop",
            "--add-host",
            "workstation,ssh=devbox2",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let c = env.read_config();
    assert_eq!(c["hosts"]["workstation"]["ssh"], "devbox2");
    assert_eq!(c["hosts"]["workstation"]["fleetBin"], "~/bin/fleet");
    assert_eq!(c["web"]["quickReplies"][1], "continue");
    assert_eq!(c["claude"], "claude --fast");
    assert_eq!(c["somethingElse"]["keep"], true);
}

#[test]
fn init_refuses_without_a_terminal_or_yes() {
    let env = Env::new();
    let out = env.cmd().arg("init").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert!(!env.config_path().exists());
}

#[test]
fn init_validates_hosts() {
    let env = Env::new();
    let out = env
        .cmd()
        .args(["init", "--yes", "--self", "a", "--add-host", "b"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("ssh="), "{}", stderr(&out));
    // A web-only peer (no ssh) is fine.
    let out = env
        .cmd()
        .args([
            "init",
            "--yes",
            "--self",
            "a",
            "--add-host",
            "b,web=http://b:7777",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    std::fs::remove_file(env.config_path()).unwrap();
    let out = env
        .cmd()
        .args(["init", "--yes", "--self", "a", "--default-host", "zzz"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn init_print_writes_nothing() {
    let env = Env::new();
    let out = env
        .cmd()
        .args(["init", "--yes", "--print", "--self", "solo"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["self"], "solo");
    assert_eq!(v["defaultHost"], "solo");
    assert!(!env.config_path().exists());
}

#[test]
fn config_set_and_get_preserve_the_rest() {
    let env = Env::new();
    let mut cfg = two_hosts();
    cfg["web"]["ui"] = json!("~/ui");
    env.write_config(&cfg);
    for (k, v) in [
        ("hosts.workstation.ssh", "other-alias"),
        ("web.port", "8080"),
        ("tmux", "null"),
        ("tui.rows", "\"1\""),
    ] {
        let out = env.cmd().args(["config", "set", k, v]).output().unwrap();
        assert!(out.status.success(), "{k}: {}", stderr(&out));
    }
    let c = env.read_config();
    assert_eq!(c["hosts"]["workstation"]["ssh"], "other-alias");
    assert_eq!(c["web"]["port"], 8080);
    assert_eq!(c["web"]["ui"], "~/ui");
    assert_eq!(c["tui"]["rows"], "1");
    let out = env
        .cmd()
        .args(["config", "get", "hosts.workstation.ssh"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out).trim(), "other-alias");
    let out = env
        .cmd()
        .args(["config", "get", "nope.nothing"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn config_set_creates_a_config() {
    let env = Env::new();
    let out = env
        .cmd()
        .args(["config", "set", "self", "box"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let c = env.read_config();
    assert_eq!(c["self"], "box");
    assert_eq!(c["version"], 1);
}

#[test]
fn a_broken_config_is_reported_not_clobbered() {
    let env = Env::new();
    std::fs::write(env.config_path(), "{ broken").unwrap();
    let out = env
        .cmd()
        .args(["config", "set", "self", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(env.config_path()).unwrap(),
        "{ broken"
    );
    let out = env.cmd().args(["config", "show"]).output().unwrap();
    assert!(stderr(&out).contains("not valid JSON"), "{}", stderr(&out));
    // Local commands still work on defaults.
    let out = env.cmd().args(["list", "--json"]).output().unwrap();
    assert!(out.status.success());
}

#[test]
fn config_show_resolved_fills_defaults() {
    let env = Env::new();
    let out = env
        .cmd()
        .args(["config", "show", "--resolved"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["self"], "local");
    assert_eq!(v["exists"], false);
    assert_eq!(v["web"]["port"], 7777);
}

#[test]
fn config_show_resolved_marks_hidden_spawn_dirs() {
    let env = Env::new();
    let mut cfg = two_hosts();
    cfg["spawnDirs"] = json!([
        { "label": "Work", "paths": { "laptop": "~/Code/work" } },
        { "label": "Brain", "paths": { "laptop": "~/brain" }, "hidden": true }
    ]);
    env.write_config(&cfg);
    let out = env
        .cmd()
        .args(["config", "show", "--resolved"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["spawnDirs"],
        json!([
            { "label": "Work", "path": "~/Code/work" },
            { "label": "Brain", "path": "~/brain", "hidden": true }
        ])
    );
}
