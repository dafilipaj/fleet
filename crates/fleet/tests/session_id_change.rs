//! A session's id changing between its lookup and its lock: the typing lock is named after the
//! session id, and Claude Code's `/clear` gives the same process a new one. A command that
//! locked the old id while another fleet command can lock the new one would type into the same
//! terminal at once — so `send`, `keys`, `rename`, `answer` and `close` look the process up
//! again once they hold the lock, and type (or kill) nothing when its id is no longer the one
//! locked (`target_changed`).
//!
//! The fake agent draws a captured screen and logs every byte it reads. A tmux wrapper rewrites
//! its registry entry to the new id during the command's first lookup, after that lookup has
//! read it (the lookup asks tmux for its panes last) and before the lock is taken.
//!
//! These tests need tmux and perl, and fail without them rather than pass having run nothing.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fake::FakeFleet;
use common::{stderr, stdout};
use serde_json::Value;

const OLD: &str = "cccccccc-1111-2222-3333-444444444444";
const NEW: &str = "cccccccc-5555-6666-7777-888888888888";

const LOGGER: &str = r#"open(my $f, ">>", $ARGV[0]) or die; select((select($f), $| = 1)[0]);
while (sysread(STDIN, my $b, 64)) { print $f $b }
"#;

struct Agent {
    f: FakeFleet,
    pid: i64,
    log: PathBuf,
}

/// A fake Claude Code session registered as [`OLD`], drawing `screen` (testdata/screens).
fn agent(screen: &str) -> Agent {
    assert!(
        fleet::core::tools::find_binary("perl").is_some(),
        "these tests need perl"
    );
    let f = FakeFleet::new().expect("these tests need tmux");
    let log = f.env.path("received.log");
    let logger = f.env.path("logger.pl");
    std::fs::write(&logger, LOGGER).unwrap();
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../testdata/screens/{screen}.txt"));
    let (cols, rows) = if screen.starts_with("live/") {
        (110, 34)
    } else {
        (80, 24)
    };
    let script = format!(
        "cat '{}'; stty raw -echo; exec perl '{}' '{}'",
        path.display(),
        logger.display(),
        log.display()
    );
    let pid = f.agent("idchange", OLD, cols, rows, &script);
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent { f, pid, log }
}

impl Agent {
    /// `fleet <args>` with a tmux that, the first time it is asked for the panes' terminals
    /// (the lookup's last step), rewrites the registry entry to [`NEW`] — the `/clear`
    /// finishing — before it answers. `(exit code, the JSON object on stdout, stderr)`.
    fn fleet_as_the_id_changes(&self, args: &[&str]) -> (i32, Value, String) {
        let env = &self.f.env;
        let reg = env.path(&format!("home/.claude/sessions/{}.json", self.pid));
        let fired = env.path("changed");
        let wrapper = env.path("clear-tmux.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in *list-panes*pane_tty*)\n\
                 if [ ! -e '{fired}' ]; then\n\
                 : > '{fired}'\n\
                 perl -pi -e 's/{OLD}/{NEW}/' '{reg}'\n\
                 fi;;\n\
                 esac\n\
                 exec '{real}' \"$@\"\n",
                fired = fired.display(),
                reg = reg.display(),
                real = env.tmux_wrapper().display(),
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = env
            .cmd()
            .env("SHELL", "/bin/sh")
            .env("FLEET_TMUX", &wrapper)
            .timeout(Duration::from_secs(20))
            .args(args)
            .output()
            .unwrap();
        assert!(fired.exists(), "the id never changed: {}", stderr(&out));
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
        (out.status.code().unwrap_or(-1), v, stderr(&out))
    }

    /// What the agent has read, once a moment has passed for anything on its way.
    fn received(&self) -> String {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn target(&self) -> String {
        format!("pid:{}", self.pid)
    }
}

/// `target_changed`, exit 1, naming both ids; nothing typed.
fn changed(a: &Agent, (code, v, err): (i32, Value, String)) {
    assert_eq!(code, 1, "{v} {err}");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["kind"], "target_changed", "{v}");
    let error = v["error"].as_str().unwrap();
    assert!(error.contains(OLD) && error.contains(NEW), "{v}");
    assert_eq!(a.received(), "", "nothing typed");
}

#[test]
fn send_types_nothing_once_the_session_id_changed_under_its_lock() {
    let a = agent("live/claude-empty");
    let t = a.target();
    changed(
        &a,
        a.fleet_as_the_id_changes(&["send", "--json", &t, "hello"]),
    );
}

#[test]
fn keys_presses_nothing_once_the_session_id_changed_under_its_lock() {
    let a = agent("live/claude-empty");
    let t = a.target();
    changed(
        &a,
        a.fleet_as_the_id_changes(&["keys", "--json", &t, "Escape"]),
    );
}

#[test]
fn rename_types_nothing_once_the_session_id_changed_under_its_lock() {
    let a = agent("live/claude-empty");
    let t = a.target();
    changed(
        &a,
        a.fleet_as_the_id_changes(&["rename", &t, "probe", "--no-tmux-sync", "--json"]),
    );
}

#[test]
fn answer_types_nothing_once_the_session_id_changed_under_its_lock() {
    let a = agent("claude-permission");
    let t = a.target();
    changed(
        &a,
        a.fleet_as_the_id_changes(&["answer", &t, "--choice", "1", "--json"]),
    );
}

// `close` types Escape and the exit command before it kills: nothing of it once the id changed,
// and its tmux session is left standing.
#[test]
fn close_types_and_kills_nothing_once_the_session_id_changed_under_its_lock() {
    let a = agent("live/claude-empty");
    let t = a.target();
    changed(
        &a,
        a.fleet_as_the_id_changes(&["close", &t, "--json", "--timeout", "0"]),
    );
    let out = a.f.tmux(&["has-session", "-t", "fleet-test-idchange"]);
    assert!(out.status.success(), "its tmux session was killed");
}

// The same commands with the id unchanged: the lookup under the lock lets them type.
#[test]
fn an_unchanged_session_id_types() {
    let a = agent("live/claude-empty");
    let t = a.target();
    let out = a.f.fleet(&["send", "--json", &t, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    a.f.wait_for("the text and its Enter", || {
        std::fs::read_to_string(&a.log).is_ok_and(|r| r.ends_with('\r'))
    });
    assert_eq!(a.received(), "hello\r");
}
