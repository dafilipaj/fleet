//! Small environment helpers: locating binaries, expanding `~`, shell quoting.
//!
//! A `fleet` started over ssh runs under a non-login shell whose PATH often lacks
//! Homebrew and `~/.local/bin`, so every external tool is resolved here, with the
//! usual install locations as fallbacks, instead of trusting a bare name.
//! `FLEET_PATH_ONLY=1` turns the fallbacks off (see [`path_only`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `FLEET_PATH_ONLY=1`: binaries come from `$PATH` alone — no fallback dirs, no
/// hard-coded install locations, no login-shell lookup — so a hermetic test or sandbox
/// decides what fleet can run. Explicit paths (`FLEET_TMUX`, `FLEET_GH`, the config's) still
/// win. Panes fleet opens carry it on ([`pane_env`]); what reaches past PATH by design is
/// refused ([`deny_path_only`]).
pub fn path_only() -> bool {
    env_flag("FLEET_PATH_ONLY")
}

/// Refuse `what` under [`path_only`]: the web server, service installs, other hosts, iTerm
/// and a local `exec` / `ssh` reach past PATH by design, so they fail (kind `usage`) rather
/// than half-honour it.
pub fn deny_path_only(what: &str) -> crate::error::Result<()> {
    path_only_refusal(path_only(), what)
}

fn path_only_refusal(on: bool, what: &str) -> crate::error::Result<()> {
    if on {
        return Err(refusal(format!(
            "{what} is not available with FLEET_PATH_ONLY set"
        )));
    }
    Ok(())
}

/// A [`path_only`] refusal: kind `usage`, the message naming the switch (see [`is_refusal`]).
fn refusal(msg: String) -> crate::error::Error {
    debug_assert!(msg.contains("FLEET_PATH_ONLY"), "{msg}");
    crate::error::Error::tagged("usage", msg)
}

/// Is `e` a [`path_only`] refusal ([`deny_path_only`], [`require_path`], [`pane_sh`])? A
/// `--json` command answers one with its failure object; a fan-out skips the host it names.
pub fn is_refusal(e: &crate::error::Error) -> bool {
    e.kind() == "usage" && e.to_string().contains("FLEET_PATH_ONLY")
}

/// Under [`path_only`], `PATH` must be set and non-empty: lookup would find nothing, but a
/// bare-name exec (`ps`, `git`) would fall back to the C library's default path
/// (`/usr/bin:/bin`), past what the caller declared. Every entry must be absolute too: a
/// relative or empty one (the current directory) would mean another directory in each pane
/// fleet opens, which tmux starts in its own directory. Checked at startup (kind `usage`).
pub fn require_path() -> crate::error::Result<()> {
    path_check(path_only(), std::env::var_os("PATH").as_deref())
}

fn path_check(on: bool, path: Option<&std::ffi::OsStr>) -> crate::error::Result<()> {
    if !on {
        return Ok(());
    }
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Err(refusal(
            "PATH is unset or empty, and FLEET_PATH_ONLY allows nothing outside it".into(),
        ));
    };
    if let Some(dir) = std::env::split_paths(path).find(|d| !d.is_absolute()) {
        let entry = if dir.as_os_str().is_empty() {
            "an empty entry".to_string()
        } else {
            format!("`{}`", dir.display())
        };
        return Err(refusal(format!(
            "PATH holds {entry}, not an absolute directory, and FLEET_PATH_ONLY needs every entry absolute"
        )));
    }
    Ok(())
}

/// The shell tmux starts a spawned pane with (`-c` the spawn script, which then becomes
/// `$SHELL`): `/bin/sh`, or under [`path_only`] the `sh` on `PATH`, refused (kind `usage`)
/// when `PATH` has none.
pub fn pane_sh() -> crate::error::Result<String> {
    Lookup::from_env().pane_sh().ok_or_else(|| {
        refusal("a tmux pane needs `sh`, and PATH has none with FLEET_PATH_ONLY set".into())
    })
}

/// `KEY=value`s (for tmux `-e`) a pane fleet opens must start with under [`path_only`]:
/// this process's `PATH` and `FLEET_PATH_ONLY=1`. Otherwise a pane gets the tmux server's
/// environment, whose `PATH` is whatever started the server. Empty without the switch.
pub fn pane_env() -> Vec<String> {
    Lookup::from_env().pane_env()
}

