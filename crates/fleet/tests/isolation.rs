//! Guard: a `fleet` run by the tests never reaches the user's tmux server or home.
//!
//! The test listens on the *default* socket path inside the private `TMUX_TMPDIR` the tests use,
//! then runs commands that talk to tmux. A command that ignored `FLEET_TMUX` (and so used the
//! default socket name) would connect to that listener; one that ignored `TMUX_TMPDIR` would
//! never be seen here — but every subprocess gets it from `common::Env`, which this pins too.

mod common;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;

use common::{Env, stderr};

#[test]
fn tests_reach_neither_the_default_socket_nor_the_real_home() {
    if fleet::core::tools::find_binary("tmux").is_none() {
        return;
    }
    let env = Env::new();
    let uid = std::fs::metadata(env.sockets.path()).unwrap().uid();
    let tmux_dir = env.sockets.path().join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&tmux_dir).unwrap();
    std::fs::set_permissions(&tmux_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let trap = UnixListener::bind(tmux_dir.join("default")).unwrap();
    trap.set_nonblocking(true).unwrap();

    let run = |args: &[&str]| {
        env.cmd()
            .args(args)
            .timeout(std::time::Duration::from_secs(10))
            .output()
            .unwrap()
    };
    // Discovery (list-panes), tmux session verbs, a server start, a target lookup.
    assert!(run(&["list", "--json"]).status.success());
    let out = run(&["tmux", "new", "-d", "fleet-test-guard", "--", "sleep", "60"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(run(&["tmux", "list", "--json"]).status.success());
    let _ = run(&["peek", "pid:1"]);
    let out = run(&["tmux", "kill", "fleet-test-guard", "-f"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // The session went to the private `-L` server…
    assert!(
        tmux_dir.join(&env.socket).exists(),
        "private socket missing"
    );
    // …and nothing ever knocked on the default one.
    match trap.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("a test reached the default tmux socket: {other:?}"),
    }

    // HOME is the temp one: no real Claude registry is read.
    let out = run(&["list", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows, serde_json::json!([]), "the real registry leaked in");
}

// Every `fleet` a test runs comes from `common::Env` (`cmd` / `std_cmd`), which is what
// isolates it. A test that builds the binary's command itself inherits the developer's HOME,
// TMUX and FLEET_TMUX — and a bare `fleet` then lists the live fleet on the live server.
#[test]
fn no_test_runs_the_fleet_binary_around_env() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("isolation.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("cargo_bin(") || code.contains("CARGO_BIN_EXE") {
                offenders.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "run `fleet` through common::Env::cmd / std_cmd: {offenders:?}"
    );
}
