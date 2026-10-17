//! Host resolution and dispatch over ssh.
//!
//! The model: every command runs *locally* on the machine that owns the thing
//! being acted on. When the target host is another machine, `fleet` re-invokes
//! itself there — `ssh <dest> fleet --local <same args>` — with the terminal
//! handed over when there is one. The remote side therefore needs `fleet`
//! installed (`fleet install --host <name>`), but nothing else about the remote
//! shell (quoting, PATH, tmux version quirks) leaks into the caller.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::core::config::Config;
use crate::core::tools::{env_flag, shq, shq_min};
use crate::error::{Error, Result};

static AS_HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record the name the caller knows this machine by (`--as-host`, passed by the
/// dispatcher so rows come back tagged with the caller's host name).
pub fn set_as_host(name: &str) {
    let name = name.trim();
    if !name.is_empty() {
        let _ = AS_HOST.set(name.to_string());
    }
}

pub fn as_host() -> Option<String> {
    AS_HOST.get().cloned()
}

/// Where a command should run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// This machine. `name` is its host name (config `self`, or `local`).
    Local {
        name: String,
    },
    Remote(Remote),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// Host name as configured (or the ad-hoc destination itself).
    pub name: String,
    /// ssh destination.
    pub dest: String,
    /// `fleet` path on that host, if configured.
    pub fleet_bin: Option<String>,
}

impl Target {
    pub fn name(&self) -> &str {
        match self {
            Target::Local { name } => name,
            Target::Remote(r) => &r.name,
        }
    }
    pub fn is_local(&self) -> bool {
        matches!(self, Target::Local { .. })
    }
    /// "this machine" / the host name — for messages.
    pub fn label(&self) -> String {
        match self {
            Target::Local { .. } => "this machine".into(),
            Target::Remote(r) => r.name.clone(),
        }
    }
}

/// What a command defaults to when no host is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Claude-session and machine commands: this machine.
    SelfHost,
    /// tmux-session commands: the config's `defaultHost`.
    DefaultHost,
}

/// Names that always mean "this machine".
fn is_local_alias(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(), "local" | "localhost" | "self")
}

/// Pick the target for one invocation.
///
/// `requested` is `--host` (or `$FLEET_HOST`). An unconfigured name is taken as
/// an ssh destination as-is, so `fleet -H some-alias tmux list` works before
/// `fleet init` has ever run. Under `FLEET_PATH_ONLY` another host is refused (kind
/// `usage`): every remote target is made here, so nothing is forwarded over ssh.
pub fn resolve(
    cfg: &Config,
    requested: Option<&str>,
    force_local: bool,
    scope: Scope,
) -> Result<Target> {
    let me = cfg.self_name();
    if force_local {
        return Ok(Target::Local { name: me });
    }
    let name = match requested.map(str::trim) {
        Some("") => return Err(Error::Other("--host is empty".into())),
        Some(n) => n.to_string(),
        None => match scope {
            Scope::SelfHost => me.clone(),
            Scope::DefaultHost => cfg.default_host(),
        },
    };
    if name == me || is_local_alias(&name) {
        return Ok(Target::Local { name: me });
    }
    crate::core::tools::deny_path_only(&format!("another host (`{name}`)"))?;
    match cfg.host(&name) {
        Some(h) => {
            let dest = h
                .ssh
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::Other(format!(
                        "host '{name}' has no ssh destination — set it with: fleet config set hosts.{name}.ssh <alias>"
                    ))
                })?
                .to_string();
            Ok(Target::Remote(Remote {
                name,
                dest,
                fleet_bin: h.fleet_bin.filter(|b| !b.trim().is_empty()),
            }))
        }
        None if cfg.hosts.contains_key(&name) => Err(Error::Other(format!(
            "host '{name}' is malformed in the config — see: fleet config show"
        ))),
        None => Ok(Target::Remote(Remote {
            dest: name.clone(),
            name,
            fleet_bin: None,
        })),
    }
}

/// Every configured host as a target, self first.
pub fn all_targets(cfg: &Config) -> Vec<Result<Target>> {
    cfg.ssh_host_names()
        .iter()
        .map(|n| resolve(cfg, Some(n), false, Scope::SelfHost))
        .collect()
}

// --- ssh ---------------------------------------------------------------------

