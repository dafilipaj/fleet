//! Which Fleet web server the window shows, whether it answers, and starting
//! `fleet web serve` when it is this machine's and nothing listens.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use fleet::core::{config, tools};
use serde_json::Value;
use tauri::Url;

use crate::profile;

/// The server the window opens, and where that choice came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub url: Url,
    /// Why this URL: `FLEET_DESKTOP_URL`, `desktop.url`, `hosts.<self>.web` or `web.port`.
    pub source: String,
    /// It is this machine's own server, so the app may start it.
    pub local: bool,
}

/// `$FLEET_DESKTOP_URL`, else config `desktop.url`, else `hosts.<self>.web`, else
/// `http://127.0.0.1:<web.port>`. An explicit URL counts as local only on loopback.
pub fn resolve(env_url: Option<&str>, loaded: &config::Loaded) -> Result<Target, String> {
    let explicit = env_url
        .map(|u| (u.to_string(), "FLEET_DESKTOP_URL".to_string()))
        .or_else(|| {
            config::get_path(&loaded.raw, "desktop.url")
                .and_then(Value::as_str)
                .map(|u| (u.to_string(), "desktop.url".to_string()))
        })
        .filter(|(u, _)| !u.trim().is_empty());
    if let Some((raw, source)) = explicit {
        let url = parse(&raw, &source)?;
        let local = is_loopback(&url);
        return Ok(Target { url, source, local });
    }
    let cfg = &loaded.config;
    let me = cfg.self_name();
    if let Some(web) = cfg
        .host(&me)
        .and_then(|h| h.web)
        .filter(|w| !w.trim().is_empty())
    {
        let source = format!("hosts.{me}.web");
        return Ok(Target {
            url: parse(&web, &source)?,
            source,
            local: true,
        });
    }
    let raw = format!("http://127.0.0.1:{}/", cfg.web_port());
    Ok(Target {
        url: parse(&raw, "web.port")?,
        source: "web.port".into(),
        local: true,
    })
}

/// Every server the window may show, in order of preference: `resolve`'s, then each
/// `desktop.fallbacks` entry (a URL, or `"self"` for this machine's own server). The app
/// shows the first that answers. `$FLEET_DESKTOP_URL` (testing) means that server only.
pub fn resolve_all(env_url: Option<&str>, loaded: &config::Loaded) -> Result<Vec<Target>, String> {
    let mut all = vec![resolve(env_url, loaded)?];
    if env_url.is_some_and(|u| !u.trim().is_empty()) {
        return Ok(all);
    }
    let entries: Vec<&Value> = match config::get_path(&loaded.raw, "desktop.fallbacks") {
        None | Some(Value::Null) => vec![],
        Some(v @ Value::String(_)) => vec![v],
        Some(Value::Array(a)) => a.iter().collect(),
        Some(_) => return Err("desktop.fallbacks: expected a list of URLs".into()),
    };
    let own = resolve(None, &without_desktop_url(loaded))?;
    for (i, v) in entries.into_iter().enumerate() {
        let source = format!("desktop.fallbacks[{i}]");
        let raw = v
            .as_str()
            .ok_or_else(|| format!("{source}: not a string"))?
            .trim();
        let t = if raw.is_empty() {
            continue;
        } else if raw == "self" {
            Target {
                source: format!("{source} = {}", own.source),
                ..own.clone()
            }
        } else {
            let url = parse(raw, &source)?;
            // This machine's own server, written out: the app may start it.
            let local = is_loopback(&url) || (own.local && url.origin() == own.url.origin());
            Target { url, source, local }
        };
        if !all.iter().any(|a| a.url.origin() == t.url.origin()) {
            all.push(t);
        }
    }
    Ok(all)
}

