//! `fleet init` — write this machine's config, interactively or from flags.
//!
//! Never overwrites an existing config without confirmation (`--force` when
//! non-interactive). Keys it doesn't manage — and host keys it doesn't manage,
//! like `fleetBin` — are carried over from the existing file.

use std::io::{BufRead, Write};

use serde_json::{Map, Value, json};

use crate::core::config;
use crate::core::tools;
use crate::error::{Error, Result};

#[derive(clap::Args, Debug, Clone, Default)]
pub struct InitArgs {
    /// This machine's host name (default: the short hostname)
    #[arg(long = "self", value_name = "NAME")]
    pub self_name: Option<String>,
    /// A host: NAME[,ssh=DEST][,web=URL] (repeatable; this machine needs no ssh)
    #[arg(long = "add-host", value_name = "SPEC")]
    pub hosts: Vec<String>,
    /// Host that tmux-session commands target by default
    #[arg(long, value_name = "NAME")]
    pub default_host: Option<String>,
    /// A spawn directory: LABEL=HOST:PATH[,HOST:PATH…] or LABEL=PATH (every host)
    #[arg(long = "spawn-dir", value_name = "SPEC")]
    pub spawn_dirs: Vec<String>,
    /// Port for the web UI
    #[arg(long, value_name = "PORT")]
    pub web_port: Option<u16>,
    /// Address the web UI binds to
    #[arg(long, value_name = "ADDR")]
    pub web_bind: Option<String>,
    /// Where the web app lives (default: auto-detected)
    #[arg(long, value_name = "PATH")]
    pub web_dir: Option<String>,
    /// Non-interactive: take the flags (and defaults) as the answers
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Overwrite an existing config without asking
    #[arg(long)]
    pub force: bool,
    /// Print the config instead of writing it
    #[arg(long)]
    pub print: bool,
}

/// `NAME[,ssh=DEST][,web=URL]`.
pub fn parse_host_spec(spec: &str) -> Result<(String, Option<String>, Option<String>)> {
    let mut parts = spec.split(',');
    let name = parts.next().unwrap_or("").trim().to_string();
    if name.is_empty() || name.contains('=') {
        return Err(Error::exit(
            1,
            format!("--add-host '{spec}': expected NAME[,ssh=DEST][,web=URL]"),
        ));
    }
    let (mut ssh, mut web) = (None, None);
    for p in parts {
        match p.split_once('=') {
            Some(("ssh", v)) if !v.trim().is_empty() => ssh = Some(v.trim().to_string()),
            Some(("web", v)) if !v.trim().is_empty() => web = Some(v.trim().to_string()),
            _ => {
                return Err(Error::exit(
                    1,
                    format!("--add-host '{spec}': unknown part '{p}' (expected ssh=… or web=…)"),
                ));
            }
        }
    }
    Ok((name, ssh, web))
}

/// `LABEL=HOST:PATH[,HOST:PATH…]` or `LABEL=PATH` (the same path on every host).
pub fn parse_spawn_spec(spec: &str, hosts: &[String]) -> Result<Value> {
    let (label, rest) = spec
        .split_once('=')
        .ok_or_else(|| Error::exit(1, format!("--spawn-dir '{spec}': expected LABEL=HOST:PATH")))?;
    let label = label.trim();
    if label.is_empty() || rest.trim().is_empty() {
        return Err(Error::exit(
            1,
            format!("--spawn-dir '{spec}': expected LABEL=HOST:PATH"),
        ));
    }
    let mut paths = Map::new();
    for part in rest.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once(':') {
            Some((h, p)) if !h.starts_with(['/', '~']) => {
                paths.insert(h.to_string(), Value::String(p.to_string()));
            }
            _ => {
                for h in hosts {
                    paths.insert(h.clone(), Value::String(part.to_string()));
                }
            }
        }
    }
    Ok(json!({ "label": label, "paths": paths }))
}

/// Build the config from answers, on top of whatever was there before.
pub struct Answers {
    pub self_name: String,
    /// (name, ssh, web) in order; self included.
    pub hosts: Vec<(String, Option<String>, Option<String>)>,
    pub default_host: String,
    pub spawn_dirs: Vec<Value>,
    pub web_port: u16,
    pub web_bind: Option<String>,
    pub web_dir: Option<String>,
}