pub fn connect_timeout() -> u64 {
    std::env::var("FLEET_CONNECT_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(5)
}

/// Whether ssh connection multiplexing is on (`FLEET_MUX=0` turns it off).
pub fn mux_enabled() -> bool {
    std::env::var("FLEET_MUX")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
        && dirs::home_dir().is_some_and(|h| h.join(".ssh").is_dir())
}

/// The control socket pattern (ssh expands `%C`).
pub fn mux_path() -> String {
    format!(
        "{}/.ssh/fleet-mux-%C",
        dirs::home_dir().unwrap_or_default().display()
    )
}

/// Options every ssh/scp/rsync hop shares: a connect timeout, keepalives, and
/// (unless `FLEET_MUX=0`) a shared master connection so repeated hops are cheap.
pub fn ssh_opts() -> Vec<String> {
    let mut o = vec![
        "-o".into(),
        format!("ConnectTimeout={}", connect_timeout()),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ];
    if mux_enabled() {
        o.extend([
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", mux_path()),
            "-o".into(),
            "ControlPersist=60".into(),
        ]);
    }
    o
}

/// How the remote end gets a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tty {
    /// `-t` when both our stdin and stdout are terminals, else `-T` + BatchMode.
    Auto,
    /// Capture-safe: no pty, never prompts.
    Never,
}

/// `ssh <opts> -t|-T <dest> <remote_cmd>`.
pub fn ssh_command(dest: &str, remote_cmd: &str, tty: Tty) -> Command {
    let mut c = Command::new("ssh");
    c.args(ssh_opts());
    let want_tty = tty == Tty::Auto && crate::core::tools::interactive();
    if want_tty {
        c.arg("-t");
    } else {
        c.args(["-T", "-o", "BatchMode=yes"]);
    }
    c.arg(dest).arg(remote_cmd);
    c
}

/// The shell snippet that runs `fleet --local <args>` on the remote host.
///
/// Prefers the configured `fleetBin`, then `~/.local/bin/fleet` (where
/// `fleet install` puts it — checked before PATH because other tools are
/// called `fleet` too), then PATH.
///
/// The script is wrapped in `sh -c` so it means the same thing whatever the
/// remote login shell is.
pub fn remote_fleet_command(r: &Remote, args: &[String]) -> String {
    format!("sh -c {}", shq(&remote_fleet_script(r, args)))
}

/// Env vars that carry over to the remote `fleet`: the launcher, the job run a spawn
/// belongs to, its GitHub identity, and the debug/dry-run switches. Not `FLEET_CONFIG` — a
/// local path means nothing on the other machine, which has its own config.
pub const FORWARDED_ENV: [&str; 5] = [
    "FLEET_CMD",
    "FLEET_JOB_RUN",
    "FLEET_GH_IDENTITY",
    "FLEET_DEBUG",
    "FLEET_DRY_RUN",
];

/// The [`FORWARDED_ENV`] vars set (non-empty) in this process.
fn forwarded_env() -> Vec<(&'static str, String)> {
    FORWARDED_ENV
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| (*k, v))
        })
        .collect()
}

pub fn remote_fleet_script(r: &Remote, args: &[String]) -> String {
    remote_fleet_script_with_env(r, args, &forwarded_env())
}

/// [`remote_fleet_script`] with the forwarded env spelled out: each var is an
/// `export K='v';` at the head of the script, single-quoted so the value
/// reaches the remote `fleet` byte for byte.
pub fn remote_fleet_script_with_env(r: &Remote, args: &[String], env: &[(&str, String)]) -> String {
    let mut exports = String::new();
    for (k, v) in env {
        exports.push_str(&format!("export {k}={}; ", shq(v)));
    }
    exports + &remote_fleet_script_bare(r, args)
}

fn remote_fleet_script_bare(r: &Remote, args: &[String]) -> String {
    let mut argv = String::from(" --local --as-host ");
    argv.push_str(&shq_min(&r.name));
    for a in args {
        argv.push(' ');
        argv.push_str(&shq_min(a));
    }
    let missing = shq(&format!(
        "fleet: not installed on {} — run: fleet install --host {}",
        r.name, r.name
    ));
    match &r.fleet_bin {
        Some(bin) => format!(
            "f={}; [ -x \"$f\" ] || {{ echo {missing} >&2; exit 127; }}; exec \"$f\"{argv}",
            crate::core::tools::remote_path(bin)
        ),
        None => format!(
            "if [ -x \"$HOME/.local/bin/fleet\" ]; then exec \"$HOME/.local/bin/fleet\"{argv}; \
elif command -v fleet >/dev/null 2>&1; then exec fleet{argv}; \
else echo {missing} >&2; exit 127; fi"
        ),
    }
}

/// Print a command the way a shell would read it (for dry-run and debug).
pub fn display_command(c: &Command) -> String {
    let mut out = shq_min(&c.get_program().to_string_lossy());
    for a in c.get_args() {
        out.push(' ');
        out.push_str(&shq_min(&a.to_string_lossy()));
    }
    out
}

