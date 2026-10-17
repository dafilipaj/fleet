//! `fleet close` against fake agents on the test's private tmux server. Each agent reads its
//! raw tty and logs every read, so a test sees what was typed, in what order; what it does
//! on `/exit` (exit, ignore it, or split its own window) or on Escape (exit) is up to the
//! test. A test can also put a tmux wrapper of its own between fleet and the private server,
//! to change the tmux session at the very moment fleet kills it.

mod common;

use std::path::PathBuf;

use common::fake::{FakeFleet, proc_start};
use common::stderr;
use serde_json::Value;

/// The agent: logs each read as `<unix seconds> <hex bytes>`; on Escape it marks its registry
/// entry idle (a turn interrupted), and on `/exit` + Enter it does what `mode` says:
/// `exits`, `ignores`, or `splits` (adds a pane beside itself, then carries on). In mode
/// `quits` it exits on `/quit` + Enter (Codex's exit command) only. In mode
/// `escexits` it exits on Escape instead, leaving behind a child that logs whatever reaches
/// the terminal after it to `<log>.after` — as the shell a real agent leaves would read it.
const AGENT: &str = r#"
use strict; use Time::HiRes qw(time sleep);
my ($log, $mode, $sessions, $tmux, $pidfile) = @ARGV;
my $reg = "$sessions/$$.json";
system("stty raw -echo");
if ($pidfile) { open(my $p, ">", "$pidfile.tmp") or die; print $p $$; close $p; rename("$pidfile.tmp", $pidfile); }
open(my $f, ">", $log) or die; select((select($f), $| = 1)[0]);
my $seen = "";
while (sysread(STDIN, my $b, 64)) {
  printf $f "%.4f %s\n", time, unpack("H*", $b);
  $seen .= $b;
  if ($b =~ /\x1b/ && -e $reg) {
    local $/; open(my $r, "<", $reg) or next; my $j = <$r>; close $r;
    $j =~ s/"status":"busy"/"status":"idle"/;
    open(my $w, ">", "$reg.tmp") or next; print $w $j; close $w; rename("$reg.tmp", $reg);
  }
  if ($b =~ /\x1b/ && $mode eq "escexits") {
    my $k = fork();
    if ($k == 0) {
      open(my $g, ">", "$log.after") or die; select((select($g), $| = 1)[0]);
      while (sysread(STDIN, my $c, 64)) { printf $g "%s\n", unpack("H*", $c); }
      exit 0;
    }
    sleep 0.05 until -e "$log.after";
    exit 0;
  }
  exit 0 if $mode eq "quits" && $seen =~ m{/quit\r};
  if ($seen =~ m{/exit\r}) {
    $seen = "";
    exit 0 if $mode eq "exits";
    system($tmux, "split-window", "-d", "-t", $ENV{TMUX_PANE}, "sleep 60") if $mode eq "splits";
  }
}
"#;

struct Agent {
    f: FakeFleet,
    pid: i64,
    sid: String,
    session: String,
    log: PathBuf,
}

impl Agent {
    /// An agent alone in `fleet-test-<name>`, its pane's own process.
    fn start(name: &str, mode: &str) -> Option<Agent> {
        let f = FakeFleet::new()?;
        let (sid, log, line) = Self::line(&f, name, mode, false);
        let pid = f.agent(name, &sid, 80, 24, &format!("exec {line}"));
        f.wait_for("the agent to start", || log.exists());
        Some(Agent {
            pid,
            sid,
            session: format!("fleet-test-{name}"),
            log,
            f,
        })
    }

    /// An agent run by the pane's shell, which outlives it (`exec sleep`) — as a real one
    /// started from a login shell leaves its shell behind when it exits.
    fn under_shell(name: &str, mode: &str) -> Option<Agent> {
        let f = FakeFleet::new()?;
        let (sid, log, line) = Self::line(&f, name, mode, true);
        let shell = f.agent(name, &sid, 80, 24, &format!("{line}; exec sleep 300"));
        let pidfile = f.env.path(&format!("{name}.pid"));
        f.wait_for("the agent to start", || log.exists() && pidfile.exists());
        let pid: i64 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_ne!(pid, shell);
        let _ = std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{shell}.json")));
        f.register(pid, name, &sid, Some(&proc_start(pid)));
        Some(Agent {
            pid,
            sid,
            session: format!("fleet-test-{name}"),
            log,
            f,
        })
    }

