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

/// A stand-in for an agent: its environment to `agent.env`, then its argv, NUL-separated (so
/// `--`, spaces, quotes and newlines all come back exactly), written whole to `agent.argv`
/// (once that exists, both are complete); then idle.
const FAKE_AGENT: &str = r#"#!/bin/sh
env > "$HOME/agent.env"
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
            .env("FLEET_LOCK_DIR", self.p("locks"))
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
            "workstation": { "ssh": "devbox", "fleetBin": common::fleet_bin() },
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

// A prompt that looks like a flag reaches the agent as its prompt, behind `--` — typed into a
// real shell, quoting and all.
#[test]
fn a_flag_shaped_prompt_arrives_as_the_prompt() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let (out, v) = r.spawn_json(&["--dir", &work, "--name", "fleet-test-e", "--", "--help me"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        v["command"].as_str().unwrap().ends_with(" -- '--help me'"),
        "{v}"
    );
    assert_eq!(r.agent_argv(), ["-n", "fleet-test-e", "--", "--help me"]);
}

/// Spawn `prompt` (after `--`) as `fleet-test-<name>`; the JSON and the agent's argv.
fn spawn_prompt(r: &Rig, name: &str, prompt: &str) -> (Value, Vec<String>) {
    let work = r.s("work");
    let (out, v) = r.spawn_json(&["--dir", &work, "--name", name, "--", prompt]);
    assert!(out.status.success(), "{}", stderr(&out));
    (v, r.agent_argv())
}

// Quotes, `$`, backticks, backslashes, newlines, tabs and non-ASCII survive the shell, typed
// inline and read from a prompt file alike.
#[test]
fn any_prompt_text_arrives_byte_for_byte() {
    let Some(r) = Rig::new() else { return };
    let text = "it's \"quoted\" $HOME `id` \\ back\nline two\tü 🚀\n";
    let (v, argv) = spawn_prompt(&r, "fleet-test-q1", text);
    assert_eq!(v["prompt_file"], Value::Null, "{v}");
    assert_eq!(argv, ["-n", "fleet-test-q1", "--", text]);
    let long = format!("{}{text}", "x".repeat(1000));
    let (v, argv) = spawn_prompt(&r, "fleet-test-q2", &long);
    assert!(v["prompt_file"].is_string(), "{v}");
    assert_eq!(argv, ["-n", "fleet-test-q2", "--", long.as_str()]);
}

// A prompt just under the typed-line limit goes inline, one byte more goes through a file:
// either way it arrives whole, its trailing newline included.
#[test]
fn a_prompt_at_the_typed_line_limit_arrives_whole() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let prompt = |n: usize| format!("{}\n", "x".repeat(n));
    let dry_len = |n: usize| {
        let (_, v) = r.spawn_json(&[
            "-n",
            "--dir",
            &work,
            "--name",
            "fleet-test-c0",
            "--",
            &prompt(n),
        ]);
        v["command"].as_str().unwrap().len()
    };
    // The longest prompt whose launch line still fits in 900 bytes.
    let (mut n, mut over) = (0, 900);
    assert!(
        dry_len(n) <= 900,
        "the temp paths alone fill the typed line"
    );
    while over - n > 1 {
        let mid = (n + over) / 2;
        if dry_len(mid) <= 900 {
            n = mid;
        } else {
            over = mid;
        }
    }
    let (v, argv) = spawn_prompt(&r, "fleet-test-c1", &prompt(n));
    assert_eq!(v["prompt_file"], Value::Null, "{v}");
    assert_eq!(argv, ["-n", "fleet-test-c1", "--", prompt(n).as_str()]);
    let (v, argv) = spawn_prompt(&r, "fleet-test-c2", &prompt(n + 1));
    assert!(v["prompt_file"].is_string(), "{v}");
    assert_eq!(argv, ["-n", "fleet-test-c2", "--", prompt(n + 1).as_str()]);
}

/// The fake agent's environment, as `NAME=value` lines (read after [`Rig::agent_argv`]).
fn agent_env(r: &Rig) -> String {
    std::fs::read_to_string(r.p("home/agent.env")).unwrap()
}

