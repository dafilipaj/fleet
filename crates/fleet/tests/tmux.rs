//! `fleet tmux …` against a private tmux server (`common::Env`: `tmux -L fleet-test-<rand>`
//! under a private TMUX_TMPDIR), so the user's own sessions are never visible, let alone
//! touched. The Env kills that server when it drops.

mod common;

use common::{Env, stderr, stdout};

struct Server {
    env: Env,
}

impl Server {
    /// `None` when tmux isn't installed — the tests then pass vacuously.
    fn new() -> Option<Self> {
        fleet::core::tools::find_binary("tmux")?;
        Some(Server { env: Env::new() })
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut c = self.env.cmd();
        c.timeout(std::time::Duration::from_secs(20));
        c
    }

    fn fleet(&self, args: &[&str]) -> std::process::Output {
        self.cmd().args(args).output().unwrap()
    }
}

#[test]
fn sessions_lifecycle() {
    let Some(s) = Server::new() else { return };

    let out = s.fleet(&["tmux", "list"]);
    assert!(out.status.success());
    assert!(
        stderr(&out).contains("no tmux sessions"),
        "{}",
        stderr(&out)
    );

    let out = s.fleet(&["new", "fleet test alpha!", "-d"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("created (detached): fleet-test-alpha"),
        "{}",
        stdout(&out)
    );
    let out = s.fleet(&["tmux", "new", "-d", "fleet-test-beta", "--", "sleep", "300"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Creating it again is a note, not a failure.
    let out = s.fleet(&["tmux", "new", "-d", "fleet-test-beta"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("already exists"));

    let out = s.fleet(&["tmux", "ls", "-q"]);
    let names = stdout(&out);
    assert!(names.lines().any(|l| l == "fleet-test-alpha"), "{names}");
    assert!(names.lines().any(|l| l == "fleet-test-beta"), "{names}");

    let out = s.fleet(&["t", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let alpha = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "fleet-test-alpha")
        .unwrap();
    assert_eq!(alpha["attached"], 0);
    assert_eq!(alpha["windows"], 1);
    assert_eq!(alpha["current"], false);
    assert_eq!(alpha["host"], "local");

    // Ambiguous → 2, no match → 3, no terminal → 3.
    let out = s.fleet(&["tmux", "kill", "fleet-test", "-f"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("matches 2 sessions"));
    let out = s.fleet(&["tmux", "kill", "zzz-nothing", "-f"]);
    assert_eq!(out.status.code(), Some(3));
    let out = s.fleet(&["enter", "alpha"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
    // Killing without -f needs a terminal to confirm on — refused, not assumed.
    let out = s.fleet(&["tmux", "kill", "alpha"]);
    assert!(!out.status.success());

    // Rename: collision refused, then a real one.
    let out = s.fleet(&["tmux", "rename", "alpha", "fleet-test-beta"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    let out = s.fleet(&["tmux", "rename", "alpha", "fleet test gamma"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("fleet-test-alpha → fleet-test-gamma"));

    // Dry-run prints the tmux command and leaves the session alone.
    let out = s.fleet(&["-n", "tmux", "kill", "gamma", "-f"]);
    assert!(
        stdout(&out).contains("kill-session -t '=fleet-test-gamma'"),
        "{}",
        stdout(&out)
    );

    let out = s.fleet(&["tmux", "kill", "gamma", "-f"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "killed: fleet-test-gamma");
    let out = s.fleet(&["tmux", "list", "-q"]);
    assert_eq!(stdout(&out).trim(), "fleet-test-beta");
}

#[test]
fn stale_lists_idle_shells_and_keeps_busy_ones() {
    let Some(s) = Server::new() else { return };
    assert!(s.fleet(&["new", "-d", "fleet-test-idle"]).status.success());
    assert!(
        s.fleet(&["new", "-d", "fleet-test-busy", "--", "sleep", "300"])
            .status
            .success()
    );
    // Give the shell a moment to start.
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = s.fleet(&["tmux", "stale", "--older-than", "0", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["claude_checked"], true,
        "an empty registry is a clean check: {v}"
    );
    let cands: Vec<&str> = v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(cands, ["fleet-test-idle"], "{v}");
    let busy = v["kept"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "fleet-test-busy")
        .unwrap();
    assert_eq!(busy["class"], "running");

    let out = s.fleet(&["tmux", "stale"]);
    assert!(
        stdout(&out).contains("no stale sessions"),
        "24h threshold: {}",
        stdout(&out)
    );
    let out = s.fleet(&["tmux", "stale", "--older-than", "0", "-q"]);
    assert_eq!(stdout(&out).trim(), "fleet-test-idle");

    // --no-fleet-check with -f is refused outright.
    let out = s.fleet(&["tmux", "stale", "--kill", "-f", "--no-fleet-check"]);
    assert_eq!(out.status.code(), Some(1));
    let out = s.fleet(&["tmux", "stale", "--older-than", "5w"]);
    assert_eq!(out.status.code(), Some(1));

    let out = s.fleet(&["tmux", "stale", "--older-than", "0", "--kill", "-f"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("killed: fleet-test-idle"),
        "{}",
        stdout(&out)
    );
    let out = s.fleet(&["tmux", "list", "-q"]);
    assert_eq!(stdout(&out).trim(), "fleet-test-busy");
}

#[test]
fn enter_falls_back_to_other_hosts_when_the_default_has_no_match() {
    let Some(s) = Server::new() else { return };
    // defaultHost = workstation, which cannot be reached; this machine = laptop.
    let mut cfg = common::two_hosts();
    cfg["hosts"]["workstation"]["ssh"] = serde_json::json!("fleet-test-host.invalid");
    s.env.write_config(&cfg);
    let out = s.fleet(&["--local", "tmux", "new", "-d", "fleet-test-local"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // Found here → a note, then the (dry-run) attach on this machine.
    let out = s.fleet(&["-n", "enter", "test-loc"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("workstation:"),
        "unreachable host warned: {err}"
    );
    assert!(err.contains("→ laptop: fleet-test-local"), "{err}");
    assert!(
        stdout(&out).contains("attach -t '=fleet-test-local'"),
        "{}",
        stdout(&out)
    );

    // Nothing anywhere → 3, naming the hosts that answered.
    let out = s.fleet(&["-n", "enter", "zzz-nothing"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no session matching 'zzz-nothing' on laptop"),
        "{}",
        stderr(&out)
    );

    // A named host means no fallback: straight to that host.
    let out = s.fleet(&["-n", "--local", "enter", "zzz-nothing"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!stderr(&out).contains("workstation"), "{}", stderr(&out));
}