/// What binary lookup sees: `$PATH`, `$HOME` (for the fallback dirs) and [`path_only`],
/// read per call. Tests build one by hand instead of touching the process environment.
struct Lookup {
    path: OsString,
    home: Option<PathBuf>,
    path_only: bool,
}

impl Lookup {
    fn from_env() -> Self {
        Lookup {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: dirs::home_dir(),
            path_only: path_only(),
        }
    }

    /// Directories searched after `$PATH` — none under [`path_only`].
    fn fallback_dirs(&self) -> Vec<PathBuf> {
        if self.path_only {
            return Vec::new();
        }
        let mut dirs = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        if let Some(home) = &self.home {
            dirs.push(home.join(".local/bin"));
            dirs.push(home.join(".cargo/bin"));
            dirs.push(home.join("bin"));
        }
        dirs
    }

    fn search_path(&self) -> OsString {
        let extra = self.fallback_dirs();
        if extra.is_empty() {
            return self.path.clone();
        }
        std::env::join_paths(std::env::split_paths(&self.path).chain(extra))
            .unwrap_or_else(|_| self.path.clone())
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        std::env::split_paths(&self.path)
            .chain(self.fallback_dirs())
            .map(|d| d.join(name))
            .find(|p| is_executable(p))
    }

    fn install_location<T>(&self, loc: T) -> Option<T> {
        (!self.path_only).then_some(loc)
    }

    fn pane_sh(&self) -> Option<String> {
        if !self.path_only {
            return Some("/bin/sh".into());
        }
        self.find("sh").map(|p| p.display().to_string())
    }

    fn pane_env(&self) -> Vec<String> {
        if !self.path_only {
            return Vec::new();
        }
        vec![
            format!("PATH={}", self.path.to_string_lossy()),
            "FLEET_PATH_ONLY=1".into(),
        ]
    }

    fn resolve(&self, name: &str, explicit: Option<&str>) -> String {
        if let Some(e) = explicit.map(str::trim).filter(|e| !e.is_empty()) {
            return expand_tilde(e);
        }
        self.find(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| name.to_string())
    }
}

/// `$PATH` with the fallback dirs appended — for a child that itself looks things up
/// (pi's launcher is a script that runs `node`). Under [`path_only`], `$PATH` as is.
pub fn search_path() -> OsString {
    Lookup::from_env().search_path()
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// First executable called `name` on `$PATH`, then in the fallback dirs.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    Lookup::from_env().find(name)
}

/// A hard-coded place to try when lookup fails (an app bundle, a tool's own install dir):
/// `Some(loc)`, or `None` under [`path_only`].
pub fn install_location<T>(loc: T) -> Option<T> {
    Lookup::from_env().install_location(loc)
}

/// `explicit` (a configured path, `~` allowed) when set, else [`find_binary`],
/// else the bare name so the spawn reports a clear "not found".
pub fn resolve_binary(name: &str, explicit: Option<&str>) -> String {
    Lookup::from_env().resolve(name, explicit)
}

/// The tmux binary: `$FLEET_TMUX`, then config `tmux`, then PATH + fallbacks.
/// Resolved once per process.
pub fn tmux() -> &'static str {
    static TMUX: OnceLock<String> = OnceLock::new();
    TMUX.get_or_init(|| {
        let env = std::env::var("FLEET_TMUX").ok();
        let cfg = crate::core::config::load().config.tmux.clone();
        resolve_binary("tmux", env.as_deref().or(cfg.as_deref()))
    })
}

/// A `tmux` command with `-u`: without a UTF-8 locale (launchd, cron, `env -i`) tmux
/// escapes non-ASCII and control characters in its output — `-F` tab separators come
/// back as `_`, so pane discovery found nothing — and mangles UTF-8 typed via send-keys.
pub fn tmux_cmd() -> std::process::Command {
    #[cfg(not(test))]
    let mut cmd = std::process::Command::new(tmux());
    #[cfg(test)]
    let mut cmd = test_tmux::command();
    cmd.arg("-u");
    cmd
}

