//! Machine-level verbs: `exec`, `ssh`, `doctor`, `list --all-hosts`, `install`,
//! and the hidden `_probe` that `doctor` runs on each host.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::core::config;
use crate::core::discovery::Session;
use crate::core::hosts::{self, Remote, Target, Tty};
use crate::core::tools::{self, remote_path, shq, shq_min};
use crate::error::{Error, Result};

fn ok(msg: &str) {
    println!("  {} {msg}", "✔".green());
}
fn bad(msg: &str) {
    println!("  {} {msg}", "✘".red());
}
fn info(msg: &str) {
    println!("  {msg}");
}

// --- exec / ssh ----------------------------------------------------------------

/// Run a command on a host. Locally through a login shell (so PATH matches an
/// interactive session); remotely over ssh, where the remote login shell does
/// the same. The exit code passes through verbatim.
pub fn exec(target: &Target, dir: Option<&str>, tty: bool, argv: &[String]) -> Result<i32> {
    if argv.is_empty() {
        return Err(Error::exit(
            1,
            "usage: fleet exec [-C <dir>] [-t] -- <cmd...>",
        ));
    }
    let dir = dir.unwrap_or("~");
    match target {
        Target::Local { .. } => {
            // A login shell's profile looks past PATH.
            tools::deny_path_only("`fleet exec`")?;
            let dir = tools::expand_tilde(dir);
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let mut c = Command::new(&shell);
            c.args([
                "-l",
                "-c",
                "cd \"$1\" && shift && exec \"$@\"",
                "fleet-exec",
                &dir,
            ]);
            c.args(argv);
            if hosts::dry_run() {
                println!("{}", hosts::display_command(&c));
                return Ok(0);
            }
            let st = c
                .status()
                .map_err(|e| Error::Other(format!("cannot run {shell}: {e}")))?;
            Ok(st.code().unwrap_or(1))
        }
        Target::Remote(r) => {
            let dir = tools::tildify(dir);
            let dir = dir.as_str();
            if !(dir.starts_with('/') || dir == "~" || dir.starts_with("~/")) {
                return Err(Error::exit(
                    1,
                    format!("--dir must be absolute or ~/… when targeting {}", r.name),
                ));
            }
            let mut s = format!("cd {} && exec", remote_path(dir));
            for a in argv {
                s.push(' ');
                s.push_str(&shq_min(a));
            }
            // No 255 remapping: the code is the command's own.
            hosts::run_remote_shell(r, &s, if tty { Tty::Auto } else { Tty::Never }, true)
        }
    }
}

/// A plain shell (or one command) on a host, no tmux.
pub fn ssh(target: &Target, argv: &[String]) -> Result<i32> {
    match target {
        Target::Local { .. } => {
            // A login shell's profile looks past PATH.
            tools::deny_path_only("`fleet ssh`")?;
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let mut c = Command::new(&shell);
            if argv.is_empty() {
                c.arg("-l");
            } else {
                c.args(["-l", "-c", "exec \"$@\"", "fleet-ssh"]).args(argv);
            }
            if hosts::dry_run() {
                println!("{}", hosts::display_command(&c));
                return Ok(0);
            }
            use std::os::unix::process::CommandExt;
            Err(Error::Other(format!("cannot exec {shell}: {}", c.exec())))
        }
        Target::Remote(r) => {
            let s = if argv.is_empty() {
                "exec \"${SHELL:-/bin/sh}\" -l".to_string()
            } else {
                let mut s = String::from("exec");
                for a in argv {
                    s.push(' ');
                    s.push_str(&shq_min(a));
                }
                s
            };
            if !hosts::dry_run() && !tools::interactive() {
                return Err(Error::exit(
                    3,
                    "'fleet ssh' needs a terminal (stdin/stdout is not a tty)",
                ));
            }
            hosts::run_remote_shell(r, &s, Tty::Auto, false)
        }
    }
}

// --- probe / doctor ------------------------------------------------------------

/// What `fleet _probe` reports about the machine it runs on.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Probe {
    pub fleet_version: String,
    pub fleet_path: Option<String>,
    pub hostname: String,
    pub user: String,
    /// `uname -sm`, e.g. `Darwin arm64`.
    pub platform: String,
    pub tmux: Option<String>,
    pub claude: Option<String>,
    pub node: Option<String>,
    pub tmux_sessions: usize,
    pub claude_sessions: usize,
    pub config_path: String,
    pub config_exists: bool,
    pub config_self: Option<String>,
    pub config_problems: Vec<String>,
    pub web_dir: Option<String>,
}

