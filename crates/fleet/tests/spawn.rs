//! `fleet spawn` end to end, against an *isolated* tmux server: `FLEET_TMUX` is a wrapper that
//! runs `tmux -L fleet-test-<rand>` (with its own `TMUX_TMPDIR` too), the harness is a fake
//! script that records its argv and idles, `ssh` is a fake that runs the remote command here,
//! and HOME and the config live in a temp dir. The user's own tmux server and sessions are
//! never visible, let alone touched.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Env, stderr, stdout};
use serde_json::Value;

/// A stand-in for an agent: its argv, NUL-separated (so `--`, spaces, quotes and newlines all
/// come back exactly), written whole to `agent.argv`; then idle.
const FAKE_AGENT: &str = r#"#!/bin/sh
printf '%s\0' "$@" > "$HOME/agent.argv.tmp" && mv "$HOME/agent.argv.tmp" "$HOME/agent.argv"
trap 'exit 0' HUP TERM INT
while :; do sleep 1; done
"#;

/// A stand-in for `ssh`: destination `down` cannot be reached (255); any other runs the remote
/// command right here, as the real one would run it there.
const FAKE_SSH: &str = r#"#!/bin/sh
prev=""
for a in "$@"; do dest="$prev"; prev="$a"; done
if [ "$dest" = down ]; then echo "ssh: connect to host down: Connection refused" >&2; exit 255; fi
exec /bin/sh -c "$prev"
"#;

struct Rig {
    env: Env,
    /// `TMUX_TMPDIR`: short (a socket path has a ~104-byte limit) and private.
    sockets: tempfile::TempDir,
}