/// Unit tests never reach the user's tmux server: every [`tmux_cmd`] a test builds goes to a
/// private socket (`-L fleet-test-unit`) under a private `TMUX_TMPDIR`, with `TMUX` and
/// `TMUX_PANE` cleared so an outer tmux cannot redirect it. Nothing starts a server there,
/// so each call fails like "no server running" — which is what the tests that poke tmux with
/// made-up targets expect, now independent of whatever the machine runs.
#[cfg(test)]
pub(crate) mod test_tmux {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    pub const SOCKET: &str = "fleet-test-unit";

    /// `$TMPDIR/fleet-test-unit`: short enough for a socket path, and one dir for every test
    /// run — no server is ever started in it, so concurrent runs cannot collide, and repeated
    /// runs leave nothing new behind.
    pub fn dir() -> &'static Path {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let d = std::env::temp_dir().join(SOCKET);
            std::fs::create_dir_all(&d).unwrap();
            d
        })
    }

    /// The real tmux binary — never `FLEET_TMUX` or the config's `tmux`, which may be a
    /// wrapper pinning `-S <socket>` (that beats `-L`) — on the private socket.
    pub fn command() -> std::process::Command {
        let bin = super::find_binary("tmux").unwrap_or_else(|| PathBuf::from("tmux"));
        let mut cmd = std::process::Command::new(bin);
        cmd.env("TMUX_TMPDIR", dir())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args(["-L", SOCKET]);
        cmd
    }
}

/// The `claude` binary used for headless naming calls.
pub fn claude() -> Option<String> {
    find_binary("claude")
        .or_else(|| {
            install_location(dirs::home_dir()?.join(".claude/local/claude"))
                .filter(|p| is_executable(p))
        })
        .map(|p| p.display().to_string())
}

/// Expand a leading `~` / `~/` against `$HOME`. Anything else passes through.
pub fn expand_tilde(p: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return p.to_string();
    };
    if p == "~" {
        return home.display().to_string();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home.join(rest).display().to_string();
    }
    p.to_string()
}

/// Rewrite an absolute path under this machine's `$HOME` to `~/…`, so it means
/// "the same place under *your* home" on a remote host.
pub fn tildify(p: &str) -> String {
    match dirs::home_dir().and_then(|h| h.to_str().map(String::from)) {
        Some(home) if p == home => "~".into(),
        Some(home) if p.starts_with(&format!("{home}/")) => format!("~{}", &p[home.len()..]),
        _ => p.to_string(),
    }
}

/// Single-quote a value for a POSIX shell.
pub fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quote an argument only when it needs it — keeps dry-run output readable.
/// A leading `=` is quoted too: zsh expands `=word` to a command path.
pub fn shq_min(s: &str) -> String {
    if !s.is_empty()
        && !s.starts_with('=')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/@%+,:=-".contains(c))
    {
        s.to_string()
    } else {
        shq(s)
    }
}

/// A path for a remote shell: `~` / `~/x` becomes `"$HOME"/x` so it expands over
/// there; anything else is quoted.
pub fn remote_path(p: &str) -> String {
    if p == "~" {
        return "\"$HOME\"".into();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return format!("\"$HOME\"/{}", shq_min(rest));
    }
    shq(p)
}