fn first_line(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout);
    s.lines().next().map(|l| l.trim().to_string())
}

pub fn uname() -> String {
    first_line("uname", &["-sm"]).unwrap_or_default()
}

pub fn gather_probe() -> Probe {
    let loaded = config::load();
    let mut problems = loaded.config.problems();
    if let Some(p) = &loaded.problem {
        problems.insert(0, p.clone());
    }
    if !loaded.exists {
        problems.clear();
    }
    let claude = tools::claude().and_then(|c| first_line(&c, &["--version"]));
    let node = tools::find_binary("node")
        .and_then(|n| first_line(&n.display().to_string(), &["--version"]));
    Probe {
        fleet_version: crate::VERSION.into(),
        fleet_path: std::env::current_exe()
            .ok()
            .map(|p| tools::tildify(&p.display().to_string())),
        hostname: first_line("hostname", &["-s"]).unwrap_or_default(),
        user: std::env::var("USER").unwrap_or_default(),
        platform: uname(),
        tmux: crate::core::tmux::version(),
        claude,
        node,
        tmux_sessions: crate::core::tmux::list_sessions()
            .map(|v| v.len())
            .unwrap_or(0),
        claude_sessions: crate::core::discovery::discover().len(),
        config_path: tools::tildify(&loaded.path.display().to_string()),
        config_exists: loaded.exists,
        config_self: loaded.config.self_name.clone(),
        config_problems: problems,
        web_dir: config::web_dir(&loaded.config).map(|p| tools::tildify(&p.display().to_string())),
    }
}

pub fn probe() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&gather_probe())?);
    Ok(())
}

fn report_probe(p: &Probe, compare: Option<&str>) -> usize {
    let mut failures = 0;
    let fleet = format!(
        "fleet {} ({})",
        p.fleet_version,
        p.fleet_path.as_deref().unwrap_or("?")
    );
    match compare {
        Some(v) if v != p.fleet_version => {
            bad(&format!(
                "{fleet} — differs from this machine's {v}; run: fleet install --host <name>"
            ));
            failures += 1;
        }
        _ => ok(&fleet),
    }
    info(&format!(
        "host: {} ({}, {})",
        p.hostname, p.user, p.platform
    ));
    match &p.tmux {
        Some(t) => ok(&format!("{t} — {} session(s)", p.tmux_sessions)),
        None => {
            bad("tmux: missing");
            failures += 1;
        }
    }
    match &p.claude {
        Some(c) => ok(&format!(
            "claude {c} — {} live session(s)",
            p.claude_sessions
        )),
        None => bad("claude: missing"),
    }
    match &p.node {
        Some(n) => ok(&format!("node {n}")),
        None => info(&format!(
            "{} node: missing (only needed for `fleet web serve`)",
            "·".dimmed()
        )),
    }
    match &p.web_dir {
        Some(d) => ok(&format!("web app: {d}")),
        None => info(&format!(
            "{} web app: not found (only needed for `fleet web serve`)",
            "·".dimmed()
        )),
    }
    if p.config_exists {
        let who = p.config_self.as_deref().unwrap_or("(self unset)");
        if p.config_problems.is_empty() {
            ok(&format!("config {} — self: {who}", p.config_path));
        } else {
            bad(&format!("config {} — self: {who}", p.config_path));
            for pr in &p.config_problems {
                info(&format!("    {pr}"));
            }
            failures += 1;
        }
    } else {
        info(&format!(
            "{} config: none at {} (run: fleet init)",
            "·".dimmed(),
            p.config_path
        ));
    }
    failures
}

/// Reachability, versions on both ends, and config sanity.
pub fn doctor(only: Option<&str>) -> Result<()> {
    let loaded = config::load();
    let cfg = &loaded.config;
    println!("fleet doctor — v{}", crate::VERSION);
    info(&format!(
        "self: {}   default host: {}",
        cfg.self_name(),
        cfg.default_host()
    ));
    if hosts::mux_enabled() {
        info(&format!("mux:  {}", tools::tildify(&hosts::mux_path())));
    } else {
        info("mux:  disabled (FLEET_MUX=0, or no ~/.ssh)");
    }

    let mut failures = 0;
    println!(
        "\n{} {}",
        "●".cyan(),
        format!("{} (this machine)", cfg.self_name()).bold()
    );
    let me = gather_probe();
    failures += report_probe(&me, None);

    let targets: Vec<Target> = match only {
        Some(h) => vec![hosts::resolve(cfg, Some(h), false, hosts::Scope::SelfHost)?],
        None => hosts::all_targets(cfg)
            .into_iter()
            .filter_map(|t| match t {
                Ok(t) => Some(t),
                // FLEET_PATH_ONLY: not checked, which is no problem with the host.
                Err(e) if tools::is_refusal(&e) => {
                    info(&format!("{} {e}", "·".dimmed()));
                    None
                }
                Err(e) => {
                    bad(&e.to_string());
                    failures += 1;
                    None
                }
            })
            .collect(),
    };
    for t in targets {
        let Target::Remote(r) = t else { continue };
        println!(
            "\n{} {}",
            "●".cyan(),
            format!("{} (ssh {})", r.name, r.dest).bold()
        );
        failures += doctor_remote(&r, &me.fleet_version);
    }
    if failures > 0 {
        return Err(Error::exit(1, format!("{failures} problem(s) found")));
    }
    Ok(())
}

