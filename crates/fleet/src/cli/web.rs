//! `fleet web serve` / `fleet web build` / `fleet web install-service`: run the web UI
//! (`<web.dir>/server.mjs`, Node ≥ 22) against the same config file, and build its
//! React UI (`<web.dir>/ui` → `ui/dist`, which the server prefers when present).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::config;
use crate::core::hosts;
use crate::core::tools;
use crate::error::{Error, Result};

pub struct ServeOpts {
    pub port: Option<u16>,
    pub bind: Option<String>,
    pub dir: Option<String>,
}

fn web_dir(explicit: Option<&str>) -> Result<PathBuf> {
    let dir = match explicit {
        Some(d) => PathBuf::from(tools::expand_tilde(d)),
        None => config::web_dir(config::get()).ok_or_else(|| {
            Error::exit(
                1,
                "web app not found — set it with: fleet config set web.dir <path/to/web> (or run from a repo checkout)",
            )
        })?,
    };
    if !dir.join("server.mjs").is_file() {
        return Err(Error::exit(
            1,
            format!("no server.mjs in {}", dir.display()),
        ));
    }
    Ok(dir)
}

pub struct BuildOpts {
    pub dir: Option<String>,
    /// Run `npm ci` even when `ui/node_modules` exists.
    pub install: bool,
}

/// The commands `web build` runs, in order: `npm ci` (when needed), then `npm run build`.
pub fn build_commands(o: &BuildOpts) -> Result<Vec<Command>> {
    let dir = web_dir(o.dir.as_deref())?;
    let ui = dir.join("ui");
    if !ui.join("package.json").is_file() {
        return Err(Error::exit(
            1,
            format!(
                "no UI sources in {} — an installed web dir only has the built UI; build in a repo checkout (then `fleet install --host <name>`)",
                ui.display()
            ),
        ));
    }
    let npm = tools::find_binary("npm").ok_or_else(|| {
        Error::exit(
            127,
            "npm not found (building the web UI needs Node ≥ 22 with npm)",
        )
    })?;
    let npm_in = |args: &[&str]| {
        let mut c = Command::new(&npm);
        c.arg("--prefix").arg(&ui).args(args);
        c
    };
    let mut out = Vec::new();
    if o.install || !ui.join("node_modules").is_dir() {
        out.push(npm_in(&["ci"]));
    }
    out.push(npm_in(&["run", "build"]));
    Ok(out)
}

pub fn build(o: BuildOpts) -> Result<()> {
    for mut c in build_commands(&o)? {
        if hosts::dry_run() {
            println!("{}", hosts::display_command(&c));
            continue;
        }
        hosts::debug(&hosts::display_command(&c));
        let st = c.status()?;
        if !st.success() {
            return Err(Error::exit(
                st.code().unwrap_or(1),
                format!("{} failed ({st})", hosts::display_command(&c)),
            ));
        }
    }
    Ok(())
}

/// Where a version manager keeps per-version installs: a path that changes (or
/// vanishes) on the next `nvm install`, so a poor thing to bake into a service.
pub fn is_version_managed(p: &Path) -> bool {
    let s = p.display().to_string();
    [
        "/.nvm/",
        "/.volta/",
        "/.fnm/",
        "/fnm/node-versions/",
        "/fnm_multishells/",
        "/.asdf/",
        "/.local/share/mise/",
    ]
    .iter()
    .any(|m| s.contains(m))
}

/// The node that runs the web app: `FLEET_NODE` / config `web.node`, then a stable install
/// (`stable`, i.e. Homebrew / `/usr/local`), then whatever PATH finds.
/// The bool is true when the pick lives under a version manager.
pub fn pick_node(
    configured: Option<&str>,
    stable: &[&Path],
    on_path: Option<PathBuf>,
) -> Option<(PathBuf, bool)> {
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        return Some((PathBuf::from(tools::expand_tilde(c)), false));
    }
    if let Some(p) = stable.iter().find(|p| tools::is_executable(p)) {
        return Some((p.to_path_buf(), false));
    }
    on_path.map(|p| {
        let managed = is_version_managed(&p);
        (p, managed)
    })
}