/// `loaded` as if `desktop.url` were unset (to find this machine's own server).
fn without_desktop_url(loaded: &config::Loaded) -> config::Loaded {
    let mut l = loaded.clone();
    if let Some(d) = l.raw.get_mut("desktop").and_then(Value::as_object_mut) {
        d.remove("url");
    }
    l
}

/// A profile's server (see `profile`): its env var when set, else the URL baked into
/// the bundle. Always remote — the app never starts it.
pub fn resolve_remote(env_url: Option<&str>, r: &profile::Remote) -> Result<Target, String> {
    let (raw, source) = env_url
        .filter(|u| !u.trim().is_empty())
        .map(|u| (u, r.url_env.clone().unwrap_or_default()))
        .or_else(|| r.url.as_deref().map(|u| (u, "built-in".to_string())))
        .filter(|(u, _)| !u.trim().is_empty())
        .ok_or_else(|| match &r.url_env {
            Some(env) => format!("no URL: set {env} or rebuild the app with --url"),
            None => "no URL: rebuild the app with --url".to_string(),
        })?;
    Ok(Target {
        url: parse(raw, &source)?,
        source,
        local: false,
    })
}

fn parse(raw: &str, source: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("{source}: bad URL {raw:?}: {e}"))?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Ok(url),
        _ => Err(format!("{source}: {raw:?} is not an http(s) URL")),
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `GET /api/health` answered 200 within `timeout`. Plain HTTP only: an https
/// target is reported reachable when its port accepts a connection.
pub fn probe(url: &Url, timeout: Duration) -> Result<(), String> {
    let host = url.host_str().ok_or("URL has no host")?;
    let port = url.port_or_known_default().ok_or("URL has no port")?;
    let addr = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {host}"))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("{addr}: {e}"))?;
    if url.scheme() == "https" {
        return Ok(());
    }
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(timeout)).ok();
    let req = format!(
        "GET /api/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        url.authority()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut head = [0u8; 64];
    let n = s.read(&mut head).map_err(|e| e.to_string())?;
    let line = String::from_utf8_lossy(&head[..n]);
    let status = line.split_whitespace().nth(1).unwrap_or("");
    if status == "200" {
        Ok(())
    } else {
        Err(format!(
            "/api/health answered {status:?}, not 200 — is this a Fleet server?"
        ))
    }
}

/// Like `probe`, but an https target must answer `GET /api/health` with 200 too
/// (checked with `/usr/bin/curl`, so no TLS stack in the app): a proxy such as
/// `tailscale serve` accepts connections even when the app behind it is down.
pub fn probe_strict(url: &Url, timeout: Duration) -> Result<(), String> {
    if url.scheme() != "https" {
        return probe(url, timeout);
    }
    let health = url.join("/api/health").map_err(|e| e.to_string())?;
    let out = Command::new("/usr/bin/curl")
        .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time"])
        .arg(format!("{:.1}", timeout.as_secs_f64()))
        .arg(health.as_str())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match status.as_str() {
        "200" => Ok(()),
        "000" | "" if !err.is_empty() => Err(err),
        "000" | "" => Err(format!("{health}: no answer")),
        s => Err(format!("{health} answered {s}, not 200")),
    }
}