fn doctor_remote(r: &Remote, local_version: &str) -> usize {
    let t0 = Instant::now();
    let reach = hosts::capture_shell(r, "true", Duration::from_secs(hosts::connect_timeout() + 5));
    match reach {
        Ok(c) if c.ok() => ok(&format!("reached in {}ms", t0.elapsed().as_millis())),
        Ok(c) => {
            bad(&c.why(r));
            let err = c.stderr.trim();
            if !err.is_empty() {
                info(&format!(
                    "    {}",
                    err.lines().last().unwrap_or(err).dimmed()
                ));
            }
            info(&format!("    check:  ssh -v {} true", r.dest));
            return 1;
        }
        Err(e) => {
            bad(&e.to_string());
            return 1;
        }
    }
    match hosts::capture_remote(r, &["_probe".to_string()], hosts::remote_timeout()) {
        Ok(c) if c.ok() => match serde_json::from_str::<Probe>(&c.stdout) {
            Ok(p) => report_probe(&p, Some(local_version)),
            Err(e) => {
                bad(&format!(
                    "fleet there answered, but not with a probe ({e}) — version too old?"
                ));
                1
            }
        },
        Ok(c) => {
            bad(&c.why(r));
            1
        }
        Err(e) => {
            bad(&e.to_string());
            1
        }
    }
}

// --- list --all-hosts ------------------------------------------------------------

/// One host's answer to `list --json`.
pub struct HostRows {
    pub host: String,
    pub rows: std::result::Result<Vec<Session>, String>,
}