    /// An agent of `harness` (`codex`, `pi`), alone in `fleet-test-<name>`: found by its
    /// command name, as those harnesses are, not by Claude's registry.
    fn as_harness(name: &str, harness: &str, mode: &str) -> Option<Agent> {
        let f = FakeFleet::new()?;
        // Its state dir under the temp `$HOME`: fleet looks for a harness that has run here.
        let home = if harness == "pi" {
            "home/.pi/agent"
        } else {
            "home/.codex"
        };
        std::fs::create_dir_all(f.env.path(home)).unwrap();
        let (sid, log, line) = Self::line(&f, name, mode, false);
        let pid = f.agent(
            name,
            &sid,
            80,
            24,
            &format!("exec /bin/bash -c \"exec -a {harness} {line}\""),
        );
        std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
        f.wait_for("the agent to start", || log.exists());
        Some(Agent {
            pid,
            sid,
            session: format!("fleet-test-{name}"),
            log,
            f,
        })
    }

    fn line(f: &FakeFleet, name: &str, mode: &str, pidfile: bool) -> (String, PathBuf, String) {
        let sid = format!("cccccccc-1111-2222-3333-{:012}", name.len());
        let script = f.env.path("agent.pl");
        std::fs::write(&script, AGENT).unwrap();
        let log = f.env.path(&format!("{name}.log"));
        let pidfile = if pidfile {
            f.env.path(&format!("{name}.pid")).display().to_string()
        } else {
            String::new()
        };
        // Where its registry entry is (`<its pid>.json`), to mark it idle on Escape.
        let reg = f.env.path("home/.claude/sessions");
        let line = format!(
            "perl '{}' '{}' '{mode}' '{}' '{}' '{pidfile}'",
            script.display(),
            log.display(),
            reg.display(),
            f.env.tmux_wrapper().display()
        );
        (sid, log, line)
    }

    /// Mark the session busy in its registry entry; the agent marks it idle on Escape.
    fn busy(&self) {
        let reg = self
            .f
            .env
            .path(&format!("home/.claude/sessions/{}.json", self.pid));
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
        v["status"] = "busy".into();
        std::fs::write(&reg, v.to_string()).unwrap();
    }

    fn target(&self) -> String {
        format!("pid:{}", self.pid)
    }