const STABLE_NODES: [&str; 2] = ["/opt/homebrew/bin/node", "/usr/local/bin/node"];

fn node_pick() -> Result<(PathBuf, bool)> {
    let stable: Vec<&Path> = STABLE_NODES.iter().map(Path::new).collect();
    // `FLEET_NODE` is what the launchd agent pins; `web.node` is the user's override.
    let env = std::env::var("FLEET_NODE").ok();
    pick_node(
        env.as_deref().or(config::get().web.node.as_deref()),
        &stable,
        tools::find_binary("node"),
    )
    .ok_or_else(|| {
        Error::exit(
            127,
            "node not found (the web UI needs Node ≥ 22) — install it or set: fleet config set web.node <path>",
        )
    })
}

fn node() -> Result<String> {
    Ok(node_pick()?.0.display().to_string())
}

/// The command `serve` runs.
pub fn serve_command(o: &ServeOpts) -> Result<Command> {
    let dir = web_dir(o.dir.as_deref())?;
    let mut c = Command::new(node()?);
    c.arg(dir.join("server.mjs")).current_dir(&dir);
    c.env("FLEET_CONFIG", config::path());
    if let Some(p) = o.port {
        c.env("FLEET_WEB_PORT", p.to_string());
    }
    if let Some(b) = &o.bind {
        c.env("FLEET_WEB_BIND", b);
    }
    // The server shells out to `fleet list --json`: point it at this binary.
    if std::env::var_os("FLEET_BIN").is_none()
        && config::get().fleet_bin.is_none()
        && let Ok(exe) = crate::cli::hosts::current_exe()
    {
        c.env("FLEET_BIN", exe);
    }
    Ok(c)
}

pub fn serve(o: ServeOpts) -> Result<()> {
    // The server puts Homebrew and `~/.local/bin` on its PATH and drives iTerm.
    tools::deny_path_only("`fleet web serve`")?;
    let mut c = serve_command(&o)?;
    if hosts::dry_run() {
        let envs: Vec<String> = c
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    format!(
                        "{}={}",
                        k.to_string_lossy(),
                        tools::shq_min(&v.to_string_lossy())
                    )
                })
            })
            .collect();
        println!("{} {}", envs.join(" "), hosts::display_command(&c));
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    Err(Error::Other(format!("cannot exec node: {}", c.exec())))
}

pub const LAUNCHD_LABEL: &str = "fleet.web";
/// The service's log, under `$HOME`. Named after the label, so it can't clash
/// with a `fleet-web.log` left by an older standalone install.
pub const SERVICE_LOG: &str = "Library/Logs/fleet.web.log";

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The launchd agent that keeps `fleet web serve` running.
pub fn launchd_plist(
    exe: &Path,
    config_path: &Path,
    node: &Path,
    path_env: &str,
    log: &Path,
) -> String {
    let e = |p: &Path| xml_escape(&p.display().to_string());
    // ProcessType=Interactive: without it launchd schedules the agent (and every `ps`/`tmux`
    // it spawns) as a background job and macOS throttles them — with Low Power Mode on,
    // `fleet list --json` took 11s instead of 0.3s and every host showed as unreachable.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>web</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>FLEET_CONFIG</key><string>{cfg}</string>
    <key>FLEET_NODE</key><string>{node}</string>
    <key>PATH</key><string>{path}</string>
  </dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = e(exe),
        cfg = e(config_path),
        node = e(node),
        path = xml_escape(path_env),
        log = e(log),
    )
}

/// A systemd user unit, printed for non-macOS hosts.
pub fn systemd_unit(exe: &Path, config_path: &Path) -> String {
    format!(
        "[Unit]\nDescription=fleet web UI\nAfter=network-online.target\n\n[Service]\nExecStart={} web serve\nEnvironment=FLEET_CONFIG={}\nRestart=always\n\n[Install]\nWantedBy=default.target\n",
        exe.display(),
        config_path.display()
    )
}

