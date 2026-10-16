//! Fake agents on the test's private tmux server (see [`Env`]): a detached session running a
//! script, registered in the temp `$HOME`'s Claude registry like a real session. The user's
//! own tmux server and sessions are never visible, let alone touched.

use std::process::Output;
use std::time::{Duration, Instant};

use super::Env;

pub struct FakeFleet {
    pub env: Env,
}

impl FakeFleet {
    /// `None` when tmux isn't installed — the tests then pass vacuously.
    pub fn new() -> Option<Self> {
        fleet::core::tools::find_binary("tmux")?;
        let env = Env::new();
        std::fs::create_dir_all(env.path("home/.claude/sessions")).unwrap();
        Some(FakeFleet { env })
    }

    /// `tmux …` on the private server.
    pub fn tmux(&self, args: &[&str]) -> Output {
        self.env.tmux(args)
    }

    /// `fleet …`, isolated, against the fake registry.
    pub fn fleet(&self, args: &[&str]) -> Output {
        let mut c = self.env.cmd();
        c.env("SHELL", "/bin/sh")
            .env_remove("FLEET_FIXTURE")
            .timeout(Duration::from_secs(20));
        c.args(args).output().unwrap()
    }

    /// Start `script` (run by `/bin/sh -c`, which should `exec` the long-lived part so the
    /// pane's pid stays the agent's) in a detached `cols`×`rows` session `fleet-test-<name>`,
    /// and register it as `session_id` — with its real `procStart`, as Claude does. Returns
    /// its pid.
    pub fn agent(&self, name: &str, session_id: &str, cols: u16, rows: u16, script: &str) -> i64 {
        let session = format!("fleet-test-{name}");
        let out = self.tmux(&[
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            "--",
            "/bin/sh",
            "-c",
            script,
        ]);
        assert!(
            out.status.success(),
            "tmux new-session: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = self.tmux(&["display-message", "-p", "-t", &session, "#{pane_pid}"]);
        let pid: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        self.register(pid, name, session_id, Some(&proc_start(pid)));
        pid
    }

    /// Write `pid`'s registry entry (`proc_start`: Claude's `procStart`, `None` to omit it).
    pub fn register(&self, pid: i64, name: &str, session_id: &str, proc_start: Option<&str>) {
        let mut reg = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": self.env.dir.path(),
            "name": name,
            "nameSource": "user",
            "status": "idle",
            "kind": "interactive",
            "entrypoint": "cli",
        });
        if let Some(p) = proc_start {
            reg["procStart"] = p.into();
        }
        std::fs::write(
            self.env.path(&format!("home/.claude/sessions/{pid}.json")),
            reg.to_string(),
        )
        .unwrap();
    }

    /// Wait (≤ 5 s) until `f` holds.
    pub fn wait_for(&self, what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A process's start time the way Claude records it (`procStart`): `ps -o lstart` in UTC.
pub fn proc_start(pid: i64) -> String {
    let out = std::process::Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