    /// `fleet close …` → (exit code, the one JSON object on stdout, stderr).
    fn close(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self.f.fleet(args);
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| Value::String(common::stdout(&out)));
        (out.status.code().unwrap_or(-1), v, stderr(&out))
    }

    /// `fleet close …` with `env` set too.
    fn close_with(&self, env: &[(&str, &str)], args: &[&str]) -> (i32, Value, String) {
        let mut c = self.f.env.cmd();
        c.env("SHELL", "/bin/sh")
            .env_remove("FLEET_FIXTURE")
            .timeout(std::time::Duration::from_secs(20));
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.args(args).output().unwrap();
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| Value::String(common::stdout(&out)));
        (out.status.code().unwrap_or(-1), v, stderr(&out))
    }

    /// A tmux wrapper for fleet that runs `script` (sh; `$T` is the private server's tmux)
    /// right before it passes on an `if-shell` — the command `close` kills with.
    fn wrapper_at_kill(&self, tag: &str, script: &str) -> String {
        self.wrapper(
            tag,
            &format!(
                "for a in \"$@\"; do if [ \"$a\" = if-shell ]; then\n{script}\nbreak; fi; done\nexec \"$T\" \"$@\""
            ),
        )
    }

    /// A tmux wrapper for fleet that logs every command it is given (one line each) to the
    /// returned log, then passes it on.
    fn logging_wrapper(&self, tag: &str) -> (String, PathBuf) {
        let log = self.f.env.path(&format!("tmux-{tag}.log"));
        let path = self.wrapper(
            tag,
            &format!("echo \"$*\" >> '{}'\nexec \"$T\" \"$@\"", log.display()),
        );
        (path, log)
    }

    /// A tmux wrapper for fleet running `script` (sh; `$T` is the private server's tmux, the
    /// command line is `$@`).
    fn wrapper(&self, tag: &str, script: &str) -> String {
        let path = self.f.env.path(&format!("tmux-{tag}.sh"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nT='{}'\n{script}\n",
                self.f.env.tmux_wrapper().display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// `proc_start` of the agent's `list --json` row.
    fn listed_start(&self) -> String {
        self.listed()["proc_start"].as_str().unwrap().to_string()
    }

    /// The agent's `list --json` row.
    fn listed(&self) -> Value {
        let out = self.f.fleet(&["list", "--json"]);
        let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["pid"] == self.pid)
            .unwrap_or_else(|| panic!("no row for {}: {rows}", self.pid))
            .clone()
    }

    /// What reached the terminal after an `escexits` agent exited.
    fn after_exit(&self) -> String {
        std::fs::read_to_string(format!("{}.after", self.log.display())).unwrap_or_default()
    }

    fn session_named(&self, name: &str) -> bool {
        self.f
            .tmux(&["has-session", "-t", &format!("={name}")])
            .status
            .success()
    }

    fn received(&self) -> Vec<u8> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' '))
            .flat_map(|(_, hex)| {
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn tmux_session_exists(&self) -> bool {
        self.f
            .tmux(&["has-session", "-t", &format!("={}", self.session)])
            .status
            .success()
    }

    fn alive(&self) -> bool {
        fleet::core::tmux::process_alive(self.pid)
    }
}

fn keys_of(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn failure_shape(v: &Value, kind: &str) {
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["kind"], kind, "{v}");
    assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
    assert_eq!(keys_of(v), ["error", "host", "kind", "ok"], "{v}");
}

#[test]
fn only_an_exact_target_closes_anything() {
    let Some(a) = Agent::start("cl-usage", "exits") else {
        return;
    };
    // A bare pid, a session id fragment, a title: never matched.
    let pid = a.pid.to_string();
    for t in [pid.as_str(), &a.sid[..8], "cl-usage"] {
        let (code, v, _) = a.close(&["close", t, "--json"]);
        assert_eq!(code, 1, "{t}: {v}");
        failure_shape(&v, "usage");
    }
    let (code, v, _) = a.close(&["close", "pid:abc", "--json"]);
    assert_eq!(code, 1);
    failure_shape(&v, "target_invalid");
    let (code, v, _) = a.close(&["close", &format!("pid:{}", a.pid + 100_000), "--json"]);
    assert_eq!(code, 1);
    failure_shape(&v, "target_not_found");
    // A command line clap rejects is a usage failure object too, exit 1.
    let (code, v, _) = a.close(&["close", "--json"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("usage")), "{v}");
    // A rejected value is one too, wherever --json stands.
    let t = a.target();
    for args in [
        vec!["close", &t, "--json", "--timeout", "x"],
        vec!["close", &t, "--timeout", "x", "--json"],
        vec!["close", "--json", &t, "--timeout", "601"],
        vec!["close", &t, "--timeout", "601", "--json"],
        vec!["close", &t, "--timeout=601", "--json"],
        // A flag given twice is one too.
        vec!["close", &t, "--timeout", "3", "--timeout", "4", "--json"],
        vec!["close", &t, "--json", "--timeout=3", "--timeout", "4"],
        vec!["close", &t, "--json", "--json"],
        // --expect-pid and --expect-start go together, and the start must be a time.
        vec!["close", &t, "--expect-pid", "1", "--json"],
        vec![
            "close",
            &t,
            "--json",
            "--expect-start",
            "2026-10-17T13:19:29Z",
        ],
        vec![
            "close",
            &t,
            "--json",
            "--expect-pid",
            "1",
            "--expect-start",
            "soon",
        ],
    ] {
        let (code, v, _) = a.close(&args);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("usage")),
            "{args:?}: {v}"
        );
    }

    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty(), "nothing was typed");
    assert!(a.tmux_session_exists() && a.alive());
}