/// `FLEET_TEST_DC` in an environment dump: which `default-command` started the pane.
fn dc_of(env: &str) -> Option<&str> {
    env.lines().find_map(|l| l.strip_prefix("FLEET_TEST_DC="))
}

fn has_var(env: &str, name: &str) -> bool {
    env.lines().any(|l| l.starts_with(&format!("{name}=")))
}

/// What a spawned agent must not inherit, and what it must keep (set in fleet's environment
/// and, below, in the tmux server's).
const MARKERS: [(&str, &str); 5] = [
    ("CLAUDECODE", "1"),
    ("CLAUDE_CODE_SESSION_ID", "parent-session"),
    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
    ("CODEX_THREAD_ID", "parent-thread"),
    (
        "TRACEPARENT",
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
    ),
];
const CONFIG: [(&str, &str); 5] = [
    ("CLAUDE_CODE_OAUTH_TOKEN", "token-for-test"),
    ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ("CLAUDE_CODE_USE_VERTEX", "1"),
    (
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "http://collector.invalid:4318",
    ),
    ("OTEL_EXPORTER_OTLP_HEADERS", "x-team=test"),
];

fn assert_clean(env: &str) {
    for (k, _) in MARKERS {
        assert!(!has_var(env, k), "{k} leaked into the agent");
    }
    for (k, v) in CONFIG {
        assert!(env.contains(&format!("{k}={v}\n")), "{k} lost");
    }
    // Started through the login shell: the profile ran, and what it sets stays.
    for k in [
        "FLEET_TEST_PROFILE",
        "CODEX_CI_FROM_PROFILE",
        "DISABLE_AUTO_UPDATE",
    ] {
        assert!(has_var(env, k), "{k} missing from the agent");
    }
}

// A session spawned from inside an agent must not act as part of it: the agent's own markers
// (`CLAUDECODE`, its session id, `CODEX_THREAD_ID`, `TRACEPARENT`, …) never reach it — not from
// fleet's environment, not from a tmux server whose global environment already holds them.
// Configuration does: auth, provider and OTel exporter settings. The login profile still runs.
#[test]
fn a_spawned_agent_keeps_the_users_config_but_not_the_parent_agents_markers() {
    let Some(r) = Rig::new() else { return };
    std::fs::write(
        r.p("home/.profile"),
        "export FLEET_TEST_PROFILE=1\nexport CODEX_CI_FROM_PROFILE=1\n",
    )
    .unwrap();
    let work = r.s("work");
    let spawn = |extra: &[&str]| {
        let mut c = r.cmd();
        c.envs(MARKERS).envs(CONFIG);
        c.args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
            .args(extra);
        let (out, v) = r.json_of(&mut c);
        assert!(out.status.success(), "{v} / {}", stderr(&out));
        r.agent_argv();
        agent_env(&r)
    };

    // 1. fleet starts the server itself: its markers stay out of the server too.
    assert_clean(&spawn(&["--name", "fleet-test-g"]));
    let global = r.tmux(&["show-environment", "-g"]);
    for (k, _) in MARKERS {
        assert!(!has_var(&global, k), "{k} reached the server");
    }

    // 2. A server whose global environment already carries them (started from an agent).
    for (k, v) in MARKERS {
        r.tmux(&["set-environment", "-g", k, v]);
    }
    assert_clean(&spawn(&["--name", "fleet-test-h"]));
    // A window in an existing session gets the same.
    assert_clean(&spawn(&["--tmux-session", "fleet-test-h"]));
}