/// PATH for the server process. A Finder-launched app gets only the system
/// dirs, so add the login shell's PATH and the usual install locations.
pub fn server_path() -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = login_shell_path() {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/'))?;
    let mut child = Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A slow or interactive profile must not hang the app.
    for _ in 0..30 {
        if let Ok(Some(_)) = child.try_wait() {
            let mut out = String::new();
            child.stdout.take()?.read_to_string(&mut out).ok()?;
            return Some(out.trim().to_string()).filter(|s| !s.is_empty());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    None
}

/// The `fleet` binary: config `fleetBin`, else the first `fleet` on `path`.
pub fn fleet_bin(cfg: &config::Config, path: &str) -> Option<PathBuf> {
    if let Some(b) = cfg.fleet_bin.as_deref().filter(|b| !b.trim().is_empty()) {
        return Some(PathBuf::from(tools::expand_tilde(b)));
    }
    std::env::split_paths(path)
        .map(|d| d.join("fleet"))
        .find(|p| tools::is_executable(p))
}

/// Where a server started by the app logs.
pub fn log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library/Logs/Fleet/web-serve.log")
}

/// `fleet web serve` arguments that make it listen where `t` points: its port,
/// and loopback only when the URL is loopback (else the config's bind).
pub fn serve_args(t: &Target) -> Vec<String> {
    let mut a = vec!["--local".into(), "web".into(), "serve".into()];
    if let Some(p) = t.url.port_or_known_default() {
        a.extend(["--port".into(), p.to_string()]);
    }
    if is_loopback(&t.url) {
        a.extend(["--bind".into(), "127.0.0.1".into()]);
    }
    a
}

/// Start `fleet <args>` in the background, output appended to `log`.
pub fn spawn(fleet: &Path, args: &[String], path: &str, log: &Path) -> Result<Child, String> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    Command::new(fleet)
        .args(args)
        .env("PATH", path)
        .current_dir(dirs::home_dir().unwrap_or_else(|| "/".into()))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", fleet.display()))
}