#[test]
fn a_shared_tmux_session_is_refused_untouched() {
    let Some(a) = Agent::start("cl-shared", "exits") else {
        return;
    };
    let s = format!("={}", a.session);
    // A second window…
    let out =
        a.f.tmux(&["new-window", "-d", "-t", &format!("{s}:"), "sleep 60"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, v, _) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 1, "{v}");
    failure_shape(&v, "host_shared");
    assert!(v["error"].as_str().unwrap().contains("2 window(s)"), "{v}");
    // …or, with that window gone, a second pane beside the agent's.
    let out = a.f.tmux(&["kill-window", "-t", &format!("{s}:1")]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out =
        a.f.tmux(&["split-window", "-d", "-t", &format!("{s}:0"), "sleep 60"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (code, v, _) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 1, "{v}");
    failure_shape(&v, "host_shared");
    assert!(v["error"].as_str().unwrap().contains("2 pane(s)"), "{v}");
    // -n checks the same.
    let (code, v, _) = a.close(&["-n", "close", &a.target(), "--json"]);
    assert_eq!(code, 1, "{v}");
    failure_shape(&v, "host_shared");

    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty(), "nothing was typed");
    assert!(a.tmux_session_exists() && a.alive(), "nothing was killed");
}

#[test]
fn an_agent_that_exits_on_exit_is_closed_with_its_shell() {
    let Some(a) = Agent::under_shell("cl-exit", "exits") else {
        return;
    };
    let start = a.listed_start();
    assert!(
        start.ends_with('Z') && start.contains('T'),
        "RFC 3339 UTC: {start}"
    );
    let pid = a.pid.to_string();
    let (code, v, err) = a.close(&[
        "close",
        &format!("id:{}", a.sid),
        "--json",
        "--expect-pid",
        &pid,
        "--expect-start",
        &start,
    ]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(
        keys_of(&v),
        [
            "dry_run",
            "harness",
            "host",
            "how",
            "ok",
            "pid",
            "session_id",
            "tmux_session"
        ]
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["how"], "exit", "{v}");
    assert_eq!(v["pid"], a.pid);
    assert_eq!(v["session_id"], a.sid.as_str());
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["tmux_session"], a.session.as_str());
    assert_eq!(v["dry_run"], false);
    assert_eq!(a.received(), b"/exit\r", "an idle agent gets no Escape");
    assert!(!a.alive());
    assert!(!a.tmux_session_exists(), "the shell it left was killed too");
}

#[test]
fn an_agent_ignoring_exit_is_killed_after_the_timeout() {
    let Some(a) = Agent::start("cl-kill", "ignores") else {
        return;
    };
    let t0 = std::time::Instant::now();
    let (code, v, err) = a.close(&["close", &a.target(), "--json", "--timeout", "1"]);
    assert_eq!(code, 0, "{v} {err}");
    assert!(
        t0.elapsed() >= std::time::Duration::from_secs(1),
        "it waited"
    );
    assert_eq!(
        (v["ok"].as_bool(), v["how"].as_str()),
        (Some(true), Some("killed")),
        "{v}"
    );
    assert_eq!(v["tmux_session"], a.session.as_str());
    assert!(!a.tmux_session_exists());
    a.f.wait_for("the agent to die with its pane", || !a.alive());
}

#[test]
fn a_busy_agent_gets_escape_first() {
    let Some(a) = Agent::start("cl-busy", "exits") else {
        return;
    };
    a.busy();
    let (code, v, err) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["how"], "exit", "{v}");
    assert_eq!(a.received(), b"\x1b/exit\r");
    assert!(!a.tmux_session_exists());
}