// The pane becomes what tmux itself would start: the effective `default-command` — from the
// config a fresh server loads, or a session's own — else the login shell.
#[test]
fn the_spawned_pane_honours_the_effective_default_command() {
    let Some(r) = Rig::new() else { return };
    std::fs::write(
        r.p("home/.tmux.conf"),
        "set -g default-command 'FLEET_TEST_DC=conf exec /bin/sh -l'\n",
    )
    .unwrap();
    let work = r.s("work");
    let spawn = |extra: &[&str]| {
        let mut c = r.cmd();
        c.env("CLAUDECODE", "1")
            .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
            .args(extra);
        let (out, v) = r.json_of(&mut c);
        assert!(out.status.success(), "{v} / {}", stderr(&out));
        r.agent_argv();
        agent_env(&r)
    };
    // No server yet: fleet's spawn starts it, and the config it loads applies.
    let env = spawn(&["--name", "fleet-test-dc"]);
    assert_eq!(
        dc_of(&env),
        Some("conf"),
        "the config's default-command did not run"
    );
    assert!(!has_var(&env, "CLAUDECODE"));
    // A session's own setting wins over the global one.
    r.tmux(&[
        "set-option",
        "-t",
        "fleet-test-dc",
        "default-command",
        "FLEET_TEST_DC=session exec /bin/sh -l",
    ]);
    let env = spawn(&["--tmux-session", "fleet-test-dc"]);
    assert_eq!(
        dc_of(&env),
        Some("session"),
        "the session's default-command did not run"
    );
    assert!(!has_var(&env, "CLAUDECODE"));
}

/// `name`'s value in an environment dump.
fn var_of<'a>(env: &'a str, name: &str) -> Option<&'a str> {
    env.lines()
        .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix('=')))
}

// Under FLEET_PATH_ONLY the pane starts with fleet's own PATH and the switch, passed with tmux
// `-e` — over the tmux server's environment, which a pane otherwise inherits: in a new session,
// and in a window of a session made without them. (`default-command` is a plain `sh` here, so
// no login profile rewrites PATH after the pane starts.)
#[test]
fn a_path_only_spawn_hands_the_pane_fleets_path() {
    let Some(r) = Rig::new() else { return };
    std::fs::write(
        r.p("home/.tmux.conf"),
        "set -g default-command 'exec /bin/sh'\n",
    )
    .unwrap();
    let work = r.s("work");
    let path = format!("{}:/usr/bin:/bin", r.s("bin"));
    let spawn = |path_only: bool, extra: &[&str]| {
        let mut c = r.cmd();
        c.env("PATH", &path);
        if path_only {
            c.env("FLEET_PATH_ONLY", "1");
        }
        c.args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
            .args(extra);
        let (out, v) = r.json_of(&mut c);
        assert!(out.status.success(), "{v} / {}", stderr(&out));
        r.agent_argv();
        agent_env(&r)
    };

    // The server starts without the switch; then its global environment says otherwise.
    spawn(false, &["--name", "fleet-test-p"]);
    r.tmux(&[
        "set-environment",
        "-g",
        "PATH",
        "/fleet-test-stale:/usr/bin:/bin",
    ]);
    r.tmux(&["set-environment", "-g", "FLEET_PATH_ONLY", "0"]);
    // Without the switch a pane gets the server's…
    let env = spawn(false, &["--name", "fleet-test-q"]);
    assert_eq!(var_of(&env, "FLEET_PATH_ONLY"), Some("0"));
    // …with it, fleet's.
    for extra in [
        &["--name", "fleet-test-r"][..],
        &["--tmux-session", "fleet-test-p"],
    ] {
        let env = spawn(true, extra);
        assert_eq!(var_of(&env, "PATH"), Some(path.as_str()), "{extra:?}");
        assert_eq!(var_of(&env, "FLEET_PATH_ONLY"), Some("1"), "{extra:?}");
    }
}

