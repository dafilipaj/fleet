//! Session recovery end to end, against an *isolated* tmux server: `FLEET_TMUX` is a wrapper
//! that runs `tmux -L fleet-test-<rand>` (with its own `TMUX_TMPDIR` too), `claude` is a fake
//! script that registers itself like Claude does and records its argv, and HOME, the config,
//! the snapshot and the boot id all live in a temp dir. A reboot is simulated by killing the
//! isolated server and changing `FLEET_BOOT_ID`. The user's own tmux server and sessions are
//! never visible, let alone touched.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Env, stderr, stdout};
use serde_json::Value;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
# A stand-in for `claude`: registers itself the way Claude does, logs its argv, idles.
id=""
prev=""
for a in "$@"; do
  [ "$prev" = "--resume" ] && id="$a"
  prev="$a"
done
[ -n "$id" ] || id="fake-$$-0000"
echo "$*" >> "$HOME/launches.log"
mkdir -p "$HOME/.claude/sessions"
printf '{"pid":%s,"sessionId":"%s","cwd":"%s","name":"n-%s","status":"idle","kind":"interactive","entrypoint":"cli"}' \
  $$ "$id" "$PWD" "$$" > "$HOME/.claude/sessions/$$.json"
trap 'rm -f "$HOME/.claude/sessions/$$.json"; exit 0' HUP TERM INT
while :; do sleep 1; done
"#;

struct Rig {
    env: Env,
    /// `TMUX_TMPDIR`: short (a socket path has a ~104-byte limit) and private, so even a tmux
    /// call that somehow bypassed the wrapper would reach an empty server, never the user's.
    sockets: tempfile::TempDir,
    boot: std::cell::RefCell<String>,
}