static DRY_RUN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn dry-run on for this process (`-n` / `--dry-run`).
pub fn set_dry_run() {
    DRY_RUN.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Print what would run instead of running it (`-n`, or `FLEET_DRY_RUN=1`).
pub fn dry_run() -> bool {
    DRY_RUN.load(std::sync::atomic::Ordering::Relaxed) || env_flag("FLEET_DRY_RUN")
}

pub fn debug(msg: &str) {
    if env_flag("FLEET_DEBUG") {
        eprintln!("fleet debug: {msg}");
    }
}

/// The friendly message for an ssh-level failure (exit 255).
pub fn unreachable_message(r: &Remote) -> String {
    format!(
        "cannot reach '{}' (ssh {} failed) — is the network/VPN up?\n    check:  ssh -v {} true",
        r.name, r.dest, r.dest
    )
}

/// Exit code for "the host could not be reached".
pub const EXIT_UNREACHABLE: i32 = 4;

/// Run `fleet <args>` on a remote host with inherited stdio. Returns the exit
/// code to exit with; an ssh failure (255) becomes a message and
/// [`EXIT_UNREACHABLE`] unless `passthrough` (for `exec`, whose code is the
/// user's own).
pub fn run_remote(r: &Remote, args: &[String], passthrough: bool) -> Result<i32> {
    run_remote_shell(r, &remote_fleet_command(r, args), Tty::Auto, passthrough)
}

/// Run a raw shell command on a remote host with inherited stdio.
pub fn run_remote_shell(r: &Remote, script: &str, tty: Tty, passthrough: bool) -> Result<i32> {
    let mut c = ssh_command(&r.dest, script, tty);
    debug(&format!("[{}] {}", r.name, display_command(&c)));
    if dry_run() {
        println!("{}", display_command(&c));
        return Ok(0);
    }
    let status = c
        .status()
        .map_err(|e| Error::Other(format!("cannot run ssh: {e}")))?;
    Ok(exit_code(r, status, passthrough))
}

fn exit_code(r: &Remote, status: ExitStatus, passthrough: bool) -> i32 {
    let code = status.code().unwrap_or(1);
    if code == 255 && !passthrough {
        eprintln!("fleet: {}", unreachable_message(r));
        return EXIT_UNREACHABLE;
    }
    code
}

/// Captured result of a remote hop.
#[derive(Debug)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub elapsed: Duration,
}

impl Captured {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
    /// One line explaining a failed hop.
    pub fn why(&self, r: &Remote) -> String {
        if self.timed_out {
            return format!("timed out after {}s", self.elapsed.as_secs());
        }
        match self.code {
            Some(255) => format!("unreachable (ssh {} failed)", r.dest),
            Some(127) => format!(
                "fleet not installed there — run: fleet install --host {}",
                r.name
            ),
            Some(c) => {
                let err = self.stderr.trim();
                if err.is_empty() {
                    format!("exited {c}")
                } else {
                    format!("exited {c}: {}", err.lines().last().unwrap_or(err))
                }
            }
            None => "killed by a signal".into(),
        }
    }
}

/// Run a command to completion with piped output and a deadline.
pub fn capture(c: Command, timeout: Duration) -> Result<Captured> {
    capture_input(c, None, timeout)
}

/// [`capture`], with `input` fed to the command's stdin (then closed).
pub fn capture_input(mut c: Command, input: Option<&[u8]>, timeout: Duration) -> Result<Captured> {
    let started = Instant::now();
    c.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| {
        Error::Other(format!(
            "cannot run {}: {e}",
            c.get_program().to_string_lossy()
        ))
    })?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&data);
        });
    }
    // Drain both pipes on threads: a large `list --json` would otherwise fill
    // the pipe buffer and deadlock against our poll loop.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let mut timed_out = false;
    let code = loop {
        match child.try_wait()? {
            Some(st) => break st.code(),
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    Ok(Captured {
        code,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
        timed_out,
        elapsed: started.elapsed(),
    })
}

/// `fleet --local <args>` on a remote host, captured (no pty, never prompts).
pub fn capture_remote(r: &Remote, args: &[String], timeout: Duration) -> Result<Captured> {
    let c = ssh_command(&r.dest, &remote_fleet_command(r, args), Tty::Never);
    debug(&format!("[{} capture] {}", r.name, display_command(&c)));
    capture(c, timeout)
}

/// A plain shell command on a remote host, captured.
pub fn capture_shell(r: &Remote, script: &str, timeout: Duration) -> Result<Captured> {
    let c = ssh_command(&r.dest, script, Tty::Never);
    debug(&format!("[{} capture] {}", r.name, display_command(&c)));
    capture(c, timeout)
}

/// Seconds allowed for one captured remote hop (`FLEET_REMOTE_TIMEOUT`).
pub fn remote_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("FLEET_REMOTE_TIMEOUT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(15),
    )
}