// Under FLEET_PATH_ONLY the pane runs nothing from outside PATH, and needs nothing on it but
// `sh`: tmux starts the `sh` found there (not `/bin/sh`), which scrubs the agent markers itself
// (no `env`) and becomes `$SHELL -l` — the one exception. With no `sh` on PATH the spawn is
// refused before anything opens or is written.
#[test]
fn a_path_only_spawn_needs_only_sh_on_path() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    std::fs::create_dir_all(r.p("only-sh")).unwrap();
    std::fs::create_dir_all(r.p("nothing")).unwrap();
    write_exe(
        &r.p("only-sh/sh"),
        &format!(
            "#!/bin/sh\necho \"sh $*\" >> '{}'\nexec /bin/sh \"$@\"\n",
            r.s("sh.log")
        ),
    );
    // The login shell's profile gives the agent the system tools back.
    std::fs::write(
        r.p("home/.profile"),
        "PATH=\"$PATH:/usr/bin:/bin\"; export PATH\n",
    )
    .unwrap();

    let mut c = r.cmd();
    c.env("PATH", r.p("only-sh"))
        .env("FLEET_PATH_ONLY", "1")
        .env("CLAUDECODE", "1")
        .args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
        .args(["--name", "fleet-test-sh", "go"]);
    let (out, v) = r.json_of(&mut c);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert!(r.agent_argv().ends_with(&["go".to_string()]));
    assert!(!has_var(&agent_env(&r), "CLAUDECODE"));
    let log = std::fs::read_to_string(r.p("sh.log")).unwrap_or_default();
    assert!(log.starts_with("sh -c "), "the pane's sh: {log:?}");

    // No `sh` on PATH: refused (kind `usage`), a dry run included, and nothing is written.
    for dry in [false, true] {
        let mut c = r.cmd();
        c.env("PATH", r.p("nothing")).env("FLEET_PATH_ONLY", "1");
        if dry {
            c.arg("-n");
        }
        c.args(["spawn", "--backend", "tmux", "--json", "--dir", &work])
            .args(["--name", "fleet-test-nosh", "go"]);
        let (out, v) = r.json_of(&mut c);
        assert_eq!(out.status.code(), Some(1), "{v}");
        assert_eq!(v["kind"], "usage", "{v}");
        assert!(v["error"].as_str().unwrap().contains("`sh`"), "{v}");
    }
    let out = r
        .cmd()
        .env("PATH", r.p("nothing"))
        .env("FLEET_PATH_ONLY", "1")
        .args([
            "handoff",
            "take it",
            "--backend",
            "tmux",
            "--no-wait",
            "--dir",
            &work,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("`sh`"), "{}", stderr(&out));
    assert!(!r.p("home/.claude/fleet-handoffs").exists());
    assert!(!r.sessions().contains(&"fleet-test-nosh".to_string()));
}

// --- spawn --resume ---------------------------------------------------------

const RESUME_ID: &str = "0b7e3c1a-5d2f-4e6a-9c8b-1f2e3d4c5b6a";

// Claude resumes as `<launcher> --resume=<id>` (the id bound to its flag), through the plain
// spawn's path: the same object plus `resumed`, and no prompt typed.
#[test]
fn spawn_resume_starts_claudes_own_resume() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let (out, v) = r.spawn_json(&[
        "--resume",
        RESUME_ID,
        "--dir",
        &work,
        "--name",
        "fleet-test-r1",
    ]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["resumed"], RESUME_ID);
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["tmux_session"], "fleet-test-r1");
    assert_eq!(v["created_session"], true);
    assert_eq!(v["dir"], work.as_str());
    assert_eq!(v["prompt_file"], Value::Null);
    assert_eq!(v["from"], Value::Null);
    let command = v["command"].as_str().unwrap();
    assert!(
        command.ends_with(&format!(" --resume={RESUME_ID} -n 'fleet-test-r1'")),
        "{command}"
    );
    let bound = format!("--resume={RESUME_ID}");
    assert_eq!(r.agent_argv(), [bound.as_str(), "-n", "fleet-test-r1"]);
}