/// Truthy env flag: `1`, `true`, `yes`.
pub fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Is stdin *and* stdout a terminal?
pub fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Short hostname, lowercased (`hostname -s`), used only as a default name.
pub fn short_hostname() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "local".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guard: a unit test's tmux command never goes to the default socket, nor to the server an
    // outer `$TMUX` names. If this fails, tests are poking the user's live tmux sessions.
    #[test]
    fn unit_tests_never_reach_the_users_tmux_server() {
        let cmd = tmux_cmd();
        // The real binary, whatever FLEET_TMUX / the config would pick.
        if let Some(real) = find_binary("tmux") {
            assert_eq!(cmd.get_program(), real.as_os_str());
        }
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[..2], ["-L", test_tmux::SOCKET], "{args:?}");
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let dir = test_tmux::dir().display().to_string();
        assert!(
            envs.contains(&("TMUX_TMPDIR".into(), Some(dir))),
            "{envs:?}"
        );
        assert!(
            envs.contains(&("TMUX".into(), None)),
            "TMUX cleared: {envs:?}"
        );
        assert!(envs.contains(&("TMUX_PANE".into(), None)), "{envs:?}");
        // …and that private server is never running, so nothing a test sends lands anywhere,
        // and no default socket ever appears next to it.
        if find_binary("tmux").is_some() {
            let out = tmux_cmd().arg("list-sessions").output().unwrap();
            assert!(!out.status.success(), "a private server is running");
        }
        let uid_dir = std::fs::read_dir(test_tmux::dir()).unwrap();
        for d in uid_dir.flatten() {
            assert!(
                !d.path().join("default").exists(),
                "default socket appeared"
            );
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(shq("a b"), "'a b'");
        assert_eq!(shq("it's"), r"'it'\''s'");
        assert_eq!(shq_min("plain-word_1"), "plain-word_1");
        assert_eq!(shq_min("two words"), "'two words'");
        assert_eq!(shq_min(""), "''");
        assert_eq!(shq_min("=session:"), "'=session:'");
        assert_eq!(shq_min("a=b"), "a=b");
    }

    #[test]
    fn remote_paths_expand_over_there() {
        assert_eq!(remote_path("~"), "\"$HOME\"");
        assert_eq!(remote_path("~/Code/x"), "\"$HOME\"/Code/x");
        assert_eq!(remote_path("~/a b"), "\"$HOME\"/'a b'");
        assert_eq!(remote_path("/tmp/x"), "'/tmp/x'");
    }

    #[test]
    fn tilde_round_trip() {
        let home = dirs::home_dir().unwrap().display().to_string();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/x"), format!("{home}/x"));
        assert_eq!(expand_tilde("/abs"), "/abs");
        assert_eq!(tildify(&format!("{home}/x/y")), "~/x/y");
        assert_eq!(tildify(&home), "~");
        assert_eq!(tildify("/elsewhere"), "/elsewhere");
        // A sibling directory that merely shares the prefix is not under $HOME.
        assert_eq!(tildify(&format!("{home}x")), format!("{home}x"));
    }

    /// An executable `dir/name` (the dir created).
    fn exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn lookup(path: &Path, home: &Path, path_only: bool) -> Lookup {
        Lookup {
            path: path.into(),
            home: Some(home.into()),
            path_only,
        }
    }

    // A binary only in a fallback dir (`~/.local/bin`) is found, unless FLEET_PATH_ONLY:
    // then PATH alone decides. One on PATH is found either way.
    #[test]
    fn path_only_skips_the_fallback_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, home) = (tmp.path().join("bin"), tmp.path().join("home"));
        let on_path = exe(&bin, "fleet-test-on-path");
        let fallback = exe(&home.join(".local/bin"), "fleet-test-fallback");

        let normal = lookup(&bin, &home, false);
        assert_eq!(normal.find("fleet-test-on-path"), Some(on_path.clone()));
        assert_eq!(normal.find("fleet-test-fallback"), Some(fallback.clone()));
        assert_eq!(
            normal.resolve("fleet-test-fallback", None),
            fallback.display().to_string()
        );

        let only = lookup(&bin, &home, true);
        assert!(only.fallback_dirs().is_empty());
        assert_eq!(only.find("fleet-test-on-path"), Some(on_path));
        assert_eq!(only.find("fleet-test-fallback"), None);
        assert_eq!(
            only.resolve("fleet-test-fallback", None),
            "fleet-test-fallback"
        );
        // An explicit path (FLEET_GH, config `tmux`, …) still wins.
        assert_eq!(
            only.resolve("fleet-test-fallback", Some("/custom/tool")),
            "/custom/tool"
        );
    }

    // What a child process gets as PATH: the fallbacks appended, or PATH untouched.
    #[test]
    fn path_only_search_path_is_path_as_is() {
        let (path, home) = (Path::new("/a:/b"), Path::new("/h"));
        assert_eq!(
            lookup(path, home, false).search_path(),
            OsString::from(
                "/a:/b:/opt/homebrew/bin:/usr/local/bin:/h/.local/bin:/h/.cargo/bin:/h/bin"
            )
        );
        assert_eq!(
            lookup(path, home, true).search_path(),
            OsString::from("/a:/b")
        );
        assert_eq!(
            lookup(Path::new(""), home, true).search_path(),
            OsString::new()
        );
    }

    // A hard-coded install location (an app bundle, `~/.claude/local`) is tried when lookup
    // fails — except under FLEET_PATH_ONLY.
    #[test]
    fn path_only_skips_install_locations() {
        let loc = "/Applications/Fleet-Test.app/Contents/MacOS/fleet-test";
        let (bin, home) = (
            Path::new("/nonexistent/bin"),
            Path::new("/nonexistent/home"),
        );
        assert_eq!(lookup(bin, home, false).install_location(loc), Some(loc));
        assert_eq!(lookup(bin, home, true).install_location(loc), None);
    }

    // A pane fleet opens under FLEET_PATH_ONLY starts with fleet's own PATH and the switch
    // (tmux `-e`); without it nothing is added, and the pane gets tmux's environment.
    #[test]
    fn path_only_panes_carry_path_and_the_switch() {
        let home = Path::new("/h");
        assert!(
            lookup(Path::new("/a:/b"), home, false)
                .pane_env()
                .is_empty()
        );
        assert_eq!(
            lookup(Path::new("/a:/b"), home, true).pane_env(),
            ["PATH=/a:/b", "FLEET_PATH_ONLY=1"]
        );
        // An empty PATH goes in empty, not left to the server's.
        assert_eq!(
            lookup(Path::new(""), home, true).pane_env(),
            ["PATH=", "FLEET_PATH_ONLY=1"]
        );
    }

    #[test]
    fn path_only_refusals_are_usage_errors() {
        assert!(path_only_refusal(false, "the iTerm backend").is_ok());
        let e = path_only_refusal(true, "the iTerm backend").unwrap_err();
        assert_eq!(e.kind(), "usage");
        assert_eq!(e.code(), 1);
        assert_eq!(
            e.to_string(),
            "the iTerm backend is not available with FLEET_PATH_ONLY set"
        );
        assert!(is_refusal(&e));
        // Another usage error is not one.
        assert!(!is_refusal(&crate::error::Error::tagged(
            "usage",
            "--after and --before cannot be used together"
        )));
    }

    // Under FLEET_PATH_ONLY a PATH that is unset or empty is refused: a bare-name exec would
    // fall back to libc's default path. Without the switch, anything goes.
    #[test]
    fn path_only_needs_a_path() {
        use std::ffi::OsStr;
        for path in [None, Some(OsStr::new(""))] {
            assert!(path_check(false, path).is_ok());
            let e = path_check(true, path).unwrap_err();
            assert_eq!(e.kind(), "usage");
            assert!(is_refusal(&e), "{e}");
        }
        assert!(path_check(true, Some(OsStr::new("/t/bin"))).is_ok());
        assert!(path_check(true, Some(OsStr::new("/t/bin:/usr/bin"))).is_ok());
    }

    // Under FLEET_PATH_ONLY every PATH entry must be absolute: a relative or empty one (the
    // current directory) would resolve against each pane's own directory. Without the
    // switch, anything goes.
    #[test]
    fn path_only_refuses_a_relative_path_entry() {
        use std::ffi::OsStr;
        for (path, named) in [
            ("bin", "`bin`"),
            ("/t/bin:./bin", "`./bin`"),
            ("/t/bin:../bin:/usr/bin", "`../bin`"),
            ("/t/bin::/usr/bin", "an empty entry"),
            (":/t/bin", "an empty entry"),
            ("/t/bin:", "an empty entry"),
        ] {
            let path_os = Some(OsStr::new(path));
            assert!(path_check(false, path_os).is_ok(), "{path}");
            let e = path_check(true, path_os).unwrap_err();
            assert_eq!(e.kind(), "usage", "{path}");
            assert!(is_refusal(&e), "{e}");
            assert!(e.to_string().contains(named), "{path}: {e}");
            assert!(e.to_string().contains("not an absolute directory"), "{e}");
        }
    }

    // A spawned pane starts with `/bin/sh`, or under FLEET_PATH_ONLY the `sh` on PATH (none
    // there: none at all), never one PATH does not hold.
    #[test]
    fn path_only_panes_start_with_the_sh_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, home) = (tmp.path().join("bin"), tmp.path().join("home"));
        let sh = exe(&bin, "sh");
        assert_eq!(
            lookup(&bin, &home, false).pane_sh().as_deref(),
            Some("/bin/sh")
        );
        assert_eq!(
            lookup(&bin, &home, true).pane_sh(),
            Some(sh.display().to_string())
        );
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(lookup(&empty, &home, true).pane_sh(), None);
    }
}