#[test]
fn an_agent_that_exits_on_escape_gets_nothing_more_typed() {
    let Some(a) = Agent::under_shell("cl-esc", "escexits") else {
        return;
    };
    a.busy();
    let (code, v, err) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["how"], "exit", "{v}");
    assert_eq!(a.received(), b"\x1b");
    assert_eq!(
        a.after_exit(),
        "",
        "nothing typed to what the pane runs after it"
    );
    assert!(!a.alive());
    assert!(!a.tmux_session_exists(), "its shell was killed");
}

#[test]
fn another_process_is_target_changed() {
    let Some(a) = Agent::start("cl-ident", "exits") else {
        return;
    };
    let start = a.listed_start();
    let pid = a.pid.to_string();
    let other = (a.pid + 1).to_string();
    for (p, s) in [
        (other.as_str(), start.as_str()),
        (pid.as_str(), "2001-02-03T04:05:06Z"),
    ] {
        let (code, v, _) = a.close(&[
            "close",
            &a.target(),
            "--json",
            "--expect-pid",
            p,
            "--expect-start",
            s,
        ]);
        assert_eq!(code, 1, "{p} {s}: {v}");
        failure_shape(&v, "target_changed");
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .contains("nothing typed or killed"),
            "{v}"
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty(), "nothing was typed");
    assert!(a.tmux_session_exists() && a.alive());
}

#[test]
fn a_replaced_tmux_session_is_never_the_one_killed() {
    let Some(a) = Agent::start("cl-swap", "ignores") else {
        return;
    };
    // Right before the kill, the agent's tmux session is renamed and an unrelated one takes
    // its old name.
    let s = a.session.clone();
    let tmux = a.wrapper_at_kill(
        "swap",
        &format!(
            "\"$T\" rename-session -t '={s}' '{s}-old'\n\"$T\" new-session -d -s '{s}' sleep 60"
        ),
    );
    let (code, v, err) = a.close_with(
        &[("FLEET_TMUX", &tmux)],
        &["close", &a.target(), "--json", "--timeout", "0"],
    );
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["how"], "killed", "{v}");
    assert_eq!(
        v["tmux_session"],
        format!("{s}-old"),
        "named as it was at the kill"
    );
    assert!(
        !a.session_named(&format!("{s}-old")),
        "the agent's session was killed"
    );
    assert!(
        a.session_named(&s),
        "the unrelated session with its old name lives"
    );
    a.f.wait_for("the agent to die with its pane", || !a.alive());
}

#[test]
fn a_pane_split_at_the_kill_stops_it() {
    let Some(a) = Agent::start("cl-late", "ignores") else {
        return;
    };
    let tmux = a.wrapper_at_kill(
        "split",
        &format!("\"$T\" split-window -d -t '={}:' sleep 60", a.session),
    );
    let (code, v, err) = a.close_with(
        &[("FLEET_TMUX", &tmux)],
        &["close", &a.target(), "--json", "--timeout", "0"],
    );
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "host_shared");
    assert!(v["error"].as_str().unwrap().contains("2 pane(s)"), "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("nothing killed"),
        "{v}"
    );
    assert!(a.tmux_session_exists() && a.alive(), "nothing was killed");
}

#[test]
fn a_failed_tmux_query_is_io_never_success() {
    let Some(a) = Agent::start("cl-io", "ignores") else {
        return;
    };
    // The kill command itself fails.
    let tmux = a.wrapper_at_kill("fail", "echo 'tmux: server busy' >&2\nexit 1");
    let (code, v, err) = a.close_with(
        &[("FLEET_TMUX", &tmux)],
        &["close", &a.target(), "--json", "--timeout", "0"],
    );
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "io");
    assert!(a.tmux_session_exists() && a.alive(), "nothing was killed");
    // The kill runs, but the check that it took cannot be made.
    let tmux = a.wrapper_at_kill(
        "unconfirmed",
        "\"$T\" \"$@\"\necho 'tmux: lost connection' >&2\nexit 1",
    );
    let (code, v, err) = a.close_with(
        &[("FLEET_TMUX", &tmux)],
        &["close", &a.target(), "--json", "--timeout", "0"],
    );
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "io");
}