// Codex resumes as `codex resume -- <id>` (after `--`, the id is its positional even if it
// read like a flag), under its configured launcher.
#[test]
fn spawn_resume_starts_codexs_own_resume() {
    let Some(r) = Rig::new() else { return };
    r.env.write_config(&serde_json::json!({
        "version": 1,
        "harnesses": { "codex": { "command": r.s("bin/agent") } }
    }));
    let work = r.s("work");
    let (out, v) = r.spawn_json(&[
        "--resume",
        RESUME_ID,
        "--harness",
        "codex",
        "--dir",
        &work,
        "--name",
        "fleet-test-r2",
    ]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(v["resumed"], RESUME_ID);
    assert_eq!(v["harness"], "codex");
    assert_eq!(v["tmux_session"], "fleet-test-r2");
    let command = v["command"].as_str().unwrap();
    assert!(
        command.ends_with(&format!(" resume -- {RESUME_ID}")),
        "{command}"
    );
    assert_eq!(r.agent_argv(), ["resume", "--", RESUME_ID]);
}

#[test]
fn spawn_resume_refusals_carry_a_kind_and_start_nothing() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let fail = |args: &[&str], kind: &str| {
        let (out, v) = r.spawn_json(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {v}");
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["kind"], kind, "{v}");
        assert!(!v["error"].as_str().unwrap().is_empty(), "{v}");
        assert_eq!(v["partial"], Value::Null, "{v}");
    };
    // A first prompt has no place in a resume.
    fail(&["--resume", RESUME_ID, "--dir", &work, "hi"], "usage");
    // The id is typed into a shell: letters, digits and `-` only, 1-128 of them.
    let long = "a".repeat(129);
    for bad in ["", "abc;touch x", "a b", "$(id)", "abc_def", &long] {
        fail(&["--resume", bad, "--dir", &work], "usage");
    }
    // Nor does a session id start with `-`: it would read as the harness's flag.
    for bad in ["--help", "-abc", "-", "--"] {
        fail(&[&format!("--resume={bad}"), "--dir", &work], "usage");
        fail(
            &[
                &format!("--resume={bad}"),
                "--harness",
                "codex",
                "--dir",
                &work,
            ],
            "usage",
        );
    }
    fail(
        &["--resume", RESUME_ID, "--harness", "pi", "--dir", &work],
        "harness_unsupported",
    );
    fail(&["--resume", RESUME_ID], "dir_missing");
    fail(
        &["--resume", RESUME_ID, "--dir", &r.s("nope")],
        "dir_missing",
    );
    assert!(r.sessions().is_empty(), "{:?}", r.sessions());
    // 128 is still an id.
    let (out, v) = r.spawn_json(&["-n", "--resume", &"a".repeat(128), "--dir", &work]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
}

// A session in fleet's current list never gets a second process: `session_live`, with the
// live one's pid. The same id under another harness is not that session.
#[test]
fn spawn_resume_of_a_live_session_names_its_pid() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let fixture = r.p("sessions.json");
    std::fs::write(
        &fixture,
        serde_json::json!([{
            "pid": 4242, "session_id": RESUME_ID, "harness": "claude",
            "cwd": work, "status": "idle", "backend": "tmux"
        }])
        .to_string(),
    )
    .unwrap();
    let resume = |id: &str, extra: &[&str]| {
        let mut c = r.cmd();
        c.env("FLEET_FIXTURE", &fixture)
            .args(["spawn", "--backend", "tmux", "--json", "--resume", id])
            .args(["--dir", &work])
            .args(extra);
        r.json_of(&mut c)
    };
    let (out, v) = resume(RESUME_ID, &[]);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "session_live", "{v}");
    assert_eq!(v["pid"], 4242, "{v}");
    // Matched like an `id:<session_id>` target: whole, case-insensitive.
    let (_, v) = resume(&RESUME_ID.to_uppercase(), &[]);
    assert_eq!(v["kind"], "session_live", "{v}");
    assert!(r.sessions().is_empty(), "{:?}", r.sessions());

    let (out, v) = resume(RESUME_ID, &["-n", "--harness", "codex"]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["resumed"], RESUME_ID);
}