pub fn build(existing: &Value, a: &Answers) -> Result<Value> {
    let mut raw = if existing.is_object() {
        existing.clone()
    } else {
        Value::Object(Map::new())
    };
    config::set_path(&mut raw, "version", json!(config::VERSION))?;
    config::set_path(&mut raw, "self", json!(a.self_name))?;
    config::set_path(&mut raw, "defaultHost", json!(a.default_host))?;
    let old_hosts = existing
        .get("hosts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut hosts = Map::new();
    for (name, ssh, web) in &a.hosts {
        let mut h = old_hosts
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let ssh = if *name == a.self_name {
            None
        } else {
            ssh.clone()
        };
        h.insert("ssh".into(), ssh.map(Value::String).unwrap_or(Value::Null));
        h.insert(
            "web".into(),
            web.clone().map(Value::String).unwrap_or(Value::Null),
        );
        hosts.insert(name.clone(), Value::Object(h));
    }
    raw["hosts"] = Value::Object(hosts);
    config::set_path(&mut raw, "web.port", json!(a.web_port))?;
    config::set_path(
        &mut raw,
        "web.bind",
        json!(a.web_bind.clone().unwrap_or_else(|| "0.0.0.0".into())),
    )?;
    config::set_path(
        &mut raw,
        "web.dir",
        a.web_dir.clone().map(Value::String).unwrap_or(Value::Null),
    )?;
    if raw.get("tmux").is_none() {
        raw["tmux"] = Value::Null;
    }
    raw["spawnDirs"] = Value::Array(a.spawn_dirs.clone());
    Ok(raw)
}

// --- tailscale -----------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Peer {
    pub host_name: String,
    pub ip: Option<String>,
    pub online: bool,
}

