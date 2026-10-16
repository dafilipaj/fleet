//! Small environment helpers: locating binaries, expanding `~`, shell quoting.
//!
//! A `fleet` started over ssh runs under a non-login shell whose PATH often lacks
//! Homebrew and `~/.local/bin`, so every external tool is resolved here, with the
//! usual install locations as fallbacks, instead of trusting a bare name.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directories searched after `$PATH`.
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join("bin"));
    }
    dirs
}

/// `$PATH` with the fallback dirs appended — for a child that itself looks things up
/// (pi's launcher is a script that runs `node`).
pub fn search_path() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::env::split_paths(&path).chain(fallback_dirs())).unwrap_or(path)
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// First executable called `name` on `$PATH`, then in the fallback dirs.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(fallback_dirs())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// `explicit` (a configured path, `~` allowed) when set, else [`find_binary`],
/// else the bare name so the spawn reports a clear "not found".
pub fn resolve_binary(name: &str, explicit: Option<&str>) -> String {
    if let Some(e) = explicit.map(str::trim).filter(|e| !e.is_empty()) {
        return expand_tilde(e);
    }
    find_binary(name)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| name.to_string())
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
            dirs::home_dir()
                .map(|h| h.join(".claude/local/claude"))
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
}