// Parse failures clap catches are `spawn --json`'s failure object too: exit 1, kind `usage` —
// a missing id, or a hyphen-prefixed one given apart (clap reads it as a flag). No tmux needed.
#[test]
fn spawn_resume_parse_failures_are_one_usage_object() {
    let env = Env::new();
    for args in [
        &["spawn", "--json", "--resume"][..],
        &["spawn", "--resume", "--json"],
        &["spawn", "--json", "--resume", "--bad"],
        &["spawn", "--json", "--resume", "-x", "--dir", "."],
        // `--json` after what clap cannot read counts the same as before it.
        &["--local", "spawn", "--resume", "--bad", "--json"],
        &["--local", "spawn", "--json", "--resume", "--bad"],
        &["--local", "spawn", "--bad=1", "-Z", "--resume", "--json"],
        // A flag given twice, however it is spelled.
        &["spawn", "--name", "a", "--name", "b", "--json"],
        &["spawn", "--json", "--name=a", "--name", "b"],
    ] {
        let out = env.cmd().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let v: Value = serde_json::from_str(stdout(&out).trim())
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", stdout(&out)));
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["kind"], "usage", "{v}");
        assert!(!v["error"].as_str().unwrap().is_empty(), "{v}");
        assert_eq!(v["partial"], Value::Null, "{v}");
    }
    // Without --json it stays clap's: exit 2, nothing on stdout — and a `--json` that is an
    // option's value, or after `--`, is not one.
    for args in [
        &["spawn", "--resume"][..],
        &["--local", "spawn", "--resume", "--bad", "--", "--json"],
        &[
            "--local",
            "spawn",
            "--bad",
            "--resume",
            "x",
            "--name=--json",
        ],
    ] {
        let out = env.cmd().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stdout(&out).is_empty(), "{args:?}: {}", stdout(&out));
    }
}

/// The reservation stamp of a `--resume` of `id` under `harness`.
fn reservation(r: &Rig, harness: &str, id: &str) -> PathBuf {
    r.p(&format!(
        "locks/resume-{harness}-{}.stamp",
        id.to_lowercase()
    ))
}

/// The Claude reservation stamp of `RESUME_ID`, read.
fn stamp(r: &Rig) -> Value {
    let raw = std::fs::read(reservation(r, "claude", RESUME_ID)).unwrap();
    serde_json::from_slice(&raw).unwrap()
}

/// Age that stamp past the 30 s hold, as if its resume started 31 s ago.
fn age_stamp(r: &Rig) {
    let mut s = stamp(r);
    s["at"] = (s["at"].as_u64().unwrap() - 31_000).into();
    std::fs::write(reservation(r, "claude", RESUME_ID), s.to_string()).unwrap();
}

// Several resumes of one session at once start it once: the rest are `session_starting`.
#[test]
fn concurrent_resumes_start_one_process() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let results: Vec<(Option<i32>, Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let (out, v) = r.spawn_json(&["--resume", RESUME_ID, "--dir", &work]);
                    (out.status.code(), v)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let started: Vec<_> = results.iter().filter(|(_, v)| v["ok"] == true).collect();
    assert_eq!(started.len(), 1, "{results:?}");
    for (code, v) in results.iter().filter(|(_, v)| v["ok"] != true) {
        assert_eq!(*code, Some(1), "{v}");
        assert_eq!(v["kind"], "session_starting", "{v}");
        assert_eq!(v["partial"], Value::Null, "{v}");
        assert!(v.get("pid").is_none(), "{v}");
    }
    assert_eq!(r.sessions().len(), 1, "{:?}", r.sessions());
    let bound = format!("--resume={RESUME_ID}");
    assert_eq!(r.agent_argv(), [bound.as_str()]);
}