fn write_exe(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    /// `None` when tmux isn't installed: the test says so and passes without checking anything.
    fn new() -> Option<Self> {
        let Some(tmux) = fleet::core::tools::find_binary("tmux") else {
            eprintln!("SKIPPED: tmux is not installed, so this spawn test checks nothing");
            return None;
        };
        let env = Env::new();
        let sockets = tempfile::Builder::new()
            .prefix("ft")
            .tempdir_in("/tmp")
            .ok()?;
        for d in ["home", "bin", "work"] {
            std::fs::create_dir_all(env.path(d)).unwrap();
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let socket = format!("fleet-test-{nanos:x}");
        // `FLEET_TEST_FAIL=<tmux command>` makes that command fail, as tmux would.
        write_exe(
            &env.path("bin/tmux"),
            &format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do\n\
                 if [ -n \"$FLEET_TEST_FAIL\" ] && [ \"$a\" = \"$FLEET_TEST_FAIL\" ]; then\n\
                 echo \"forced failure of $a\" >&2; exit 1; fi\n\
                 done\n\
                 exec '{}' -L '{socket}' \"$@\"\n",
                tmux.display()
            ),
        );
        write_exe(&env.path("bin/agent"), FAKE_AGENT);
        write_exe(&env.path("bin/ssh"), FAKE_SSH);
        Some(Rig { env, sockets })
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.env.path(rel)
    }

    fn s(&self, rel: &str) -> String {
        self.p(rel).display().to_string()
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut c = self.env.cmd();
        let path = format!(
            "{}:{}",
            self.s("bin"),
            std::env::var("PATH").unwrap_or_default()
        );
        c.env("FLEET_TMUX", self.p("bin/tmux"))
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("HOME", self.p("home"))
            .env("XDG_STATE_HOME", self.p("home/state"))
            .env("FLEET_CMD", self.p("bin/agent"))
            .env("SHELL", "/bin/sh")
            .env("PATH", path)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("FLEET_FIXTURE")
            .env_remove("FLEET_JOB_RUN")
            .env_remove("FLEET_TEST_FAIL")
            .timeout(Duration::from_secs(30));
        c
    }

    fn json_of(&self, c: &mut assert_cmd::Command) -> (std::process::Output, Value) {
        let out = c.output().unwrap();
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {} / {}", stdout(&out), stderr(&out)));
        (out, v)
    }

    fn spawn_json(&self, args: &[&str]) -> (std::process::Output, Value) {
        let mut c = self.cmd();
        c.args(["spawn", "--backend", "tmux", "--json"]).args(args);
        self.json_of(&mut c)
    }

    /// The isolated server, directly (through the same wrapper).
    fn tmux(&self, args: &[&str]) -> String {
        let out = std::process::Command::new(self.p("bin/tmux"))
            .args(args)
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("HOME", self.p("home"))
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("FLEET_TEST_FAIL")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn sessions(&self) -> Vec<String> {
        self.tmux(&["list-sessions", "-F", "#{session_name}"])
            .lines()
            .map(String::from)
            .collect()
    }

    /// The argv the fake agent started with (waits for it), consumed so the next spawn's is
    /// read fresh.
    fn agent_argv(&self) -> Vec<String> {
        let f = self.p("home/agent.argv");
        self.wait_for("the agent to start", || f.exists());
        let raw = std::fs::read(&f).unwrap();
        std::fs::remove_file(&f).unwrap();
        let mut args: Vec<String> = raw
            .split(|b| *b == 0)
            .map(|a| String::from_utf8(a.to_vec()).unwrap())
            .collect();
        args.pop(); // after the last NUL
        args
    }

    fn wait_for(&self, what: &str, ok: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.tmux(&["kill-server"]);
        // Belt and braces: any fake still alive is ours — matched by this rig's own path.
        let _ = std::process::Command::new("pkill")
            .args(["-f", &self.s("bin/agent")])
            .output();
    }
}

#[test]
fn spawn_json_says_where_the_session_landed() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let (out, v) = r.spawn_json(&["--dir", &work, "--name", "fleet-test-a", "say hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["host"], "local");
    assert_eq!(v["backend"], "tmux");
    assert_eq!(v["tmux_session"], "fleet-test-a");
    assert_eq!(v["tmux_window"], "fleet-test-a:0");
    assert_eq!(v["created_session"], true);
    assert_eq!(v["dir"], work.as_str());
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["model"], Value::Null);
    assert_eq!(v["name"], "fleet-test-a");
    assert_eq!(v["prompt_file"], Value::Null);
    // The handle is the pane `list --json` will report for the session.
    let pane = v["handle"].as_str().unwrap().to_string();
    assert!(pane.starts_with('%'), "{pane}");
    let panes = r.tmux(&["list-panes", "-t", "=fleet-test-a:", "-F", "#{pane_id}"]);
    assert_eq!(panes.trim(), pane);
    assert!(v["command"].as_str().unwrap().contains("'say hi'"), "{v}");
    assert!(r.agent_argv().ends_with(&["say hi".to_string()]));

    // A window in an existing session: `created_session` false, a pane of its own.
    let (out, v) = r.spawn_json(&["--dir", &work, "--tmux-session", "fleet-test-a"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(v["tmux_session"], "fleet-test-a");
    assert_eq!(v["created_session"], false);
    assert_ne!(v["handle"].as_str().unwrap(), pane);
}

#[test]
fn spawn_json_failures_carry_a_kind_and_exit_non_zero() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let fail = |args: &[&str], kind: &str| {
        let (out, v) = r.spawn_json(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {v}");
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["kind"], kind, "{v}");
        assert!(!v["error"].as_str().unwrap().is_empty(), "{v}");
        assert_eq!(v["partial"], Value::Null, "{v}");
        // People still get the reason on stderr.
        assert!(stderr(&out).contains("Error:"), "{}", stderr(&out));
    };
    fail(&["--dir", &r.s("nope")], "dir_missing");
    fail(&["--dir", &work, "--model", "a b"], "model_invalid");
    fail(&["--dir", &work, "--name", "::"], "name_invalid");

    let (out, _) = r.spawn_json(&["--dir", &work, "--name", "fleet-test-b"]);
    assert!(out.status.success(), "{}", stderr(&out));
    fail(&["--dir", &work, "--name", "fleet-test-b"], "name_taken");

    // A launcher that cannot start never reaches tmux.
    let mut c = r.cmd();
    c.env("FLEET_CMD", r.p("bin/no-such-agent"))
        .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
        .args(["--name", "fleet-test-c"]);
    let (out, v) = r.json_of(&mut c);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["kind"], "harness_missing", "{v}");
    assert!(!r.sessions().contains(&"fleet-test-c".to_string()));
}

// The terminal was made, typing into it failed: the failure still says where the terminal is,
// so the caller can find and close it.
#[test]
fn a_failure_after_the_terminal_exists_reports_where_it_is() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let mut c = r.cmd();
    c.env("FLEET_TEST_FAIL", "send-keys")
        .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
        .args(["--name", "fleet-test-p", "hi"]);
    let (out, v) = r.json_of(&mut c);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "backend_failed", "{v}");
    let p = &v["partial"];
    assert_eq!(p["backend"], "tmux", "{v}");
    assert_eq!(p["tmux_session"], "fleet-test-p", "{v}");
    assert_eq!(p["tmux_window"], "fleet-test-p:0", "{v}");
    assert_eq!(p["created_session"], true, "{v}");
    let pane = p["handle"].as_str().unwrap();
    assert_eq!(
        r.tmux(&["list-panes", "-t", "=fleet-test-p:", "-F", "#{pane_id}"])
            .trim(),
        pane
    );
}