fn write_exe(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    /// `None` when tmux isn't installed — the test then passes vacuously.
    fn new() -> Option<Self> {
        let tmux = fleet::core::tools::find_binary("tmux")?;
        let env = Env::new();
        let sockets = tempfile::Builder::new()
            .prefix("ft")
            .tempdir_in("/tmp")
            .ok()?;
        for d in ["home", "bin", "work/one", "work/two", "state"] {
            std::fs::create_dir_all(env.path(d)).unwrap();
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let socket = format!("fleet-test-{nanos:x}");
        write_exe(
            &env.path("bin/tmux"),
            &format!(
                "#!/bin/sh\nexec '{}' -L '{socket}' \"$@\"\n",
                tmux.display()
            ),
        );
        write_exe(&env.path("bin/claude"), FAKE_CLAUDE);
        Some(Rig {
            env,
            sockets,
            boot: std::cell::RefCell::new("boot-1".into()),
        })
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.env.path(rel)
    }

    fn s(&self, rel: &str) -> String {
        self.p(rel).display().to_string()
    }

    fn cmd(&self) -> assert_cmd::Command {
        let mut c = self.env.cmd();
        c.env("FLEET_TMUX", self.p("bin/tmux"))
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("HOME", self.p("home"))
            .env("XDG_STATE_HOME", self.p("state"))
            .env("FLEET_SNAPSHOT", self.p("state/snapshot.json"))
            .env("FLEET_BOOT_ID", self.boot.borrow().as_str())
            .env("FLEET_CMD", self.p("bin/claude"))
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("FLEET_FIXTURE")
            .timeout(Duration::from_secs(30));
        c
    }

    fn fleet(&self, args: &[&str]) -> std::process::Output {
        self.cmd().args(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.fleet(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", stdout(&out)))
    }

    /// The isolated server, directly (through the same wrapper).
    fn tmux(&self, args: &[&str]) -> String {
        let mut c = std::process::Command::new(self.p("bin/tmux"));
        c.args(args);
        self.env.isolate(&mut c);
        let out = c
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("SHELL", "/bin/sh")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn registered(&self) -> Vec<Value> {
        let dir = self.p("home/.claude/sessions");
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect()
    }

    fn wait_for(&self, what: &str, ok: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Kill the isolated server (and with it every fake claude), then boot "again".
    fn reboot(&self, next: &str) {
        self.tmux(&["kill-server"]);
        self.wait_for("the fakes to exit", || self.registered().is_empty());
        *self.boot.borrow_mut() = next.into();
    }

    fn launches(&self) -> String {
        std::fs::read_to_string(self.p("home/launches.log")).unwrap_or_default()
    }

    fn snapshot(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.p("state/snapshot.json")).unwrap())
            .unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.tmux(&["kill-server"]);
        // Belt and braces: any fake still alive is ours — matched by this rig's own path.
        let _ = std::process::Command::new("pkill")
            .args(["-f", &self.s("bin/claude")])
            .output();
    }
}

#[test]
fn a_reboot_leaves_sessions_dormant_and_restore_brings_them_back() {
    let Some(r) = Rig::new() else { return };
    let one = r.s("work/one");
    let two = r.s("work/two");

    // --- boot 1: a two-window session with a Claude pane, and a plain shell session.
    let out = r.fleet(&["tmux", "new", "-d", "fleet-test-a", "-C", &one]);
    assert!(out.status.success(), "{}", stderr(&out));
    let first = r.tmux(&["list-panes", "-t", "=fleet-test-a:", "-F", "#{pane_id}"]);
    let first = first.trim();
    r.tmux(&["split-window", "-d", "-t", first, "-c", &two]);
    r.tmux(&[
        "new-window",
        "-d",
        "-t",
        "=fleet-test-a:",
        "-n",
        "logs",
        "-c",
        &two,
    ]);
    let launch = format!(
        "{} --dangerously-skip-permissions --model test-model -n some-name 'a prompt'",
        r.s("bin/claude")
    );
    r.tmux(&["send-keys", "-t", first, "-l", &launch]);
    r.tmux(&["send-keys", "-t", first, "Enter"]);
    assert!(
        r.fleet(&["tmux", "new", "-d", "fleet-test-b", "-C", &two])
            .status
            .success()
    );
    r.wait_for("the fake claude to register", || r.registered().len() == 1);
    let id = r.registered()[0]["sessionId"].as_str().unwrap().to_string();

    // `fleet list` records the snapshot.
    let rows = r.json(&["list", "--json"]);
    assert!(rows.is_array(), "list --json stays a bare array");
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    let snap = r.snapshot();
    assert_eq!(snap["bootId"], "boot-1");
    let a = snap["tmux"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "fleet-test-a")
        .unwrap_or_else(|| panic!("{snap}"));
    assert_eq!(a["windows"].as_array().unwrap().len(), 2);
    assert_eq!(a["windows"][1]["name"], "logs");
    assert_eq!(a["windows"][1]["autoName"], false);
    let claude = &a["windows"][0]["panes"][0]["claude"];
    assert_eq!(claude["sessionId"], id.as_str());
    assert_eq!(
        claude["flags"],
        serde_json::json!(["--dangerously-skip-permissions", "--model", "test-model"])
    );
    assert!(snap["dormant"]["tmux"].as_array().unwrap().is_empty());
    let mode = std::fs::metadata(r.p("state/snapshot.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // Same boot, same state: nothing is dormant.
    assert_eq!(
        r.json(&["restore", "--json"])["dormant"],
        serde_json::json!([])
    );

    // --- reboot.
    r.reboot("boot-2");
    let v = r.json(&["restore", "--json"]);
    assert_eq!(v["bootId"], "boot-2");
    let dormant = v["dormant"].as_array().unwrap();
    let names: Vec<&str> = dormant
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{v}");
    assert!(names.contains(&"fleet-test-a") && names.contains(&"fleet-test-b"));
    let da = dormant
        .iter()
        .find(|d| d["name"] == "fleet-test-a")
        .unwrap();
    assert_eq!(da["kind"], "tmux");
    assert_eq!(da["target"], "fleet-test-a");
    assert_eq!(
        (da["windows"].as_u64(), da["panes"].as_u64()),
        (Some(2), Some(3))
    );
    assert_eq!(da["sessions"][0]["sessionId"], id.as_str());
    assert!(da["since"].is_string());

    // The hints: the `list` footer and the dimmed `tmux list` rows.
    let out = r.fleet(&["list"]);
    assert!(
        stdout(&out).contains("2 dormant (fleet restore)"),
        "{}",
        stdout(&out)
    );
    let out = r.fleet(&["tmux", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("◌ fleet-test-a"), "{}", stdout(&out));
    assert!(stdout(&out).contains("dormant"), "{}", stdout(&out));
    assert_eq!(stdout(&r.fleet(&["tmux", "list", "-q"])).trim(), "");

    // Ambiguous and unknown targets start nothing.
    let out = r.fleet(&["restore", "fleet-test"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let out = r.fleet(&["restore", "zzz-nothing"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let out = r.fleet(&["-n", "enter", "fleet-test"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("dormant"), "{}", stderr(&out));

    // A dry run prints the plan and changes nothing.
    let out = r.fleet(&["-n", "restore", "fleet-test-a"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("new-session -d -s fleet-test-a"), "{text}");
    assert!(
        text.contains(&format!(
            "--dangerously-skip-permissions --model test-model --resume {id}"
        )),
        "{text}"
    );
    assert!(r.tmux(&["list-sessions"]).trim().is_empty());
    assert_eq!(
        r.json(&["restore", "--json"])["dormant"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // `enter` restores a dormant match before attaching (dry: the plan and the attach).
    let out = r.fleet(&["-n", "enter", "fleet-test-b"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("restoring dormant session fleet-test-b"));
    assert!(
        stdout(&out).contains("attach -t '=fleet-test-b'"),
        "{}",
        stdout(&out)
    );

    // --- the real restore.
    let v = r.json(&["restore", "fleet-test-a", "--json"]);
    let done = &v["restored"][0];
    assert_eq!(done["session"], "fleet-test-a");
    assert_eq!(done["renamed"], false);
    assert_eq!(done["launched"][0]["sessionId"], id.as_str());
    assert_eq!(v["failed"], serde_json::json!([]));
    let panes = r.tmux(&[
        "list-panes",
        "-s",
        "-t",
        "=fleet-test-a:",
        "-F",
        "#{window_name}|#{pane_current_path}",
    ]);
    let panes: Vec<&str> = panes.lines().collect();
    assert_eq!(panes.len(), 3, "{panes:?}");
    assert!(panes[2].starts_with("logs|"), "{panes:?}");
    r.wait_for("the resumed claude", || {
        r.registered().iter().any(|s| s["sessionId"] == id.as_str())
    });
    let last = r.launches().lines().last().unwrap_or_default().to_string();
    assert_eq!(
        last,
        format!("--dangerously-skip-permissions --model test-model --resume {id}"),
        "same flags, same session id, no prompt and no -n"
    );
    let canon = |p: &str| std::fs::canonicalize(p).unwrap();
    let reg = r.registered();
    let cwd = reg[0]["cwd"].as_str().unwrap();
    assert_eq!(canon(cwd), canon(&one));
    // No longer dormant; fleet-test-b still is.
    let v = r.json(&["restore", "--json"]);
    let names: Vec<&str> = v["dormant"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fleet-test-b"]);
    // Recorded as live on the next `list`.
    r.json(&["list", "--json"]);
    assert!(
        r.snapshot()["tmux"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "fleet-test-a")
    );

    // --- reboot again; this time the name is taken by the time we restore.
    r.reboot("boot-3");
    assert!(
        r.fleet(&["tmux", "new", "-d", "fleet-test-a", "-C", &two])
            .status
            .success()
    );
    let v = r.json(&["restore", "fleet-test-a", "--json"]);
    assert_eq!(v["restored"][0]["session"], "fleet-test-a-restored");
    assert_eq!(v["restored"][0]["renamed"], true);
    r.wait_for("the resumed claude", || {
        r.registered().iter().any(|s| s["sessionId"] == id.as_str())
    });

    // Forget: one, then all.
    let v = r.json(&["restore", "--forget", "fleet-test-b", "--json"]);
    assert_eq!(v["forgotten"], serde_json::json!(["fleet-test-b"]));
    let v = r.json(&["restore", "--json"]);
    assert_eq!(v["dormant"], serde_json::json!([]), "{v}");
}

#[test]
fn a_lone_claude_session_comes_back_in_its_own_tmux_session() {
    let Some(r) = Rig::new() else { return };
    let one = r.s("work/one");
    // A session that ran outside tmux (iTerm) before the reboot, with an unknown key.
    let snap = serde_json::json!({
        "version": 1, "host": "local", "bootId": "boot-0",
        "updatedAt": "2026-01-01T00:00:00Z", "someFutureKey": [1, 2],
        "tmux": [],
        "iterm": [ { "sessionId": "lone-1234-abcd", "name": "n", "cwd": one,
                     "title": "Fix the login page", "flags": ["--chrome"] } ],
        "dormant": { "tmux": [], "iterm": [] }
    });
    std::fs::write(r.p("state/snapshot.json"), snap.to_string()).unwrap();

    let v = r.json(&["restore", "--json"]);
    let d = &v["dormant"][0];
    assert_eq!(d["kind"], "claude");
    assert_eq!(d["target"], "lone-1234-abcd");
    assert_eq!(d["name"], "Fix the login page");
    assert_eq!(d["since"], "2026-01-01T00:00:00Z");

    let v = r.json(&["restore", "login", "--json"]);
    let done = &v["restored"][0];
    assert_eq!(done["kind"], "claude");
    assert_eq!(done["session"], "Fix-the-login-page");
    r.wait_for("the resumed claude", || {
        r.registered()
            .iter()
            .any(|s| s["sessionId"] == "lone-1234-abcd")
    });
    assert_eq!(
        r.launches().lines().last().unwrap_or_default(),
        "--chrome --resume lone-1234-abcd"
    );
    assert_eq!(
        r.tmux(&["list-sessions", "-F", "#{session_name}"]).trim(),
        "Fix-the-login-page"
    );
    let after = r.snapshot();
    assert_eq!(after["someFutureKey"], serde_json::json!([1, 2]));
    assert_eq!(after["dormant"]["iterm"], serde_json::json!([]));
}

#[test]
fn nothing_is_recorded_in_fixture_mode() {
    let env = Env::new();
    let fx = common::fixture(
        env.dir.path(),
        r#"[{"pid":42,"session_id":"fixture-1","name":"demo","status":"idle"}]"#,
    );
    let out = env
        .cmd()
        .env("FLEET_FIXTURE", &fx)
        .env("FLEET_BOOT_ID", "b")
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!env.path("snapshot.json").exists());
}

/// Start the fake claude in `pane` (a fresh session: no `--resume`).
fn start_fake(r: &Rig, pane: &str) {
    r.tmux(&["send-keys", "-t", pane, "-l", &r.s("bin/claude")]);
    r.tmux(&["send-keys", "-t", pane, "Enter"]);
}

fn first_pane(r: &Rig, session: &str) -> String {
    r.tmux(&[
        "list-panes",
        "-t",
        &format!("={session}:"),
        "-F",
        "#{pane_id}",
    ])
    .lines()
    .next()
    .unwrap_or_default()
    .to_string()
}

fn closed_names(v: &Value) -> Vec<String> {
    v["closed"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn sessions_closed_within_a_boot_are_recently_closed_not_dormant() {
    let Some(r) = Rig::new() else { return };
    let one = r.s("work/one");
    let two = r.s("work/two");

    // --- two tmux sessions, each with a Claude pane.
    for (name, dir) in [("fleet-test-c", &one), ("fleet-test-d", &two)] {
        let out = r.fleet(&["tmux", "new", "-d", name, "-C", dir]);
        assert!(out.status.success(), "{}", stderr(&out));
        start_fake(&r, &first_pane(&r, name));
    }
    r.wait_for("both fakes to register", || r.registered().len() == 2);
    r.json(&["list", "--json"]);
    let v = r.json(&["restore", "--json"]);
    assert_eq!(v["closed"], serde_json::json!([]), "{v}");

    // --- close fleet-test-d (the tmux session goes): closed at once, not dormant.
    let canon = |p: &str| std::fs::canonicalize(p).unwrap();
    let reg_in = |dir: &str| {
        r.registered()
            .into_iter()
            .find(|s| canon(s["cwd"].as_str().unwrap()) == canon(dir))
            .unwrap_or_else(|| panic!("no fake in {dir}"))
    };
    let d_id = reg_in(&two)["sessionId"].as_str().unwrap().to_string();
    let c_reg = reg_in(&one);
    let c_id = c_reg["sessionId"].as_str().unwrap().to_string();
    r.tmux(&["kill-session", "-t", "=fleet-test-d"]);
    r.wait_for("the closed fake to exit", || r.registered().len() == 1);
    r.json(&["list", "--json"]);
    let v = r.json(&["restore", "--closed", "--json"]);
    assert_eq!(closed_names(&v), ["fleet-test-d"]);
    assert_eq!(v["dormant"], serde_json::json!([]));
    let d = &v["closed"][0];
    assert_eq!(d["kind"], "tmux");
    assert_eq!(d["sessions"][0]["sessionId"], d_id.as_str());
    assert!(
        d["closedAt"].is_string() && d["since"] == d["closedAt"],
        "{d}"
    );

    // --- /exit in fleet-test-c: claude ends, the pane's shell stays.
    let pid = c_reg["pid"].as_i64().unwrap();
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .output();
    r.wait_for("the exited fake", || r.registered().is_empty());
    r.json(&["list", "--json"]);
    let v = r.json(&["restore", "--json"]);
    assert_eq!(v["closed"].as_array().unwrap().len(), 2, "{v}");
    let c = v["closed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "claude")
        .unwrap();
    assert_eq!(c["target"], c_id.as_str());
    assert_eq!(c["tmuxSession"], "fleet-test-c");

    // Closed is never part of --all, nor of the dormant hints.
    let v = r.json(&["restore", "--all", "--json"]);
    assert_eq!(v["restored"], serde_json::json!([]), "{v}");
    let out = r.fleet(&["restore"]);
    let text = stdout(&out);
    assert!(text.contains("nothing dormant"), "{text}");
    assert!(
        text.contains("2 recently closed (fleet restore --closed)"),
        "{text}"
    );
    let out = r.fleet(&["restore", "--closed"]);
    assert!(stdout(&out).contains("fleet-test-d"), "{}", stdout(&out));
    assert!(stdout(&out).contains("closed "), "{}", stdout(&out));
    assert!(!stdout(&r.fleet(&["list"])).contains("dormant"));
    let out = r.fleet(&["restore", "--closed", "zzz-nothing"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let out = r.fleet(&["restore", "--closed", "--all"]);
    assert!(!out.status.success(), "--closed --all is refused");

    // --- the Claude session comes back in a new window of its live tmux session.
    let out = r.fleet(&["-n", "restore", "--closed", &c_id]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("new-window -d -t '=fleet-test-c:'"),
        "{}",
        stdout(&out)
    );
    let v = r.json(&["restore", "--closed", &c_id, "--json"]);
    let done = &v["restored"][0];
    assert_eq!(done["session"], "fleet-test-c", "{v}");
    assert_eq!(done["renamed"], false);
    r.wait_for("the resumed claude", || {
        r.registered()
            .iter()
            .any(|s| s["sessionId"] == c_id.as_str())
    });
    let windows = r.tmux(&[
        "list-windows",
        "-t",
        "=fleet-test-c:",
        "-F",
        "#{window_index}",
    ]);
    assert_eq!(windows.lines().count(), 2, "{windows}");
    assert_eq!(
        r.launches().lines().last().unwrap_or_default(),
        format!("--resume {c_id}")
    );

    // --- the whole tmux session comes back under its name.
    let v = r.json(&["restore", "--closed", "fleet-test-d", "--json"]);
    assert_eq!(v["restored"][0]["session"], "fleet-test-d", "{v}");
    r.wait_for("the resumed claude", || {
        r.registered()
            .iter()
            .any(|s| s["sessionId"] == d_id.as_str())
    });
    r.json(&["list", "--json"]);
    assert_eq!(
        r.json(&["restore", "--json"])["closed"],
        serde_json::json!([])
    );

    // --- forget: close fleet-test-d again, then forget everything closed.
    r.tmux(&["kill-session", "-t", "=fleet-test-d"]);
    r.wait_for("the closed fake to exit", || r.registered().len() == 1);
    r.json(&["list", "--json"]);
    let v = r.json(&["restore", "--closed", "--forget-all", "--json"]);
    assert_eq!(v["forgotten"], serde_json::json!(["fleet-test-d"]), "{v}");
    assert_eq!(
        r.json(&["restore", "--json"])["closed"],
        serde_json::json!([])
    );

    // --- closed just before a reboot (within the grace period): dormant, not closed.
    r.tmux(&["kill-session", "-t", "=fleet-test-c"]);
    r.wait_for("the fakes to exit", || r.registered().is_empty());
    r.json(&["list", "--json"]);
    assert_eq!(
        closed_names(&r.json(&["restore", "--json"])),
        ["fleet-test-c"]
    );
    r.reboot("boot-2");
    let v = r.json(&["restore", "--json"]);
    assert_eq!(v["closed"], serde_json::json!([]), "{v}");
    let names: Vec<&str> = v["dormant"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fleet-test-c"]);
    // The file keeps its closed list alongside (additive).
    assert!(r.snapshot()["closed"].is_object());
}
