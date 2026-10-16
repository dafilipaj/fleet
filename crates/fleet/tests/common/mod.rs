//! Shared helpers: every test runs `fleet` against a temp config (never the
//! user's real one) with ssh multiplexing off.
#![allow(dead_code, deprecated)]

pub mod fake;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Overrides fleet (or Claude, Codex, pi) consults before `HOME`: left in place they would
/// point a test at the developer's real state, config or agent sessions. Every process a
/// test starts loses them (a test that wants one sets it after [`Env::cmd`]).
pub const UNSET: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "CODEX_HOME",
    "PI_CODING_AGENT_DIR",
    "CLAUDE_CONFIG_DIR",
    "FLEET_HOST",
    "FLEET_DRY_RUN",
    "FLEET_AS_HOST",
    "FLEET_FIXTURE",
    "FLEET_BRIEFS_DIR",
    "FLEET_STACKS_DIR",
    "FLEET_JOBS_DIR",
    "FLEET_GROUPS_STATE",
    "FLEET_REPOS_STATE",
    "FLEET_PRS_CACHE",
    "FLEET_PRS_SEEN",
    "FLEET_BOOT_ID",
    "FLEET_CMD",
    "FLEET_BIN",
    "FLEET_GH",
    "FLEET_NODE",
    "FLEET_WEB_BIND",
    "FLEET_WEB_PORT",
    "FLEET_MCP_TOKEN",
];

/// One test's world. Every `fleet` it runs ([`Env::cmd`] / [`Env::std_cmd`]) is isolated
/// from the machine:
///
/// - `HOME` is `<dir>/home`, so no real Claude registry, transcript or name cache is read,
///   and every override in [`UNSET`] is cleared;
/// - tmux goes to a private server: `FLEET_TMUX` is a wrapper running `tmux -L
///   fleet-test-<rand>`, `TMUX_TMPDIR` a private dir under `/tmp` (a socket path has a
///   ~104-byte limit), and `TMUX` / `TMUX_PANE` are cleared so an outer tmux cannot steer it.
///
/// Tests never run the `fleet` binary any other way — `tests/isolation.rs` checks.
/// Whatever server a test starts there is killed when the `Env` drops.
pub struct Env {
    pub dir: tempfile::TempDir,
    pub sockets: tempfile::TempDir,
    pub socket: String,
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sockets = tempfile::Builder::new()
            .prefix("ft")
            .tempdir_in("/tmp")
            .unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let socket = format!("fleet-test-{}-{nanos:x}", std::process::id());
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        let wrapper = dir.path().join("tmux.sh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec '{}' -L '{socket}' \"$@\"\n",
                Self::real_tmux().display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Env {
            dir,
            sockets,
            socket,
        }
    }

    fn real_tmux() -> PathBuf {
        fleet::core::tools::find_binary("tmux").unwrap_or_else(|| PathBuf::from("tmux"))
    }

    /// The `tmux -L <socket>` wrapper `fleet` is pointed at.
    pub fn tmux_wrapper(&self) -> PathBuf {
        self.path("tmux.sh")
    }

    /// Isolate any process the test runs itself (a tmux, a fake agent) the way
    /// [`Env::cmd`] isolates `fleet`: temp HOME, private TMUX_TMPDIR, [`UNSET`] cleared.
    pub fn isolate(&self, c: &mut std::process::Command) {
        c.env("HOME", self.path("home"))
            .env("TMUX_TMPDIR", self.sockets.path());
        for k in UNSET {
            c.env_remove(k);
        }
    }

    /// What every `fleet` under test gets (`Some`) or loses (`None`).
    fn fleet_env(&self) -> Vec<(&'static str, Option<OsString>)> {
        let mut v: Vec<(&'static str, Option<OsString>)> =
            UNSET.iter().map(|k| (*k, None)).collect();
        v.extend([
            ("FLEET_CONFIG", Some(self.config_path().into())),
            ("HOME", Some(self.path("home").into())),
            ("FLEET_TMUX", Some(self.tmux_wrapper().into())),
            ("TMUX_TMPDIR", Some(self.sockets.path().into())),
            // `fleet list` records the session snapshot: never into the real one.
            ("FLEET_SNAPSHOT", Some(self.path("snapshot.json").into())),
            ("FLEET_MUX", Some("0".into())),
            ("FLEET_CONNECT_TIMEOUT", Some("2".into())),
            ("NO_COLOR", Some("1".into())),
        ]);
        v
    }

    /// `fleet` as a plain `std::process::Command` (to spawn it, or to watch it run), isolated
    /// exactly like [`Env::cmd`].
    pub fn std_cmd(&self) -> std::process::Command {
        let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("fleet"));
        for (k, v) in self.fleet_env() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c
    }

    /// `tmux …` on this test's private server.
    pub fn tmux(&self, args: &[&str]) -> std::process::Output {
        let mut c = std::process::Command::new(self.tmux_wrapper());
        c.args(args);
        self.isolate(&mut c);
        c.output().unwrap()
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.path().join("config.json")
    }

    pub fn write_config(&self, v: &serde_json::Value) -> PathBuf {
        let p = self.config_path();
        std::fs::write(&p, serde_json::to_string_pretty(v).unwrap()).unwrap();
        p
    }

    pub fn read_config(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.config_path()).unwrap()).unwrap()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// `fleet` with FLEET_CONFIG pointed into the temp dir, isolated (see [`Env`]).
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::cargo_bin("fleet").unwrap();
        for (k, v) in self.fleet_env() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c
    }
}

/// A two-host config: this machine is `laptop`, tmux commands go to
/// `workstation` (reachable as ssh alias `devbox`).
pub fn two_hosts() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "self": "laptop",
        "defaultHost": "workstation",
        "hosts": {
            "laptop": { "ssh": null, "web": null },
            "workstation": { "ssh": "devbox", "web": null }
        },
        "web": { "port": 7777, "bind": "0.0.0.0", "dir": null },
        "tmux": null,
        "spawnDirs": []
    })
}

/// The `fleet` binary's path, for a config that names it (a fake host's `fleetBin`). Only
/// the path: whatever runs it is a `fleet` an [`Env`] started, so it keeps that isolation.
pub fn fleet_bin() -> PathBuf {
    assert_cmd::cargo::cargo_bin("fleet")
}

pub fn fixture(dir: &Path, json: &str) -> PathBuf {
    let p = dir.join("fleet-fixture.json");
    std::fs::write(&p, json).unwrap();
    p
}

pub fn stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

impl Drop for Env {
    fn drop(&mut self) {
        // Only when a server was started: the socket exists under the private dir.
        let started = std::fs::read_dir(self.sockets.path())
            .into_iter()
            .flatten()
            .flatten()
            .any(|d| d.path().join(&self.socket).exists());
        if started {
            let _ = self.tmux(&["kill-server"]);
        }
    }
}