#[test]
fn a_pane_added_before_the_kill_stops_it() {
    let Some(a) = Agent::start("cl-race", "splits") else {
        return;
    };
    let (code, v, err) = a.close(&["close", &a.target(), "--json", "--timeout", "1"]);
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "host_shared");
    assert!(
        v["error"].as_str().unwrap().contains("nothing killed"),
        "{v}"
    );
    assert_eq!(a.received(), b"/exit\r");
    assert!(a.tmux_session_exists() && a.alive(), "nothing was killed");
}

#[test]
fn a_held_lock_is_busy() {
    let Some(a) = Agent::start("cl-lock", "exits") else {
        return;
    };
    std::fs::create_dir_all(a.f.env.path("locks")).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(a.f.env.path(&format!("locks/{}.lock", a.sid)))
        .unwrap();
    held.lock().unwrap();
    let (code, v, _) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 1, "{v}");
    failure_shape(&v, "busy");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty());
    assert!(a.tmux_session_exists() && a.alive());
}

#[test]
fn dry_run_touches_nothing() {
    let Some(a) = Agent::start("cl-dry", "exits") else {
        return;
    };
    a.busy();
    let (code, v, err) = a.close(&["-n", "close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["how"], "exit", "the plan: its exit command");
    assert_eq!(v["tmux_session"], a.session.as_str());
    // Text output says what it would do.
    let out = a.f.fleet(&["-n", "close", &a.target()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        common::stdout(&out).contains("would"),
        "{}",
        common::stdout(&out)
    );

    // FLEET_DRY_RUN=1 is -n: nothing typed or killed, nothing printed before the object.
    let (code, v, err) = a.close_with(&[("FLEET_DRY_RUN", "1")], &["close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["how"], "exit", "{v}");

    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty(), "no Escape, no /exit");
    assert!(a.tmux_session_exists() && a.alive());
}

#[test]
fn a_suspended_agent_gets_nothing_typed_and_is_killed() {
    let Some(a) = Agent::under_shell("cl-stopped", "exits") else {
        return;
    };
    // Busy, so it would get Escape first — but Ctrl-Z stopped it: what is typed would go
    // to whatever is in front of it.
    a.busy();
    let stop = |sig: &str| {
        std::process::Command::new("kill")
            .args([sig, &a.pid.to_string()])
            .status()
            .unwrap()
    };
    assert!(stop("-STOP").success());
    let (tmux, log) = a.logging_wrapper("stopped");
    let (code, v, err) = a.close_with(&[("FLEET_TMUX", &tmux)], &["close", &a.target(), "--json"]);
    let _ = stop("-KILL");
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["how"], "killed", "{v}");
    let sent = std::fs::read_to_string(&log).unwrap();
    assert!(
        !sent.contains("send-keys") && !sent.contains("paste-buffer"),
        "nothing typed, Escape included:\n{sent}"
    );
    assert!(!a.tmux_session_exists());
}

#[test]
fn a_session_resumed_elsewhere_before_the_kill_is_target_changed() {
    let Some(a) = Agent::start("cl-moved", "ignores") else {
        return;
    };
    // Another process, in a tmux session of its own, registered under the same session id —
    // as a resume of it would be. It takes the session over once the exit command is typed.
    let other = "fleet-test-cl-moved-2";
    let out = a.f.tmux(&[
        "new-session",
        "-d",
        "-s",
        other,
        "--",
        "/bin/sh",
        "-c",
        "exec sleep 300",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out =
        a.f.tmux(&["display-message", "-p", "-t", other, "#{pane_pid}"]);
    let pid2: i64 = common::stdout(&out).trim().parse().unwrap();
    a.f.register(pid2, "cl-moved", &a.sid, Some(&proc_start(pid2)));
    let reg = |pid: i64| a.f.env.path(&format!("home/.claude/sessions/{pid}.json"));
    let staged = a.f.env.path("staged.json");
    std::fs::rename(reg(pid2), &staged).unwrap();
    let tmux = a.wrapper(
        "moved",
        &format!(
            "for a in \"$@\"; do case \"$a\" in send-keys|paste-buffer) [ -e '{staged}' ] && mv '{staged}' '{to}' && rm -f '{from}';; esac; done\nexec \"$T\" \"$@\"",
            staged = staged.display(),
            to = reg(pid2).display(),
            from = reg(a.pid).display()
        ),
    );
    let (code, v, err) = a.close_with(
        &[("FLEET_TMUX", &tmux)],
        &[
            "close",
            &format!("id:{}", a.sid),
            "--json",
            "--timeout",
            "0",
        ],
    );
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "target_changed");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains(&format!("is now pid {pid2}")), "{v}");
    assert!(e.contains("exit command was typed, nothing killed"), "{v}");
    assert_eq!(a.received(), b"/exit\r");
    assert!(a.tmux_session_exists() && a.alive(), "nothing was killed");
    assert!(a.session_named(other));
}

#[test]
fn an_agent_off_its_panes_terminal_is_target_changed() {
    let Some(a) = Agent::start("cl-tty", "exits") else {
        return;
    };
    // tmux now says the agent's pane is on another terminal than the agent's.
    let tmux = a.wrapper(
        "tty",
        "for a in \"$@\"; do case \"$a\" in '#{pane_id}'?'#{pane_tty}') \"$T\" \"$@\" | sed 's|/dev/.*|/dev/fleet-test-nowhere|'; exit 0;; esac; done\nexec \"$T\" \"$@\"",
    );
    let (code, v, err) = a.close_with(&[("FLEET_TMUX", &tmux)], &["close", &a.target(), "--json"]);
    assert_eq!(code, 1, "{v} {err}");
    failure_shape(&v, "target_changed");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("no longer runs in pane"), "{v}");
    assert!(e.contains("nothing typed, nothing killed"), "{v}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(a.received().is_empty(), "nothing was typed");
    assert!(a.tmux_session_exists() && a.alive());
}

#[test]
fn a_codex_session_quits_on_quit() {
    let Some(a) = Agent::as_harness("cl-codex", "codex", "quits") else {
        return;
    };
    let row = a.listed();
    assert_eq!(row["harness"], "codex", "{row}");
    // Its start time: read fresh by close (an exact Codex lookup carries none), and checked.
    let start = a.listed_start();
    let pid = a.pid.to_string();
    let (code, v, _) = a.close(&[
        "close",
        &a.target(),
        "--json",
        "--expect-pid",
        &pid,
        "--expect-start",
        "2001-02-03T04:05:06Z",
    ]);
    assert_eq!(code, 1, "{v}");
    failure_shape(&v, "target_changed");
    assert!(a.received().is_empty());
    let (code, v, err) = a.close(&[
        "close",
        &a.target(),
        "--json",
        "--expect-pid",
        &pid,
        "--expect-start",
        &start,
    ]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["harness"], "codex", "{v}");
    assert_eq!(v["how"], "exit", "{v}");
    assert_eq!(v["tmux_session"], a.session.as_str());
    assert_eq!(
        a.received(),
        b"/quit\r",
        "Codex's exit command, not Claude's"
    );
    assert!(!a.alive());
    assert!(!a.tmux_session_exists());
}

#[test]
fn a_pi_session_goes_straight_to_the_kill() {
    let Some(a) = Agent::as_harness("cl-pi", "pi", "exits") else {
        return;
    };
    let (code, v, err) = a.close(&["-n", "close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(
        (v["harness"].as_str(), v["how"].as_str()),
        (Some("pi"), Some("killed")),
        "{v}"
    );
    let t0 = std::time::Instant::now();
    let (code, v, err) = a.close(&["close", &a.target(), "--json"]);
    assert_eq!(code, 0, "{v} {err}");
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(4),
        "no wait for an exit it was never asked for"
    );
    assert_eq!(v["harness"], "pi", "{v}");
    assert_eq!(v["how"], "killed", "{v}");
    assert!(
        a.received().is_empty(),
        "pi has no exit command: nothing typed"
    );
    assert!(!a.tmux_session_exists());
    a.f.wait_for("the agent to die with its pane", || !a.alive());
}
