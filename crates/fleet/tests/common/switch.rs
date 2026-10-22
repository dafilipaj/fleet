//! What the tests of `fleet model` and `fleet effort` share: where a fake Claude Code's
//! transcript is, a tmux that runs something first on every call, and the failure object both
//! print.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::fake::FakeFleet;
use super::{stderr, stdout};

/// The transcript a Claude session `sid` registered in `f` (cwd: the test dir) is looked up at.
pub fn transcript_of(f: &FakeFleet, sid: &str) -> PathBuf {
    let cwd = f.env.dir.path().display().to_string();
    let encoded: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    f.env
        .path(&format!("home/.claude/projects/{encoded}/{sid}.jsonl"))
}

/// `fleet <args>` with a tmux that first runs `before` (sh, `$*` its arguments) on every call:
/// (exit code, the one JSON object on stdout, stderr, how long it ran).
pub fn under_tmux(f: &FakeFleet, before: &str, args: &[&str]) -> (i32, Value, String, Duration) {
    let wrapper = f.env.path("before-tmux.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n{before}\nexec '{}' \"$@\"\n",
            f.env.tmux_wrapper().display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let began = std::time::Instant::now();
    let out = f
        .env
        .cmd()
        .env("SHELL", "/bin/sh")
        .env("FLEET_TMUX", &wrapper)
        .timeout(Duration::from_secs(20))
        .args(args)
        .output()
        .unwrap();
    let took = began.elapsed();
    let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
    (out.status.code().unwrap_or(-1), v, stderr(&out), took)
}

pub fn keys_of(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

/// `{ ok: false, host, kind, error }`, exit 1 — with `reason` for `busy` and `host_shared`,
/// `status` for `not_idle`, `typed` for `io`.
pub fn failed(code: i32, v: &Value, kind: &str) {
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["kind"], kind, "{v}");
    assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
    assert!(v["host"].as_str().is_some_and(|h| !h.is_empty()), "{v}");
    let mut want = vec!["error", "host", "kind", "ok"];
    match kind {
        "busy" | "host_shared" => want.push("reason"),
        "not_idle" => want.push("status"),
        "io" => want.push("typed"),
        _ => {}
    }
    want.sort();
    assert_eq!(keys_of(v), want, "{v}");
}