/// `(self, peers)` from `tailscale status --json`, when tailscale is around.
pub fn tailscale() -> Option<(Peer, Vec<Peer>)> {
    let bin = tools::find_binary("tailscale")
        .map(|p| p.display().to_string())
        .or_else(|| {
            let app =
                tools::install_location("/Applications/Tailscale.app/Contents/MacOS/Tailscale")?;
            std::path::Path::new(app).exists().then(|| app.to_string())
        })?;
    let out = std::process::Command::new(bin)
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_tailscale(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_tailscale(text: &str) -> Option<(Peer, Vec<Peer>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let peer = |p: &Value| Peer {
        host_name: p["HostName"].as_str().unwrap_or("").to_string(),
        ip: p["TailscaleIPs"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .find(|ip| ip.contains('.'))
            })
            .map(str::to_string),
        online: p["Online"].as_bool().unwrap_or(false),
    };
    let me = peer(&v["Self"]);
    let mut peers: Vec<Peer> = v["Peer"]
        .as_object()
        .map(|m| {
            m.values()
                .map(peer)
                .filter(|p| !p.host_name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    peers.sort_by(|a, b| b.online.cmp(&a.online).then(a.host_name.cmp(&b.host_name)));
    Some((me, peers))
}

// --- prompting -----------------------------------------------------------------

struct Prompter<R: BufRead> {
    input: R,
}

impl<R: BufRead> Prompter<R> {
    fn ask(&mut self, q: &str, default: Option<&str>) -> Result<String> {
        match default.filter(|d| !d.is_empty()) {
            Some(d) => eprint!("{q} [{d}]: "),
            None => eprint!("{q}: "),
        }
        std::io::stderr().flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Err(Error::exit(1, "init: input ended — aborted"));
        }
        let line = line.trim().to_string();
        Ok(if line.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            line
        })
    }

    fn yes(&mut self, q: &str, default: bool) -> Result<bool> {
        let a = self.ask(
            &format!("{q} {}", if default { "(Y/n)" } else { "(y/N)" }),
            None,
        )?;
        Ok(match a.to_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        })
    }
}

fn default_self(existing: &config::Config) -> String {
    existing.self_name.clone().unwrap_or_else(|| {
        let h = crate::core::tmux::sanitize_name(&tools::short_hostname());
        if h.is_empty() { "local".into() } else { h }
    })
}

fn web_url(ip: Option<&str>, port: u16) -> Option<String> {
    ip.map(|ip| format!("http://{ip}:{port}"))
}

fn interactive(existing: &config::Loaded, a: &InitArgs) -> Result<Answers> {
    let stdin = std::io::stdin();
    let mut p = Prompter {
        input: stdin.lock(),
    };
    let old = &existing.config;
    eprintln!("fleet init — this writes {}\n", existing.path.display());

    let ts = tailscale();
    let self_name = p.ask(
        "Name for this machine",
        Some(&a.self_name.clone().unwrap_or_else(|| default_self(old))),
    )?;
    let port: u16 = p
        .ask(
            "Web UI port",
            Some(&a.web_port.unwrap_or(old.web_port()).to_string()),
        )?
        .parse()
        .map_err(|_| Error::exit(1, "init: not a port number"))?;
    let old_host = |n: &str| old.host(n).unwrap_or_default();
    let self_web = p.ask(
        &format!("Web URL of {self_name} (how other machines reach it; empty for none)"),
        old_host(&self_name)
            .web
            .or_else(|| web_url(ts.as_ref().and_then(|t| t.0.ip.as_deref()), port))
            .as_deref(),
    )?;
    let mut hosts = vec![(
        self_name.clone(),
        None,
        (!self_web.is_empty()).then_some(self_web),
    )];

    if let Some((_, peers)) = &ts
        && !peers.is_empty()
    {
        eprintln!("\nTailscale peers:");
        for pe in peers.iter().take(12) {
            eprintln!(
                "  {} {:<24} {}",
                if pe.online { "●" } else { "○" },
                pe.host_name,
                pe.ip.as_deref().unwrap_or("")
            );
        }
    }
    let mut suggested: Vec<String> = old
        .hosts()
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| *n != self_name)
        .collect();
    loop {
        eprintln!();
        let name = p.ask(
            "Add a remote host — name (empty to finish)",
            suggested.first().map(String::as_str),
        )?;
        if !suggested.is_empty() {
            suggested.remove(0);
        }
        if name.is_empty() {
            break;
        }
        let peer = ts.as_ref().and_then(|(_, peers)| {
            peers
                .iter()
                .find(|pe| pe.host_name.eq_ignore_ascii_case(&name))
                .cloned()
        });
        let h = old_host(&name);
        let ssh = p.ask(
            &format!("  ssh destination for {name} (~/.ssh/config alias or user@host)"),
            Some(&h.ssh.clone().unwrap_or_else(|| name.clone())),
        )?;
        let web = p.ask(
            &format!("  web URL of {name} (empty for none)"),
            h.web
                .or_else(|| web_url(peer.as_ref().and_then(|pe| pe.ip.as_deref()), port))
                .as_deref(),
        )?;
        hosts.push((
            name,
            (!ssh.is_empty()).then_some(ssh),
            (!web.is_empty()).then_some(web),
        ));
    }

    let names: Vec<String> = hosts.iter().map(|h| h.0.clone()).collect();
    let default_default = a
        .default_host
        .clone()
        .or_else(|| old.default_host.clone().filter(|d| names.contains(d)))
        .unwrap_or_else(|| names.get(1).unwrap_or(&names[0]).clone());
    let default_host = loop {
        let d = p.ask(
            &format!("\nDefault host for tmux commands ({})", names.join(" / ")),
            Some(&default_default),
        )?;
        if names.contains(&d) {
            break d;
        }
        eprintln!("  not one of: {}", names.join(", "));
    };

    let mut spawn_dirs: Vec<Value> = if old.spawn_dirs.is_empty() {
        Vec::new()
    } else if p.yes(
        &format!("\nKeep the {} existing spawn dir(s)?", old.spawn_dirs.len()),
        true,
    )? {
        existing
            .raw
            .get("spawnDirs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    loop {
        let label = p.ask("\nAdd a spawn dir — label (empty to finish)", None)?;
        if label.is_empty() {
            break;
        }
        let mut paths = Map::new();
        for n in &names {
            let path = p.ask(&format!("  path on {n} (empty to skip)"), None)?;
            if !path.is_empty() {
                paths.insert(n.clone(), Value::String(path));
            }
        }
        spawn_dirs.push(json!({ "label": label, "paths": paths }));
    }

    let detected = a
        .web_dir
        .clone()
        .or_else(|| old.web.dir.clone())
        .or_else(|| config::detect_web_dir().map(|d| tools::tildify(&d.display().to_string())));
    let web_dir = p.ask(
        "\nWeb app directory (empty to auto-detect)",
        detected.as_deref(),
    )?;

    Ok(Answers {
        self_name,
        hosts,
        default_host,
        spawn_dirs,
        web_port: port,
        web_bind: a.web_bind.clone().or_else(|| old.web.bind.clone()),
        web_dir: (!web_dir.is_empty()).then_some(web_dir),
    })
}

fn from_flags(existing: &config::Loaded, a: &InitArgs) -> Result<Answers> {
    let old = &existing.config;
    let self_name = a.self_name.clone().unwrap_or_else(|| default_self(old));
    let mut hosts = Vec::new();
    for spec in &a.hosts {
        let (name, ssh, web) = parse_host_spec(spec)?;
        if hosts.iter().any(|(n, _, _): &(String, _, _)| *n == name) {
            return Err(Error::exit(1, format!("--add-host {name} given twice")));
        }
        // A host without ssh is a web-only peer (e.g. a laptop the workstation can't ssh into).
        if name != self_name && ssh.is_none() && web.is_none() {
            return Err(Error::exit(
                1,
                format!("--add-host {name} needs ssh=<destination> and/or web=<url>"),
            ));
        }
        hosts.push((name, ssh, web));
    }
    if !hosts.iter().any(|h| h.0 == self_name) {
        hosts.insert(0, (self_name.clone(), None, None));
    }
    let names: Vec<String> = hosts.iter().map(|h| h.0.clone()).collect();
    let default_host = a
        .default_host
        .clone()
        .unwrap_or_else(|| names.get(1).unwrap_or(&names[0]).clone());
    if !names.contains(&default_host) {
        return Err(Error::exit(
            1,
            format!(
                "--default-host {default_host} is not one of: {}",
                names.join(", ")
            ),
        ));
    }
    let spawn_dirs = if a.spawn_dirs.is_empty() {
        existing
            .raw
            .get("spawnDirs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        a.spawn_dirs
            .iter()
            .map(|s| parse_spawn_spec(s, &names))
            .collect::<Result<Vec<_>>>()?
    };
    Ok(Answers {
        self_name,
        hosts,
        default_host,
        spawn_dirs,
        web_port: a.web_port.unwrap_or(old.web_port()),
        web_bind: a.web_bind.clone().or_else(|| old.web.bind.clone()),
        web_dir: a.web_dir.clone().or_else(|| old.web.dir.clone()),
    })
}

pub fn run(a: InitArgs) -> Result<()> {
    let path = config::path();
    let existing = config::load_from(&path);
    if existing.exists && existing.problem.is_some() && !a.force {
        return Err(Error::exit(
            1,
            format!(
                "{} — fix it by hand (fleet config edit) or start over with --force",
                existing.problem.clone().unwrap_or_default()
            ),
        ));
    }
    let can_prompt = tools::interactive();
    if !a.yes && !can_prompt {
        return Err(Error::exit(
            1,
            "init: not a terminal — pass --yes with --self/--add-host/… to run non-interactively",
        ));
    }
    if existing.exists && !a.force && !a.print {
        if a.yes {
            return Err(Error::exit(
                1,
                format!(
                    "{} already exists — pass --force to overwrite it",
                    path.display()
                ),
            ));
        }
        let stdin = std::io::stdin();
        let mut p = Prompter {
            input: stdin.lock(),
        };
        if !p.yes(
            &format!(
                "{} already exists. Rewrite it (unknown keys are kept)?",
                path.display()
            ),
            false,
        )? {
            eprintln!("fleet: aborted — nothing written");
            return Ok(());
        }
    }
    let answers = if a.yes {
        from_flags(&existing, &a)?
    } else {
        interactive(&existing, &a)?
    };
    let raw = build(&existing.raw, &answers)?;
    let problems = serde_json::from_value::<config::Config>(raw.clone())
        .map(|c| c.problems())
        .unwrap_or_default();
    if a.print {
        println!("{}", serde_json::to_string_pretty(&raw)?);
        return Ok(());
    }
    if !a.yes {
        eprintln!("\n{}", serde_json::to_string_pretty(&raw)?);
        let stdin = std::io::stdin();
        let mut p = Prompter {
            input: stdin.lock(),
        };
        if !p.yes(&format!("\nWrite this to {}?", path.display()), true)? {
            eprintln!("fleet: aborted — nothing written");
            return Ok(());
        }
    }
    config::write_raw(&path, &raw)?;
    println!("wrote {}", path.display());
    for pr in problems {
        eprintln!("fleet: config: {pr}");
    }
    let remotes: Vec<&String> = answers
        .hosts
        .iter()
        .filter(|h| h.0 != answers.self_name && h.1.is_some())
        .map(|h| &h.0)
        .collect();
    if let Some(r) = remotes.first() {
        println!("next: fleet install --host {r}   then: fleet doctor");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_specs() {
        assert_eq!(
            parse_host_spec("workstation,ssh=devbox,web=http://100.x.y.z:7777").unwrap(),
            (
                "workstation".into(),
                Some("devbox".into()),
                Some("http://100.x.y.z:7777".into())
            )
        );
        assert_eq!(
            parse_host_spec("laptop").unwrap(),
            ("laptop".into(), None, None)
        );
        assert!(parse_host_spec("").is_err());
        assert!(parse_host_spec("x,port=1").is_err());
    }

    #[test]
    fn spawn_specs() {
        let hosts = vec!["laptop".to_string(), "workstation".to_string()];
        let v = parse_spawn_spec("Work=laptop:~/Code/app,workstation:~/src/app", &hosts).unwrap();
        assert_eq!(v["label"], "Work");
        assert_eq!(v["paths"]["workstation"], "~/src/app");
        let v = parse_spawn_spec("Home=~", &hosts).unwrap();
        assert_eq!(v["paths"]["laptop"], "~");
        assert_eq!(v["paths"]["workstation"], "~");
        assert!(parse_spawn_spec("nolabel", &hosts).is_err());
    }

    #[test]
    fn build_keeps_what_it_does_not_manage() {
        let existing = json!({
            "version": 1,
            "self": "old",
            "hosts": { "workstation": { "ssh": "x", "fleetBin": "~/bin/fleet" } },
            "web": { "ui": "~/ui", "quickReplies": ["ok"] },
            "claude": "my-claude",
            "tui": { "rows": "1" }
        });
        let a = Answers {
            self_name: "laptop".into(),
            hosts: vec![
                ("laptop".into(), Some("ignored".into()), None),
                (
                    "workstation".into(),
                    Some("devbox".into()),
                    Some("http://100.x.y.z:7777".into()),
                ),
            ],
            default_host: "workstation".into(),
            spawn_dirs: vec![],
            web_port: 7777,
            web_bind: None,
            web_dir: None,
        };
        let raw = build(&existing, &a).unwrap();
        assert_eq!(raw["self"], "laptop");
        assert_eq!(
            raw["hosts"]["laptop"]["ssh"],
            Value::Null,
            "self never has ssh"
        );
        assert_eq!(raw["hosts"]["workstation"]["ssh"], "devbox");
        assert_eq!(raw["hosts"]["workstation"]["fleetBin"], "~/bin/fleet");
        assert_eq!(raw["web"]["ui"], "~/ui");
        assert_eq!(raw["web"]["quickReplies"][0], "ok");
        assert_eq!(raw["web"]["port"], 7777);
        assert_eq!(raw["claude"], "my-claude");
        assert_eq!(raw["tui"]["rows"], "1");
        assert_eq!(raw["tmux"], Value::Null);
        assert!(raw["hosts"].get("old").is_none());
    }

    #[test]
    fn tailscale_status_parsing() {
        let text = r#"{"Self":{"HostName":"laptop","TailscaleIPs":["192.0.2.10","fd7a::1"],"Online":true},
          "Peer":{"k1":{"HostName":"box","TailscaleIPs":["fd7a::2","192.0.2.20"],"Online":true},
                  "k2":{"HostName":"phone","TailscaleIPs":[],"Online":false}}}"#;
        let (me, peers) = parse_tailscale(text).unwrap();
        assert_eq!(me.ip.as_deref(), Some("192.0.2.10"));
        assert_eq!(peers[0].host_name, "box");
        assert_eq!(peers[0].ip.as_deref(), Some("192.0.2.20"));
        assert!(!peers[1].online);
    }
}