/// Register `pid` in the rig's Claude registry as session `RESUME_ID` (what Claude does once
/// it is up). Returns the entry's path.
fn register(r: &Rig, pid: i64, cwd: &str) -> PathBuf {
    let sessions = r.p("home/.claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let entry = sessions.join(format!("{pid}.json"));
    std::fs::write(
        &entry,
        serde_json::json!({
            "pid": pid, "sessionId": RESUME_ID, "cwd": cwd, "status": "idle",
            "kind": "interactive", "entrypoint": "cli"
        })
        .to_string(),
    )
    .unwrap();
    entry
}

fn pane_pid(r: &Rig, session: &str) -> i64 {
    r.tmux(&["display-message", "-p", "-t", session, "#{pane_pid}"])
        .trim()
        .parse()
        .unwrap()
}

// A resume holds its session until it registers (then `session_live`), or for 30 s: a retry
// meanwhile never starts a second process. Another harness's session of that id is not held.
#[test]
fn a_resume_holds_the_session_until_it_registers() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let resume = |extra: &[&str]| {
        let mut args = vec!["--resume", RESUME_ID, "--dir", work.as_str()];
        args.extend_from_slice(extra);
        r.spawn_json(&args)
    };
    let (out, v) = resume(&["--name", "fleet-test-h1"]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    r.agent_argv();

    // Not registered yet: the retry (in any case) is refused, and a dry run says the same.
    let up = RESUME_ID.to_uppercase();
    for dry in [false, true] {
        let mut c = r.cmd();
        if dry {
            c.arg("-n");
        }
        c.args(["spawn", "--backend", "tmux", "--json", "--resume", &up])
            .args(["--dir", &work]);
        let (out, v) = r.json_of(&mut c);
        assert_eq!(out.status.code(), Some(1), "{v}");
        assert_eq!(v["kind"], "session_starting", "{v}");
    }
    assert_eq!(r.sessions(), ["fleet-test-h1"]);
    // Codex's session of the same id is another session.
    let (out, v) = resume(&["-n", "--harness", "codex"]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));

    // It registers: now it is live, with its pid.
    let pid = pane_pid(&r, "fleet-test-h1");
    let entry = register(&r, pid, &work);
    let (out, v) = resume(&[]);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["kind"], "session_live", "{v}");
    assert_eq!(v["pid"], pid, "{v}");

    // Gone again — its terminal closed — and the hold has run out (30 s, aged here): a
    // resume starts it anew.
    std::fs::remove_file(&entry).unwrap();
    age_stamp(&r);
    r.tmux(&["kill-session", "-t", "=fleet-test-h1"]);
    let (out, v) = resume(&["--name", "fleet-test-h2"]);
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(r.sessions(), ["fleet-test-h2"]);
}

// The reservation is tied to the tmux pane the resume was typed into. A resume that never
// registers (Claude waiting at its trust prompt) holds the session past the 30 s for as long
// as that pane runs it; once the pane is gone, or its shell is back at the prompt, it is stale.
#[test]
fn a_resume_stays_reserved_while_its_host_runs_it() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let resume =
        |name: &str| r.spawn_json(&["--resume", RESUME_ID, "--dir", &work, "--name", name]);
    let refused = || {
        let (out, v) = r.spawn_json(&["--resume", RESUME_ID, "--dir", &work]);
        assert_eq!(out.status.code(), Some(1), "{v}");
        assert_eq!(v["kind"], "session_starting", "{v}");
    };
    let (out, v) = resume("fleet-test-t1");
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    r.agent_argv();
    let s = stamp(&r);
    assert_eq!(s["pane"], v["handle"], "{s} / {v}");
    assert_eq!(s["shell"], pane_pid(&r, "fleet-test-t1"), "{s}");

    // Long past 30 s, never registered, its pane still running it: still starting.
    age_stamp(&r);
    refused();
    assert_eq!(r.sessions(), ["fleet-test-t1"]);

    // Its host is gone: stale, and the next resume starts it.
    r.tmux(&["kill-session", "-t", "=fleet-test-t1"]);
    let (out, v) = resume("fleet-test-t2");
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    r.agent_argv();

    // The pane outlives what was typed into it (the resume exited): stale too.
    age_stamp(&r);
    refused();
    let shell = pane_pid(&r, "fleet-test-t2");
    let _ = std::process::Command::new("pkill")
        .args(["-TERM", "-P", &shell.to_string()])
        .output();
    let idle = || {
        std::process::Command::new("pgrep")
            .args(["-P", &shell.to_string()])
            .output()
            .is_ok_and(|o| o.stdout.is_empty())
    };
    r.wait_for("the agent to exit", idle);
    let (out, v) = resume("fleet-test-t3");
    assert!(out.status.success(), "{v} / {}", stderr(&out));
    assert_eq!(r.sessions().len(), 2, "{:?}", r.sessions());
}