/// PATH for the service: the chosen node's dir, where tmux/claude/fleet live,
/// then the basics. Version-manager dirs are left out (they go stale on the next
/// upgrade) — except the node's own, when that is the only node there is.
pub fn service_path(node: &Path, bins: &[Option<PathBuf>]) -> String {
    let mut dirs: Vec<String> = Vec::new();
    let mut push = |d: &Path| {
        let d = d.display().to_string();
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(d) = node.parent() {
        push(d);
    }
    for p in bins.iter().flatten() {
        if let Some(d) = p.parent().filter(|_| !is_version_managed(p)) {
            push(d);
        }
    }
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        if !dirs.iter().any(|x| x == d) {
            dirs.push(d.into());
        }
    }
    dirs.join(":")
}

pub struct ServiceOpts {
    pub uninstall: bool,
    /// Write the file but don't (un)load it.
    pub no_load: bool,
    /// Only print the service definition.
    pub print: bool,
}

pub fn install_service(o: ServiceOpts) -> Result<()> {
    tools::deny_path_only("`fleet web install-service`")?;
    let exe = crate::cli::hosts::current_exe()?;
    let cfg_path = config::path();
    if !cfg!(target_os = "macos") {
        println!("# systemd user unit — save as ~/.config/systemd/user/fleet-web.service, then:");
        println!("#   systemctl --user daemon-reload && systemctl --user enable --now fleet-web");
        print!("{}", systemd_unit(&exe, &cfg_path));
        return Ok(());
    }
    let home = dirs::home_dir().ok_or_else(|| Error::Other("no home directory".into()))?;
    let log = home.join(SERVICE_LOG);
    // The server fails fast without these; say so now rather than in a log.
    let node = if o.uninstall {
        PathBuf::from("node")
    } else {
        if !o.print {
            web_dir(None)?;
        }
        let (node, managed) = node_pick()?;
        if managed {
            eprintln!(
                "warning: the only node found is version-managed ({}) — the service breaks when that version goes away. \
                 Install a stable node (e.g. Homebrew) or pin one: fleet config set web.node <path>",
                tools::tildify(&node.display().to_string())
            );
        }
        node
    };
    let bins: Vec<Option<PathBuf>> = ["tmux", "claude", "fleet"]
        .iter()
        .map(|b| tools::find_binary(b))
        .collect();
    let plist = launchd_plist(&exe, &cfg_path, &node, &service_path(&node, &bins), &log);
    launchd_agent(LAUNCHD_LABEL, &plist, &log, &o)
}