/// The last `n` lines of `path` (for the "server not running" screen).
pub fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(raw: &str) -> config::Loaded {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(&p, raw).unwrap();
        config::load_from(&p)
    }

    #[test]
    fn resolve_prefers_env_then_desktop_url_then_self_web_then_port() {
        let l = loaded(
            r#"{ "self": "laptop", "desktop": { "url": "http://192.0.2.1:7777" },
                 "hosts": { "laptop": { "web": "http://127.0.0.1:7788" } }, "web": { "port": 7799 } }"#,
        );
        let t = resolve(Some("http://localhost:1234"), &l).unwrap();
        assert_eq!((t.url.as_str(), t.local), ("http://localhost:1234/", true));
        let t = resolve(None, &l).unwrap();
        assert_eq!((t.source.as_str(), t.local), ("desktop.url", false));

        let l = loaded(
            r#"{ "self": "laptop", "hosts": { "laptop": { "web": "http://192.0.2.1:7788" } } }"#,
        );
        let t = resolve(None, &l).unwrap();
        assert_eq!(t.url.as_str(), "http://192.0.2.1:7788/");
        assert_eq!((t.source.as_str(), t.local), ("hosts.laptop.web", true));

        let l = loaded(r#"{ "web": { "port": 7799 } }"#);
        let t = resolve(None, &l).unwrap();
        assert_eq!(t.url.as_str(), "http://127.0.0.1:7799/");
    }

    #[test]
    fn fallbacks_follow_the_preferred_server_in_order() {
        let l = loaded(
            r#"{ "self": "laptop", "desktop": { "url": "https://workstation.example:7443",
                 "fallbacks": ["http://192.0.2.2:7777", "self", "https://workstation.example:7443/", ""] },
                 "hosts": { "laptop": { "web": "http://192.0.2.1:7777" } } }"#,
        );
        let all = resolve_all(None, &l).unwrap();
        let got: Vec<_> = all
            .iter()
            .map(|t| (t.url.as_str(), t.source.as_str(), t.local))
            .collect();
        assert_eq!(
            got,
            [
                ("https://workstation.example:7443/", "desktop.url", false),
                ("http://192.0.2.2:7777/", "desktop.fallbacks[0]", false),
                (
                    "http://192.0.2.1:7777/",
                    "desktop.fallbacks[1] = hosts.laptop.web",
                    true
                ),
            ]
        );

        // This machine's own server written out is local too; a lone string works.
        let l = loaded(
            r#"{ "self": "laptop", "desktop": { "url": "http://192.0.2.9:7777", "fallbacks": "http://192.0.2.1:7777" },
                 "hosts": { "laptop": { "web": "http://192.0.2.1:7777" } } }"#,
        );
        let all = resolve_all(None, &l).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[1].local);

        // The env override (testing) means that server only.
        let all = resolve_all(Some("http://127.0.0.1:1234"), &l).unwrap();
        assert_eq!(all.len(), 1);

        let bad = loaded(r#"{ "desktop": { "fallbacks": [7] } }"#);
        assert!(
            resolve_all(None, &bad)
                .unwrap_err()
                .contains("fallbacks[0]")
        );
        let bad = loaded(r#"{ "desktop": { "fallbacks": ["ftp://x"] } }"#);
        assert!(resolve_all(None, &bad).is_err());
        let none = loaded("{}");
        assert_eq!(resolve_all(None, &none).unwrap().len(), 1);
    }

    #[test]
    fn missing_config_means_default_port() {
        let l = config::load_from(Path::new("/nonexistent/fleet/config.json"));
        let t = resolve(None, &l).unwrap();
        assert_eq!(t.url.as_str(), "http://127.0.0.1:7777/");
        assert!(t.local);
    }

    #[test]
    fn bad_urls_are_errors() {
        let l = loaded("{}");
        assert!(resolve(Some("not a url"), &l).is_err());
        assert!(resolve(Some("file:///etc/passwd"), &l).is_err());
    }

    #[test]
    fn remote_profiles_use_env_then_built_in_url_and_never_start_a_server() {
        let r = profile::Remote {
            name: "Example".into(),
            identifier: "dev.example".into(),
            url: Some("https://app.example.com:4443".into()),
            url_env: Some("EXAMPLE_URL".into()),
            title_bar: profile::TitleBar::Native,
            frame_origins: vec![],
        };
        let t = resolve_remote(None, &r).unwrap();
        assert_eq!(t.url.as_str(), "https://app.example.com:4443/");
        assert_eq!((t.source.as_str(), t.local), ("built-in", false));
        let t = resolve_remote(Some("http://127.0.0.1:5173"), &r).unwrap();
        assert_eq!((t.source.as_str(), t.local), ("EXAMPLE_URL", false));
        assert_eq!(resolve_remote(Some(" "), &r).unwrap().source, "built-in");

        let none = profile::Remote { url: None, ..r };
        assert!(
            resolve_remote(None, &none)
                .unwrap_err()
                .contains("EXAMPLE_URL")
        );
        assert!(resolve_remote(Some("file:///etc/passwd"), &none).is_err());
    }

    #[test]
    fn probe_strict_reports_an_unreachable_https_server() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let u = Url::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
        assert!(probe_strict(&u, Duration::from_secs(2)).is_err());
    }

    #[test]
    fn probe_accepts_health_200_only() {
        use std::net::TcpListener;
        fn serve_once(reply: &'static str) -> Url {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(reply.as_bytes());
            });
            Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
        }
        let t = Duration::from_secs(2);
        assert!(probe(&serve_once("HTTP/1.1 200 OK\r\n\r\n{}"), t).is_ok());
        assert!(probe(&serve_once("HTTP/1.1 404 Not Found\r\n\r\n"), t).is_err());

        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let closed = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        assert!(probe(&closed, t).is_err());
    }

    #[test]
    fn serve_args_follow_the_url() {
        let t = |u: &str| Target {
            url: Url::parse(u).unwrap(),
            source: String::new(),
            local: true,
        };
        assert_eq!(
            serve_args(&t("http://127.0.0.1:7791/")),
            [
                "--local",
                "web",
                "serve",
                "--port",
                "7791",
                "--bind",
                "127.0.0.1"
            ]
        );
        assert_eq!(
            serve_args(&t("http://192.0.2.1:7777/")),
            ["--local", "web", "serve", "--port", "7777"]
        );
    }

    #[test]
    fn tail_keeps_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(tail(&p, 2), "b\nc");
        assert_eq!(tail(&dir.path().join("none"), 2), "");
    }
}