// The stamp is down before anything is typed: a start that fails while typing (or a fleet
// killed there) still holds the session, while one that never got a terminal frees it; and a
// stamp that cannot be written stops the resume before it creates anything.
#[test]
fn the_reservation_is_written_before_the_resume_is_typed() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let resume = |fail: Option<&str>, name: &str| {
        let mut c = r.cmd();
        if let Some(f) = fail {
            c.env("FLEET_TEST_FAIL", f);
        }
        c.args([
            "spawn",
            "--backend",
            "tmux",
            "--json",
            "--resume",
            RESUME_ID,
        ])
        .args(["--dir", &work, "--name", name]);
        r.json_of(&mut c)
    };

    // No terminal: nothing started, the session is free at once.
    let (out, v) = resume(Some("new-session"), "fleet-test-w0");
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["kind"], "backend_failed", "{v}");
    assert!(!reservation(&r, "claude", RESUME_ID).exists());

    // Typing failed in an existing terminal: the stamp names it already, and holds.
    let (out, v) = resume(Some("send-keys"), "fleet-test-w1");
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["kind"], "backend_failed", "{v}");
    assert_eq!(stamp(&r)["pane"], v["partial"]["handle"], "{v}");
    let (out, v) = resume(None, "fleet-test-w2");
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["kind"], "session_starting", "{v}");
    r.tmux(&["kill-server"]);

    // A stamp that cannot be written: refused before any terminal exists.
    let stamp_path = reservation(&r, "claude", RESUME_ID);
    std::fs::remove_file(&stamp_path).unwrap();
    std::fs::create_dir_all(stamp_path.join("blocked")).unwrap();
    let (out, v) = resume(None, "fleet-test-w3");
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["partial"], Value::Null, "{v}");
    assert!(r.sessions().is_empty(), "{:?}", r.sessions());
}

/// A stand-in Codex session on the rig's private server: a process named `codex` in `cwd`
/// (tmux session `name`) and its rollout under the temp home. Returns its pid.
fn fake_codex(r: &Rig, name: &str, cwd: &Path, id: &str) -> i64 {
    let cwd = std::fs::canonicalize(cwd).unwrap();
    let script = format!(
        "cd '{}' && exec /bin/bash -c 'exec -a codex sleep 300'",
        cwd.display()
    );
    r.tmux(&[
        "new-session",
        "-d",
        "-s",
        name,
        "--",
        "/bin/sh",
        "-c",
        &script,
    ]);
    let pid = pane_pid(r, name);
    let day = r.p("home/.codex/sessions/2026/10/16");
    std::fs::create_dir_all(&day).unwrap();
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    let now = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let meta = serde_json::json!({ "type": "session_meta",
        "payload": { "id": id, "cwd": cwd, "timestamp": now } });
    std::fs::write(
        day.join(format!("rollout-2026-10-16T10-00-00-{id}.jsonl")),
        format!("{meta}\n"),
    )
    .unwrap();
    pid
}

// Liveness is per harness, through real discovery (no fixture): a stale Claude entry of the
// id does not hide a live Codex session of it, a live Claude one is not the Codex one, and
// the other way round.
#[test]
fn liveness_is_per_harness_in_real_discovery() {
    let Some(r) = Rig::new() else { return };
    let work = r.s("work");
    let resume = |harness: &str| {
        r.spawn_json(&[
            "-n",
            "--resume",
            RESUME_ID,
            "--harness",
            harness,
            "--dir",
            &work,
        ])
    };
    // A stale Claude entry: its pid is gone.
    let dead = {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        pid
    };
    register(&r, dead, &work);
    let codex_dir = r.p("codex-work");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let codex = fake_codex(&r, "fleet-test-cx", &codex_dir, RESUME_ID);

    let live_pid = |harness: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (out, v) = resume(harness);
            if v["kind"] == "session_live" || Instant::now() > deadline {
                assert_eq!(out.status.code(), Some(1), "{harness}: {v}");
                return v["pid"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("{harness}: {v}"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    assert_eq!(live_pid("codex"), codex, "the stale Claude entry hid Codex");
    let (out, v) = resume("claude");
    assert!(
        out.status.success(),
        "a stale entry or Codex counted as Claude: {v}"
    );

    // A live Claude session of the id: Claude's resume names it, Codex's still names Codex.
    r.tmux(&[
        "new-session",
        "-d",
        "-s",
        "fleet-test-cl",
        "--",
        "sleep",
        "300",
    ]);
    let claude = pane_pid(&r, "fleet-test-cl");
    register(&r, claude, &work);
    assert_eq!(live_pid("claude"), claude);
    assert_eq!(live_pid("codex"), codex);
}