// --- argv rewriting ----------------------------------------------------------

/// Strip the host-selection flags from a raw argv (without the program name), so
/// the remote side runs the rest as-is. Stops at `--`.
pub fn strip_host_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    let mut literal = false;
    while i < args.len() {
        let a = &args[i];
        if literal {
            out.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--" {
            literal = true;
            out.push(a.clone());
            i += 1;
            continue;
        }
        if a == "-H" || a == "--host" || a == "--as-host" {
            i += 2;
            continue;
        }
        if a.starts_with("--host=")
            || a.starts_with("--as-host=")
            || a == "--local"
            || a == "-n"
            || a == "--dry-run"
        {
            i += 1;
            continue;
        }
        if let Some(rest) = a.strip_prefix("-H")
            && !rest.is_empty()
            && !a.starts_with("--")
        {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// Make directory arguments mean the same place on the remote host: a value of
/// `--dir` / `-C` under this machine's `$HOME` is rewritten to `~/…` (the
/// remote expands it against its own home). Relative paths are refused — they
/// can only mean something here.
pub fn remap_dir_args(args: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            out.extend(args[i..].iter().cloned());
            break;
        }
        let (flag, value, joined) = if a == "--dir" || a == "-C" {
            match args.get(i + 1) {
                Some(v) => (a.as_str(), v.clone(), false),
                None => {
                    out.push(a.clone());
                    i += 1;
                    continue;
                }
            }
        } else if let Some(v) = a.strip_prefix("--dir=") {
            ("--dir", v.to_string(), true)
        } else {
            out.push(a.clone());
            i += 1;
            continue;
        };
        let mapped = crate::core::tools::tildify(&value);
        if !(mapped.starts_with('/') || mapped == "~" || mapped.starts_with("~/")) {
            return Err(Error::Other(format!(
                "{flag} must be absolute or ~/… when targeting another host (got '{value}')"
            )));
        }
        if joined {
            out.push(format!("--dir={mapped}"));
            i += 1;
        } else {
            out.push(flag.to_string());
            out.push(mapped);
            i += 2;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg() -> Config {
        serde_json::from_value(json!({
            "version": 1,
            "self": "laptop",
            "defaultHost": "workstation",
            "hosts": {
                "laptop": { "ssh": null },
                "workstation": { "ssh": "devbox" },
                "noaddr": { "ssh": null },
                "custom": { "ssh": "me@box", "fleetBin": "~/bin/fleet" }
            }
        }))
        .unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn scope_decides_the_default() {
        let c = cfg();
        assert!(
            resolve(&c, None, false, Scope::SelfHost)
                .unwrap()
                .is_local()
        );
        let t = resolve(&c, None, false, Scope::DefaultHost).unwrap();
        assert_eq!(
            t,
            Target::Remote(Remote {
                name: "workstation".into(),
                dest: "devbox".into(),
                fleet_bin: None
            })
        );
        // --local beats everything.
        assert!(
            resolve(&c, Some("workstation"), true, Scope::DefaultHost)
                .unwrap()
                .is_local()
        );
    }

    #[test]
    fn self_and_aliases_are_local() {
        let c = cfg();
        for n in ["laptop", "local", "localhost", "self"] {
            assert!(
                resolve(&c, Some(n), false, Scope::DefaultHost)
                    .unwrap()
                    .is_local(),
                "{n}"
            );
        }
    }

    #[test]
    fn hosts_without_ssh_are_an_error_and_unknown_names_are_destinations() {
        let c = cfg();
        let e = resolve(&c, Some("noaddr"), false, Scope::SelfHost).unwrap_err();
        assert!(e.to_string().contains("no ssh destination"), "{e}");
        match resolve(&c, Some("adhoc"), false, Scope::SelfHost).unwrap() {
            Target::Remote(r) => assert_eq!((r.name.as_str(), r.dest.as_str()), ("adhoc", "adhoc")),
            t => panic!("{t:?}"),
        }
        assert!(resolve(&c, Some(""), false, Scope::SelfHost).is_err());
    }

    #[test]
    fn no_config_means_everything_is_local() {
        let c = Config::default();
        assert!(
            resolve(&c, None, false, Scope::DefaultHost)
                .unwrap()
                .is_local()
        );
    }

    #[test]
    fn remote_command_prefers_the_installed_binary() {
        let r = Remote {
            name: "workstation".into(),
            dest: "devbox".into(),
            fleet_bin: None,
        };
        let cmd = remote_fleet_script_bare(&r, &s(&["tmux", "enter", "my session"]));
        assert!(
            cmd.contains(
                "\"$HOME/.local/bin/fleet\" --local --as-host workstation tmux enter 'my session'"
            ),
            "{cmd}"
        );
        assert!(cmd.contains("exit 127"), "{cmd}");
        let r = Remote {
            fleet_bin: Some("~/bin/fleet".into()),
            ..r
        };
        let cmd = remote_fleet_script_bare(&r, &s(&["list"]));
        assert!(remote_fleet_command(&r, &s(&["list"])).starts_with("sh -c '"));
        assert!(cmd.starts_with("f=\"$HOME\"/bin/fleet;"), "{cmd}");
    }

    #[test]
    fn remote_script_forwards_env_safely() {
        let r = Remote {
            name: "workstation".into(),
            dest: "devbox".into(),
            fleet_bin: None,
        };
        let env = vec![
            ("FLEET_CMD", "cat #".to_string()),
            ("FLEET_DEBUG", "1".to_string()),
            ("FLEET_DRY_RUN", "it's $(rm -rf ~)".to_string()),
        ];
        let script = remote_fleet_script_with_env(&r, &s(&["list"]), &env);
        assert!(
            script.starts_with(
                "export FLEET_CMD='cat #'; export FLEET_DEBUG='1'; export FLEET_DRY_RUN='it'\\''s $(rm -rf ~)'; if "
            ),
            "{script}"
        );
        assert!(!FORWARDED_ENV.contains(&"FLEET_CONFIG"));
        // Run the exports through a real sh: values arrive verbatim.
        let mut probe = String::new();
        for (k, v) in &env {
            probe.push_str(&format!("export {k}={}; ", shq(v)));
        }
        probe.push_str("printf '%s|%s|%s' \"$FLEET_CMD\" \"$FLEET_DEBUG\" \"$FLEET_DRY_RUN\"");
        let out = Command::new("sh").arg("-c").arg(&probe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "cat #|1|it's $(rm -rf ~)"
        );
        // Nothing to forward: the script is unchanged.
        assert_eq!(
            remote_fleet_script_with_env(&r, &s(&["list"]), &[]),
            remote_fleet_script_bare(&r, &s(&["list"]))
        );
    }

    #[test]
    fn host_flags_are_stripped_but_literals_survive() {
        assert_eq!(
            strip_host_flags(&s(&["-H", "devbox", "tmux", "list"])),
            s(&["tmux", "list"])
        );
        assert_eq!(
            strip_host_flags(&s(&["--host=devbox", "list", "--json", "-Hdevbox"])),
            s(&["list", "--json"])
        );
        assert_eq!(
            strip_host_flags(&s(&["--local", "exec", "--", "echo", "-H", "x"])),
            s(&["exec", "--", "echo", "-H", "x"])
        );
    }

    #[test]
    fn dir_args_are_remapped_under_home() {
        let home = dirs::home_dir().unwrap().display().to_string();
        let out = remap_dir_args(&s(&["spawn", "--dir", &format!("{home}/Code/x"), "go"])).unwrap();
        assert_eq!(out, s(&["spawn", "--dir", "~/Code/x", "go"]));
        let out = remap_dir_args(&s(&["tmux", "new", "-C", "/srv/x"])).unwrap();
        assert_eq!(out, s(&["tmux", "new", "-C", "/srv/x"]));
        let out = remap_dir_args(&s(&[&format!("--dir={home}")])).unwrap();
        assert_eq!(out, s(&["--dir=~"]));
        assert!(remap_dir_args(&s(&["--dir", "relative/x"])).is_err());
        // After `--` nothing is ours to touch.
        let out = remap_dir_args(&s(&["exec", "--", "ls", "-C", "rel"])).unwrap();
        assert_eq!(out, s(&["exec", "--", "ls", "-C", "rel"]));
    }

    #[test]
    fn capture_times_out_and_drains_large_output() {
        let mut c = Command::new("sh");
        c.args(["-c", "yes x | head -c 300000"]);
        let got = capture(c, Duration::from_secs(10)).unwrap();
        assert!(got.ok());
        assert_eq!(got.stdout.len(), 300000);

        let mut c = Command::new("sh");
        c.args(["-c", "sleep 5"]);
        let got = capture(c, Duration::from_millis(200)).unwrap();
        assert!(got.timed_out && !got.ok());
    }
}