#[test]
fn spawn_json_dry_run_prints_the_command_and_starts_nothing() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let (out, v) = r.spawn_json(&["-n", "--dir", &work, "--name", "fleet-test-d", "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["tmux_session"], Value::Null);
    assert_eq!(v["handle"], Value::Null);
    assert!(v["command"].as_str().unwrap().contains("'hi'"), "{v}");
    assert!(r.sessions().is_empty());
}

// Inside a job run a spawn always lands in tmux (the run is recorded there): the JSON names
// the backend that was used, not the one asked for — dry run included.
#[test]
fn a_job_run_spawn_reports_the_backend_it_really_used() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    for dry in [true, false] {
        let mut c = r.cmd();
        c.env("FLEET_JOB_RUN", "nightly/r1");
        if dry {
            c.arg("-n");
        }
        c.args(["spawn", "--backend", "iterm", "--json", "--dir", &work])
            .args(["--name", "fleet-test-j"]);
        let (out, v) = r.json_of(&mut c);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(v["dry_run"], dry, "{v}");
        assert_eq!(v["backend"], "tmux", "{v}");
    }
    assert!(r.sessions().contains(&"fleet-test-j".to_string()));
}

// `FLEET_CMD=./agent` means the agent in the spawn's directory: that is where the shell runs it.
#[test]
fn a_relative_launcher_is_judged_from_the_spawn_dir() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    std::fs::copy(r.p("bin/agent"), r.p("work/agent")).unwrap();
    let mut c = r.cmd();
    c.current_dir(r.p("home"))
        .env("FLEET_CMD", "./agent")
        .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
        .args(["--name", "fleet-test-r", "go"]);
    let (out, v) = r.json_of(&mut c);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert!(r.agent_argv().ends_with(&["go".to_string()]));
    // From a dir without it, it is missing.
    let mut c = r.cmd();
    c.current_dir(r.p("work"))
        .env("FLEET_CMD", "./agent")
        .args([
            "spawn",
            "--backend",
            "tmux",
            "--json",
            "--dir",
            &r.s("home"),
        ])
        .args(["--name", "fleet-test-r2"]);
    let (out, v) = r.json_of(&mut c);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["kind"], "harness_missing", "{v}");
}

/// A config whose `workstation` is reached through the fake `ssh` (this same machine, this
/// same fleet binary) and whose `down` cannot be reached at all.
fn remote_config(r: &Rig) {
    r.env.write_config(&serde_json::json!({
        "version": 1,
        "self": "laptop",
        "hosts": {
            "laptop": { "ssh": null },
            "workstation": { "ssh": "devbox", "fleetBin": assert_cmd::cargo::cargo_bin("fleet") },
            "down": { "ssh": "down" }
        }
    }));
}

// `-H <host> spawn --json` answers with exactly one object too: the host's own answer, or why
// it could not be had.
#[test]
fn spawn_json_on_another_host_is_still_one_object() {
    let Some(r) = Rig::new() else { return };
    remote_config(&r);
    let work = r.s("work");
    let remote = |global: &[&str], extra: &[&str]| {
        let mut c = r.cmd();
        c.args(global)
            .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
            .args(extra);
        r.json_of(&mut c)
    };

    // Reached: the host's own answer, named after the host as this machine knows it.
    let (out, v) = remote(&["-H", "workstation"], &["--name", "fleet-test-h1"]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["host"], "workstation", "{v}");
    assert_eq!(v["tmux_session"], "fleet-test-h1", "{v}");
    // ... including its failures.
    let (out, v) = remote(&["-H", "workstation"], &["--name", "fleet-test-h1"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["kind"], "name_taken", "{v}");

    // Not reached.
    let (out, v) = remote(&["-H", "down"], &[]);
    assert_eq!(out.status.code(), Some(4), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "host_unreachable", "{v}");
    assert_eq!(v["host"], "down");

    // Dry run: nothing contacted; the command that would run.
    let (out, v) = remote(&["-n", "-H", "workstation"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["remote"], true, "{v}");
    assert!(v["command"].as_str().unwrap().contains("spawn"), "{v}");

    // No `ssh` to run at all: still one object, and a non-zero exit.
    std::fs::create_dir_all(r.p("empty")).unwrap();
    let mut c = r.cmd();
    c.env("PATH", r.p("empty"))
        .args(["-H", "workstation", "spawn", "--json", "--dir", &work]);
    let (out, v) = r.json_of(&mut c);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "ssh_failed", "{v}");
    assert_eq!(v["host"], "workstation");

    // A --dir that cannot travel to another machine.
    let mut c = r.cmd();
    c.args(["-H", "workstation", "spawn", "--json", "--dir", "rel/dir"]);
    let (out, v) = r.json_of(&mut c);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["kind"], "dir_invalid", "{v}");
}