/// Write (or remove) `~/Library/LaunchAgents/<label>.plist` and (re)load it — or, with
/// `--print`, only print it. Shared by every fleet launchd agent.
pub fn launchd_agent(label: &str, plist: &str, log: &Path, o: &ServiceOpts) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| Error::Other("no home directory".into()))?;
    let plist_path = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    if o.print {
        print!("{plist}");
        return Ok(());
    }
    let uid = String::from_utf8_lossy(&Command::new("id").arg("-u").output()?.stdout)
        .trim()
        .to_string();
    let domain = format!("gui/{uid}");
    let launchctl = |args: &[&str]| -> Result<()> {
        let mut c = Command::new("launchctl");
        c.args(args);
        if hosts::dry_run() {
            println!("{}", hosts::display_command(&c));
            return Ok(());
        }
        // `bootout` of an agent that isn't loaded fails; that's fine (and not worth printing).
        if args.first() == Some(&"bootout") {
            c.stderr(std::process::Stdio::null());
        }
        let _ = c.status();
        Ok(())
    };
    if o.uninstall {
        if !o.no_load {
            launchctl(&["bootout", &format!("{domain}/{label}")])?;
        }
        if hosts::dry_run() {
            println!("rm {}", plist_path.display());
            return Ok(());
        } else if plist_path.exists() {
            std::fs::remove_file(&plist_path)?;
        }
        println!(
            "removed {}",
            tools::tildify(&plist_path.display().to_string())
        );
        return Ok(());
    }
    if hosts::dry_run() {
        println!("write {}", plist_path.display());
    } else {
        std::fs::create_dir_all(plist_path.parent().unwrap_or(&home))?;
        std::fs::write(&plist_path, plist)?;
        println!(
            "wrote {}",
            tools::tildify(&plist_path.display().to_string())
        );
    }
    if o.no_load {
        println!(
            "load it with: launchctl bootstrap {domain} {}",
            plist_path.display()
        );
        return Ok(());
    }
    launchctl(&["bootout", &format!("{domain}/{label}")])?;
    let p = plist_path.display().to_string();
    launchctl(&["bootstrap", &domain, &p])?;
    if hosts::dry_run() {
        return Ok(());
    }
    println!(
        "loaded {label} — logs: {}",
        tools::tildify(&log.display().to_string())
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_prefers_config_then_stable_then_path() {
        let tmp = std::env::temp_dir().join(format!("fleet-node-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let stable = tmp.join("node");
        std::fs::write(&stable, "").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nvm = PathBuf::from("/h/.nvm/versions/node/v22.1.0/bin/node");
        let missing = tmp.join("missing/node");

        let pick = |cfg, st: &[&Path], p: Option<PathBuf>| pick_node(cfg, st, p);
        assert_eq!(
            pick(Some("/pinned/node"), &[&stable], Some(nvm.clone())).unwrap(),
            ("/pinned/node".into(), false)
        );
        assert_eq!(
            pick(None, &[&missing, &stable], Some(nvm.clone())).unwrap(),
            (stable.clone(), false)
        );
        assert_eq!(
            pick(None, &[&missing], Some(nvm.clone())).unwrap(),
            (nvm.clone(), true),
            "nvm only: used, flagged"
        );
        assert_eq!(
            pick(None, &[&missing], Some("/usr/bin/node".into())).unwrap(),
            ("/usr/bin/node".into(), false)
        );
        assert!(pick(None, &[&missing], None).is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn version_managed_paths() {
        for p in [
            "/h/.nvm/versions/node/v22/bin/node",
            "/h/.volta/bin/node",
            "/h/.local/share/fnm/node-versions/v22/installation/bin/node",
            "/h/.fnm/x/node",
        ] {
            assert!(is_version_managed(Path::new(p)), "{p}");
        }
        assert!(!is_version_managed(Path::new("/opt/homebrew/bin/node")));
    }

    #[test]
    fn service_path_skips_version_manager_dirs_but_keeps_the_node() {
        let brew = Path::new("/opt/homebrew/bin/node");
        let bins = [
            Some(PathBuf::from("/h/.nvm/versions/node/v22/bin/claude")),
            Some(PathBuf::from("/h/.local/bin/fleet")),
        ];
        let p = service_path(brew, &bins);
        assert!(p.starts_with("/opt/homebrew/bin:/h/.local/bin:"), "{p}");
        assert!(!p.contains(".nvm"), "{p}");
        let nvm = Path::new("/h/.nvm/versions/node/v22/bin/node");
        assert!(service_path(nvm, &bins).starts_with("/h/.nvm/versions/node/v22/bin:"));
    }

    #[test]
    fn service_log_does_not_clash_with_the_old_one() {
        assert_eq!(SERVICE_LOG, "Library/Logs/fleet.web.log");
    }

    #[test]
    fn plist_runs_this_binary_with_the_config() {
        let p = launchd_plist(
            Path::new("/opt/x/fleet"),
            Path::new("/cfg/a&b.json"),
            Path::new("/opt/homebrew/bin/node"),
            "/usr/bin:/bin",
            Path::new("/logs/w.log"),
        );
        assert!(p.contains("<string>/opt/x/fleet</string>"));
        assert!(p.contains("<string>serve</string>"));
        assert!(p.contains("/cfg/a&amp;b.json"), "{p}");
        assert!(p.contains("<key>KeepAlive</key><true/>"));
        assert!(p.contains("<key>ProcessType</key><string>Interactive</string>"));
        assert!(p.contains("<key>FLEET_NODE</key><string>/opt/homebrew/bin/node</string>"));
        assert!(p.contains(LAUNCHD_LABEL));
    }

    #[test]
    fn systemd_unit_shape() {
        let u = systemd_unit(Path::new("/x/fleet"), Path::new("/c.json"));
        assert!(u.contains("ExecStart=/x/fleet web serve"));
        assert!(u.contains("FLEET_CONFIG=/c.json"));
    }
}