/// `list --json` on every configured host, in parallel, rows tagged with the
/// host name *this* config knows it by.
pub fn gather_all_hosts() -> Vec<HostRows> {
    let cfg = config::get();
    let names = cfg.ssh_host_names();
    let handles: Vec<_> = names
        .into_iter()
        .map(|name| {
            let target = hosts::resolve(cfg, Some(&name), false, hosts::Scope::SelfHost);
            std::thread::spawn(move || {
                let rows = match target {
                    Err(e) => Err(e.to_string()),
                    Ok(Target::Local { .. }) => Ok(crate::cli::commands::list_rows()),
                    Ok(Target::Remote(r)) => {
                        match hosts::capture_remote(
                            &r,
                            &["list".into(), "--json".into()],
                            hosts::remote_timeout(),
                        ) {
                            Err(e) => Err(e.to_string()),
                            Ok(c) if !c.ok() => Err(c.why(&r)),
                            Ok(c) => serde_json::from_str::<Vec<Session>>(&c.stdout)
                                .map_err(|e| format!("bad JSON from {}: {e}", r.name)),
                        }
                    }
                };
                let rows = rows.map(|mut v| {
                    for s in v.iter_mut() {
                        s.host = Some(name.clone());
                    }
                    v
                });
                HostRows { host: name, rows }
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

pub fn list_all_hosts(json: bool) -> Result<()> {
    let all = gather_all_hosts();
    let mut any_ok = false;
    let mut rows = Vec::new();
    for h in &all {
        match &h.rows {
            Ok(v) => {
                any_ok = true;
                if !json {
                    println!(
                        "{}\n",
                        crate::cli::render::plain_table_titled(v, Some(&h.host))
                    );
                }
                rows.extend(v.iter().cloned());
            }
            Err(e) => {
                eprintln!("{} {}: {e}", "fleet:".red(), h.host);
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    }
    if !any_ok {
        return Err(Error::exit(hosts::EXIT_UNREACHABLE, "no host answered"));
    }
    Ok(())
}

// --- install -------------------------------------------------------------------

/// Where `fleet install` puts the binary on a host.
pub const INSTALLED_BIN: &str = "~/.local/bin/fleet";

fn run_step(what: &str, mut c: Command) -> Result<()> {
    if hosts::dry_run() {
        println!("{}", hosts::display_command(&c));
        return Ok(());
    }
    hosts::debug(&hosts::display_command(&c));
    let st = c.status().map_err(|e| {
        Error::Other(format!(
            "{what}: cannot run {}: {e}",
            c.get_program().to_string_lossy()
        ))
    })?;
    if !st.success() {
        return Err(Error::Other(format!("{what} failed ({st})")));
    }
    Ok(())
}

fn remote_step(r: &Remote, what: &str, script: &str) -> Result<String> {
    if hosts::dry_run() {
        println!(
            "{}",
            hosts::display_command(&hosts::ssh_command(&r.dest, script, Tty::Never))
        );
        return Ok(String::new());
    }
    let c = hosts::capture_shell(r, script, hosts::remote_timeout())?;
    if !c.ok() {
        let err = c.stderr.trim();
        return Err(Error::Other(format!(
            "{what} on {}: {}{}",
            r.name,
            c.why(r),
            if err.is_empty() || c.code == Some(255) {
                String::new()
            } else {
                format!("\n    {err}")
            }
        )));
    }
    Ok(c.stdout)
}

/// Copy a directory's contents to `dest:remote_dir`, skipping tests, VCS and
/// dependencies anywhere, plus the top-level entries in `skip_top` (which are also
/// left alone on the remote). rsync when available (so removed files go too), tar
/// over ssh otherwise.
fn sync_dir(r: &Remote, local: &Path, remote_dir: &str, skip_top: &[&str]) -> Result<()> {
    const EXCLUDES: [&str; 4] = ["node_modules", "tests", ".git", ".DS_Store"];
    if tools::find_binary("rsync").is_some() {
        let mut c = Command::new("rsync");
        c.args(["-az", "--delete"]);
        for e in EXCLUDES {
            c.arg(format!("--exclude={e}"));
        }
        // A leading `/` anchors the pattern at the transfer root.
        for e in skip_top {
            c.arg(format!("--exclude=/{e}"));
        }
        let ssh_e = std::iter::once("ssh".to_string())
            .chain(hosts::ssh_opts().iter().map(|o| shq_min(o)))
            .collect::<Vec<_>>()
            .join(" ");
        c.args(["-e", &ssh_e]);
        c.arg(format!("{}/", local.display()));
        // rsync paths are relative to the remote home.
        let rel = remote_dir.strip_prefix("~/").unwrap_or(remote_dir);
        c.arg(format!("{}:{rel}/", r.dest));
        return run_step("rsync", c);
    }
    let mut excl = String::new();
    for e in EXCLUDES {
        excl.push_str(&format!(" --exclude={}", shq(e)));
    }
    // tar's --exclude is unanchored in bsdtar, so top-level skips are done by naming
    // the entries to pack instead of `.`.
    let entries = if skip_top.is_empty() {
        ".".to_string()
    } else {
        top_entries(local, skip_top)?
            .iter()
            .map(|e| shq(e))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let script = format!(
        "tar -C {} -cf -{excl} {entries} | ssh {} {} {}",
        shq(&local.display().to_string()),
        hosts::ssh_opts()
            .iter()
            .map(|o| shq_min(o))
            .collect::<Vec<_>>()
            .join(" "),
        shq(&r.dest),
        shq(&format!(
            "rm -rf {d} && mkdir -p {d} && tar -C {d} -xf -",
            d = remote_path(remote_dir)
        ))
    );
    let mut c = Command::new("sh");
    c.args(["-c", &script]);
    run_step("copy web app", c)
}

/// `dir`'s top-level entry names minus `skip`, sorted (for the tar fallback).
fn top_entries(dir: &Path, skip: &[&str]) -> Result<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !skip.contains(&n.as_str()))
        .collect();
    out.sort();
    Ok(out)
}

/// The built React UI inside a web dir (`npm --prefix web/ui run build`), if built.
pub fn built_ui(web: &Path) -> Option<PathBuf> {
    let dist = web.join("ui").join("dist");
    dist.join("index.html").is_file().then_some(dist)
}

pub const UI_BUILD_HINT: &str =
    "npm --prefix web/ui ci && npm --prefix web/ui run build   (or: fleet web build)";

/// Copy the web app: the server, then only the *built* React UI
/// (`ui/dist`) — never its sources or node_modules.
fn install_web(r: &Remote, dir: &Path) -> Result<()> {
    let remote_ui = format!("{}/ui", config::INSTALLED_WEB_DIR);
    let remote_dist = format!("{remote_ui}/dist");
    let dist = built_ui(dir);
    // rsync needs the destination's parent; a stale remote ui/ would keep serving an old build.
    let prep = match &dist {
        Some(_) => format!("mkdir -p {}", remote_path(&remote_ui)),
        None => format!("rm -rf {}", remote_path(&remote_ui)),
    };
    remote_step(
        r,
        "prepare web dir",
        &format!(
            "mkdir -p {} && {prep}",
            remote_path(config::INSTALLED_WEB_DIR)
        ),
    )?;
    sync_dir(r, dir, config::INSTALLED_WEB_DIR, &["ui"])?;
    match &dist {
        Some(d) => sync_dir(r, d, &remote_dist, &[])?,
        None => eprintln!(
            "fleet: the web UI is not built ({}/ui/dist missing) — {} gets no UI until it is; build it with:\n  {UI_BUILD_HINT}",
            tools::tildify(&dir.display().to_string()),
            r.name
        ),
    }
    if !hosts::dry_run() {
        println!(
            "  {} {} (from {}{})",
            "✔".green(),
            config::INSTALLED_WEB_DIR,
            tools::tildify(&dir.display().to_string()),
            if dist.is_some() {
                ", with ui/dist"
            } else {
                ", no UI (not built)"
            }
        );
    }
    Ok(())
}

pub struct InstallOpts {
    pub web: bool,
    pub force: bool,
}

/// Copy this binary (and the web app) to a remote host's `~/.local`.
pub fn install(target: &Target, o: InstallOpts) -> Result<()> {
    let Target::Remote(r) = target else {
        return Err(Error::exit(
            1,
            "install copies this binary to another host — pass --host <name> (a configured remote host)",
        ));
    };
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    // Same OS and CPU, or the copy is useless.
    let local = uname();
    let remote = remote_step(r, "uname", "uname -sm")?;
    let remote = remote.trim();
    if !hosts::dry_run() && remote != local {
        if o.force {
            eprintln!(
                "fleet: {} is {remote}, this machine is {local} — installing anyway (--force)",
                r.name
            );
        } else {
            return Err(Error::exit(
                1,
                format!(
                    "{} is {remote} but this binary is built for {local} — build fleet for that platform there (cargo install --path crates/fleet), or --force",
                    r.name
                ),
            ));
        }
    }
    println!(
        "→ installing fleet {} on {} ({})",
        crate::VERSION,
        r.name,
        if remote.is_empty() { &local } else { remote }
    );

    remote_step(
        r,
        "mkdir",
        "mkdir -p \"$HOME/.local/bin\" \"$HOME/.local/share/fleet\"",
    )?;
    let mut scp = Command::new("scp");
    scp.args(hosts::ssh_opts()).arg("-q").arg(&exe);
    scp.arg(format!("{}:.local/bin/fleet.new", r.dest));
    run_step("scp", scp)?;
    let version = remote_step(
        r,
        "activate",
        "chmod 755 \"$HOME/.local/bin/fleet.new\" && mv -f \"$HOME/.local/bin/fleet.new\" \"$HOME/.local/bin/fleet\" && \"$HOME/.local/bin/fleet\" --version",
    )?;
    if !hosts::dry_run() {
        println!("  {} {INSTALLED_BIN} — {}", "✔".green(), version.trim());
    }

    if o.web {
        match config::web_dir(config::get()) {
            Some(dir) => install_web(r, &dir)?,
            None => eprintln!(
                "fleet: web app not found here (set web.dir, or run from a checkout) — skipped"
            ),
        }
    }

    if hosts::dry_run() {
        return Ok(());
    }
    // Is it usable over there? The probe says whether it has a config yet.
    match hosts::capture_remote(r, &["_probe".to_string()], hosts::remote_timeout()) {
        Ok(c) if c.ok() => {
            if let Ok(p) = serde_json::from_str::<Probe>(&c.stdout) {
                if !p.config_exists {
                    println!(
                        "  {} no config on {} yet — run: fleet -H {} init",
                        "·".dimmed(),
                        r.name,
                        r.name
                    );
                }
                if p.tmux.is_none() {
                    println!("  {} tmux is missing on {}", "✘".red(), r.name);
                }
            }
        }
        Ok(c) => eprintln!(
            "fleet: installed, but the remote probe failed: {}",
            c.why(r)
        ),
        Err(e) => eprintln!("fleet: installed, but the remote probe failed: {e}"),
    }
    println!("  next: fleet doctor --host {}", r.name);
    Ok(())
}

/// For `web install-service`: the absolute path of this binary.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}
