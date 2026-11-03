//! `fleet usage --json` with no Claude login: exit 1 with the reason on stderr, and
//! `{ error, providers }` on stdout so Codex still reaches a caller. A temp HOME, a
//! fake `security` (no keychain item) and a fixture Codex home — nothing real is read.

mod common;

#[cfg(unix)]
#[test]
fn no_claude_login_still_reports_codex() {
    use std::os::unix::fs::PermissionsExt;

    let env = common::Env::new();
    let home = env.path("home");
    std::fs::create_dir_all(&home).unwrap();
    let bin = env.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let security = bin.join("security");
    std::fs::write(&security, "#!/bin/sh\nexit 44\n").unwrap();
    std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o755)).unwrap();
    let codex = env.path("codex");
    let day = codex.join("sessions/2026/10/16");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join("rollout-fixture.jsonl"),
        include_str!("../../../testdata/codex/rollout-usage.jsonl"),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = env
        .cmd()
        .args(["--local", "usage", "--json"])
        .env("HOME", &home)
        .env("PATH", path)
        .env("CODEX_HOME", &codex)
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no Claude Code login found"), "{stderr}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["error", "providers"]);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("no Claude Code login")
    );
    let p = &v["providers"];
    assert_eq!(
        (p[0]["id"].as_str(), p[0]["status"].as_str()),
        (Some("claude"), Some("error"))
    );
    assert_eq!(p[0]["limits"], serde_json::json!([]));
    assert_eq!(
        (p[1]["id"].as_str(), p[1]["status"].as_str()),
        (Some("codex"), Some("ok"))
    );
    assert_eq!(p[1]["limits"][0]["label"], "5h limit");
}
