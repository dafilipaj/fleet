//! `fleet brief`: read, edit and regenerate a session's brief (docs/architecture.md → "Session
//! briefs"). The files are the contract; generation belongs to the web server, so
//! `--regenerate` asks it over HTTP instead of calling a model here.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use colored::Colorize;
use serde_json::Value;

use crate::cli::commands::host_label;
use crate::cli::config_cmd;
use crate::core::brief::{self, Brief, Where};
use crate::core::config;
use crate::core::discovery::{self, Session};
use crate::core::hosts::{self, Remote, Target, Tty};
use crate::core::tools::{expand_tilde, shq_min};
use crate::error::{Error, Result};

/// Exit code for "the brief changed while you were editing it" (nothing saved).
pub const EXIT_CONFLICT: i32 = 3;

/// The session a brief belongs to: a live one, or a gone one whose brief file is still there
/// (named by its full id) — the same rule as the web API.
pub struct Source {
    pub id: String,
    pub session: Option<Session>,
    pub dir: PathBuf,
}

impl Source {
    pub fn label(&self) -> String {
        match &self.session {
            Some(s) => s.headline(),
            None => self.id.clone(),
        }
    }
    fn cwd(&self) -> Option<&str> {
        self.session.as_ref().and_then(|s| s.cwd.as_deref())
    }
    fn path(&self) -> PathBuf {
        brief::file(&self.dir, &self.id).unwrap_or_else(|_| self.dir.join(&self.id))
    }
}

pub fn resolve(target: &str) -> Result<Source> {
    let dir = brief::briefs_dir()?;
    match discovery::resolve_target(target) {
        Ok(s) => {
            let id = s
                .session_id
                .clone()
                .filter(|id| brief::is_session_id(id))
                .ok_or_else(|| Error::Other(format!("{} has no session id yet", s.headline())))?;
            Ok(Source {
                id,
                session: Some(s),
                dir,
            })
        }
        // A gone session's brief by its full id — bare, or as the exact `id:<id>` form. Only
        // when nothing live matched: an ambiguous or malformed target stays an error.
        Err(e @ discovery::TargetError::NotFound(_)) => {
            let id = match discovery::exact_target(target) {
                Some(Ok(discovery::Exact::Id(id))) => id,
                _ => target.trim().to_string(),
            };
            // Session ids are lowercase; `id:` matches any case, so look the canonical one
            // up first (a case-sensitive filesystem would miss `AAAA…` otherwise).
            let lower = id.to_ascii_lowercase();
            for id in [lower, id] {
                if brief::is_session_id(&id) && brief::read(&dir, &id)?.is_some() {
                    return Ok(Source {
                        id,
                        session: None,
                        dir,
                    });
                }
            }
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// The git checkout root (repo or linked worktree) containing `dir`, or `None` outside git.
fn git_root(dir: &str) -> Option<String> {
    let mut c = std::process::Command::new("git");
    c.args(["-C", dir, "rev-parse", "--show-toplevel"]);
    let got = hosts::capture(c, Duration::from_secs(2)).ok()?;
    let top = got.stdout.trim();
    (got.code == Some(0) && top.starts_with('/')).then(|| top.to_string())
}

/// The session's directory on this machine (absolute, `~` expanded) and its git root.
fn dirs_of(src: &Source, b: &Brief) -> (Option<String>, Option<String>) {
    let cwd = src.cwd().map(String::from).or_else(|| b.meta_str("cwd"));
    let abs = cwd.map(|c| expand_tilde(&c)).filter(|c| c.starts_with('/'));
    let root = abs
        .as_deref()
        .filter(|d| std::path::Path::new(d).is_dir())
        .and_then(git_root);
    (abs, root)
}

fn view(src: &Source, b: &Brief, exists: bool) -> Value {
    let label = host_label();
    let me = config::get().self_name();
    let (abs, root) = dirs_of(src, b);
    let w = Where {
        host_label: &label,
        self_name: &me,
        cwd: src.cwd(),
        abs_cwd: abs.as_deref(),
        git_root: root.as_deref(),
    };
    brief::view(&src.id, b, exists, &w, &src.path())
}

/// The continue prompt for `src`, as the server builds it: host from the brief, else this
/// machine; cwd from the live session, else the brief.
pub fn prompt_for(src: &Source, b: &Brief) -> String {
    let host = b
        .meta_str("host")
        .unwrap_or_else(|| config::get().self_name());
    let cwd = src.cwd().map(String::from).or_else(|| b.meta_str("cwd"));
    brief::continue_prompt(b, Some(&host), cwd.as_deref())
}

/// What `spawn --from` needs: the continue prompt and the directory the session worked in.
pub struct Continue {
    pub id: String,
    pub label: String,
    pub prompt: String,
    pub cwd: Option<String>,
}

pub fn continue_from(target: &str) -> Result<Continue> {
    let src = resolve(target)?;
    let (b, exists) = brief::load(&src.dir, &src.id)?;
    if !exists {
        return Err(Error::tagged(
            "no_brief",
            format!(
                "{} has no brief yet — nothing to continue from (generate one: fleet brief {} --regenerate)",
                src.label(),
                src.id
            ),
        ));
    }
    Ok(Continue {
        prompt: prompt_for(&src, &b),
        cwd: src.cwd().map(String::from).or_else(|| b.meta_str("cwd")),
        label: src.label(),
        id: src.id,
    })
}

/// `fleet brief <session>` (body), `--json`, `--prompt`.
pub fn show(target: &str, json: bool, prompt: bool) -> Result<()> {
    let src = resolve(target)?;
    let (b, exists) = brief::load(&src.dir, &src.id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view(&src, &b, exists))?);
    } else if prompt {
        println!("{}", prompt_for(&src, &b));
    } else {
        if !exists {
            eprintln!(
                "{}",
                format!(
                    "no brief yet for {} — the web server writes one (or: fleet brief {} --edit)",
                    src.label(),
                    src.id
                )
                .dimmed()
            );
        }
        print!("{}", brief::serialize_body(&b));
    }
    Ok(())
}

/// Save `text` as a human edit of `src`'s brief. `expect_updated`: refuse when the stored
/// `updated` is no longer this (`""` = there was no brief) — an edit made against an older copy.
fn save_edit(src: &Source, text: &str, expect_updated: Option<&str>) -> Result<(Brief, usize)> {
    let (before, _) = brief::load(&src.dir, &src.id)?;
    let current = before.meta_str("updated").unwrap_or_default();
    if let Some(want) = expect_updated
        && want != current
    {
        return Err(Error::exit(
            EXIT_CONFLICT,
            format!(
                "the brief of {} changed since it was opened (updated {}) — not saved",
                src.label(),
                if current.is_empty() { "-" } else { &current }
            ),
        ));
    }
    let me = config::get().self_name();
    let now = brief::iso_now();
    let removed = brief::removed_resource_keys(&before, &brief::parse_brief(text)).len();
    let mut b = brief::human_edit(&before, text, &now, src.cwd(), &me)?;
    if hosts::dry_run() {
        b.meta
            .insert("session".into(), Value::String(src.id.clone()));
        b.meta.insert("updated".into(), Value::String(now));
        eprintln!("dry-run: would write {}:", src.path().display());
        print!("{}", brief::serialize_brief(&b));
        return Ok((b, removed));
    }
    brief::save(&src.dir, &src.id, &mut b, &now)?;
    Ok((b, removed))
}

fn report_saved(src: &Source, b: &Brief, removed: usize, json: bool) -> Result<()> {
    if hosts::dry_run() {
        return Ok(());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&view(src, b, true))?);
    } else {
        let note = if removed > 0 {
            format!(" ({removed} resource(s) dismissed — they won't be re-added)")
        } else {
            String::new()
        };
        println!("saved the brief of {}{note}", src.label().bold());
    }
    Ok(())
}

/// `fleet brief <session> --set`: the new body (or whole file) on stdin, saved as a human edit.
pub fn set(target: &str, json: bool, expect_updated: Option<&str>) -> Result<()> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    if text.trim().is_empty() {
        return Err(Error::Other(
            "empty brief on stdin — nothing saved (pipe the markdown in)".into(),
        ));
    }
    let src = resolve(target)?;
    let (b, removed) = save_edit(&src, &text, expect_updated)?;
    report_saved(&src, &b, removed, json)
}

/// Put `body` in a temp file, open the editor, read it back. → (text, temp path). The caller
/// removes the file once the edit is safely saved; on failure it stays, so nothing is lost.
fn edit_text(body: &str, id: &str) -> Result<(String, PathBuf)> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let short: String = id.chars().take(8).collect();
    let path = std::env::temp_dir().join(format!(
        "fleet-brief-{short}-{}-{}.md",
        std::process::id(),
        chrono::Local::now().format("%H%M%S")
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    config_cmd::open_editor(&path)?;
    let text = std::fs::read_to_string(&path)?;
    Ok((text, path))
}

fn kept(path: &std::path::Path) -> String {
    format!("your edit is kept in {}", path.display())
}

/// `fleet brief <session> --edit`: the body in `$VISUAL` / `$EDITOR`, saved as a human edit.
/// For a session on another host the brief is fetched over ssh (`fleet brief --json` there),
/// edited here, and written back with `fleet brief <id> --set --expect-updated <updated>`.
pub fn edit(t: &Target, target: &str, json: bool) -> Result<i32> {
    match t {
        Target::Local { .. } => edit_local(target, json).map(|_| 0),
        Target::Remote(r) => edit_remote(r, target, json),
    }
}

fn edit_local(target: &str, json: bool) -> Result<()> {
    let src = resolve(target)?;
    let (b, _) = brief::load(&src.dir, &src.id)?;
    if hosts::dry_run() {
        println!(
            "dry-run: would open {} on the brief of {} ({})",
            config_cmd::editor(),
            src.label(),
            src.path().display()
        );
        return Ok(());
    }
    let opened = b.meta_str("updated").unwrap_or_default();
    let body = brief::serialize_body(&b);
    let (text, tmp) = edit_text(&body, &src.id)?;
    if text == body {
        let _ = std::fs::remove_file(&tmp);
        println!("no changes");
        return Ok(());
    }
    match save_edit(&src, &text, Some(&opened)) {
        Ok((b, removed)) => {
            let _ = std::fs::remove_file(&tmp);
            report_saved(&src, &b, removed, json)
        }
        Err(Error::Exit(code, msg)) => Err(Error::Exit(code, format!("{msg}; {}", kept(&tmp)))),
        Err(e) => Err(Error::Other(format!("{e}; {}", kept(&tmp)))),
    }
}

fn edit_remote(r: &Remote, target: &str, json: bool) -> Result<i32> {
    let fetch = vec!["brief".to_string(), target.to_string(), "--json".into()];
    if hosts::dry_run() {
        let c = hosts::ssh_command(&r.dest, &hosts::remote_fleet_command(r, &fetch), Tty::Never);
        println!("{}", hosts::display_command(&c));
        println!(
            "dry-run: then {} on the body here, and `fleet brief <id> --set` on {} with it on stdin",
            config_cmd::editor(),
            r.name
        );
        return Ok(0);
    }
    let got = hosts::capture_remote(r, &fetch, hosts::remote_timeout())?;
    if !got.ok() {
        return Err(Error::Other(format!("{}: {}", r.name, got.why(r))));
    }
    let v: Value = serde_json::from_str(&got.stdout).map_err(|e| {
        Error::Other(format!(
            "{}: unexpected `fleet brief --json` output ({e}) — is fleet up to date there? (fleet install --host {})",
            r.name, r.name
        ))
    })?;
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if !brief::is_session_id(&id) {
        return Err(Error::Other(format!(
            "{}: no session id in the brief",
            r.name
        )));
    }
    let body = v["body"].as_str().unwrap_or_default().to_string();
    let opened = v["updated"].as_str().unwrap_or_default().to_string();
    let (text, tmp) = edit_text(&body, &id)?;
    if text == body {
        let _ = std::fs::remove_file(&tmp);
        println!("no changes");
        return Ok(0);
    }
    let mut args = vec![
        "brief".to_string(),
        id.clone(),
        "--set".into(),
        "--expect-updated".into(),
        opened,
    ];
    if json {
        args.push("--json".into());
    }
    let c = hosts::ssh_command(&r.dest, &hosts::remote_fleet_command(r, &args), Tty::Never);
    hosts::debug(&format!("[{} set] {}", r.name, hosts::display_command(&c)));
    let put = hosts::capture_input(c, Some(text.as_bytes()), hosts::remote_timeout())?;
    if !put.ok() {
        let code = if put.code == Some(EXIT_CONFLICT) {
            EXIT_CONFLICT
        } else {
            1
        };
        return Err(Error::exit(
            code,
            format!("{}: {}; {}", r.name, put.why(r), kept(&tmp)),
        ));
    }
    let _ = std::fs::remove_file(&tmp);
    print!("{}", put.stdout);
    Ok(0)
}

// ------------------------------------------------------------------ open in the editor

/// The editor command that opens `path`: `code <path>` here, `code --remote ssh-remote+<dest>
/// <path>` for a directory on another host (`cursor` with `web.editor: "cursor"`).
pub fn open_command(bin: &str, remote_dest: Option<&str>, path: &str) -> std::process::Command {
    let mut c = std::process::Command::new(bin);
    if let Some(dest) = remote_dest {
        c.args(["--remote", &format!("ssh-remote+{dest}")]);
    }
    c.arg(path);
    c
}

/// `fleet brief <session> --open`: the session's git root (else its cwd) in VS Code / Cursor —
/// locally, or through Remote-SSH for a session on another host (its paths come from `fleet
/// brief --json` there).
pub fn open(t: &Target, target: &str) -> Result<()> {
    let (path, dest, label) = match t {
        Target::Local { .. } => {
            let src = resolve(target)?;
            let (b, _) = brief::load(&src.dir, &src.id)?;
            let (abs, root) = dirs_of(&src, &b);
            let path = root.or(abs).ok_or_else(|| {
                Error::Other(format!(
                    "{}: no directory known for this session",
                    src.label()
                ))
            })?;
            (path, None, src.label())
        }
        Target::Remote(r) => {
            let fetch = vec!["brief".to_string(), target.to_string(), "--json".into()];
            let got = if hosts::dry_run() {
                None
            } else {
                Some(hosts::capture_remote(r, &fetch, hosts::remote_timeout())?)
            };
            let path = match got {
                None => {
                    let c = hosts::ssh_command(
                        &r.dest,
                        &hosts::remote_fleet_command(r, &fetch),
                        Tty::Never,
                    );
                    println!("{}", hosts::display_command(&c));
                    "<gitRoot or absCwd from there>".to_string()
                }
                Some(got) if !got.ok() => {
                    return Err(Error::Other(format!("{}: {}", r.name, got.why(r))));
                }
                Some(got) => {
                    let v: Value = serde_json::from_str(&got.stdout).unwrap_or(Value::Null);
                    v["gitRoot"]
                        .as_str()
                        .or_else(|| v["absCwd"].as_str())
                        .filter(|p| p.starts_with('/'))
                        .map(String::from)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "{}: no absolute directory in `fleet brief --json` — is fleet up to date there? (fleet install --host {})",
                                r.name, r.name
                            ))
                        })?
                }
            };
            (path, Some(r.dest.clone()), target.to_string())
        }
    };
    let bin = config::get().web.editor_command();
    let mut c = open_command(bin, dest.as_deref(), &path);
    if hosts::dry_run() {
        println!("{}", hosts::display_command(&c));
        return Ok(());
    }
    let status = c.status().map_err(|e| {
        Error::Other(format!(
            "cannot run `{bin}` ({e}) — put the editor's command on PATH (VS Code: \"Shell Command: Install 'code' command in PATH\")"
        ))
    })?;
    if !status.success() {
        return Err(Error::Other(format!("`{bin}` failed ({status})")));
    }
    println!("opened {} in {bin}: {path}", label.bold());
    Ok(())
}

// ------------------------------------------------------------------ regenerate

/// This machine's web server: `hosts.<self>.web`, else `http://127.0.0.1:<web.port>`.
pub fn local_web_base() -> String {
    let cfg = config::get();
    cfg.host(&cfg.self_name())
        .and_then(|h| h.web)
        .map(|w| w.trim().trim_end_matches('/').to_string())
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", cfg.web_port()))
}

pub fn path_segment(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The `curl` that POSTs the regenerate request. No HTTP client in the crate: one POST to a
/// server on this machine doesn't justify one, and curl ships with macOS and every Linux.
pub fn regenerate_command(base: &str, host: &str, id: &str) -> std::process::Command {
    let url = format!(
        "{base}/api/hosts/{}/sessions/{}/brief/regenerate",
        path_segment(host),
        path_segment(id)
    );
    let mut c = std::process::Command::new("curl");
    c.args([
        "-sS",
        "--max-time",
        "20",
        "-X",
        "POST",
        "-H",
        "Content-Type: application/json",
        "--data",
        "{}",
        "-w",
        "\n%{http_code}",
        &url,
    ]);
    c
}

/// `fleet brief <session> --regenerate`: ask this host's web server for a fresh brief.
pub fn regenerate(target: &str, json: bool) -> Result<()> {
    let src = resolve(target)?;
    if src.session.is_none() {
        return Err(Error::Other(format!(
            "{} is not a live session — a brief is regenerated from a live session's transcript",
            src.id
        )));
    }
    let base = local_web_base();
    let me = config::get().self_name();
    let c = regenerate_command(&base, &me, &src.id);
    if hosts::dry_run() {
        println!("{}", hosts::display_command(&c));
        return Ok(());
    }
    let got = hosts::capture(c, Duration::from_secs(25))
        .map_err(|e| Error::Other(format!("{e} — curl is needed for --regenerate")))?;
    let unreachable = || {
        Error::Other(format!(
            "cannot reach the web server at {base} — briefs are generated by it (start it: fleet web serve; \
or set hosts.{me}.web)"
        ))
    };
    if got.code != Some(0) {
        return Err(unreachable());
    }
    let (body, status) = got
        .stdout
        .trim_end()
        .rsplit_once('\n')
        .map(|(b, s)| (b.to_string(), s.trim().to_string()))
        .unwrap_or_else(|| (String::new(), got.stdout.trim().to_string()));
    if status == "000" {
        return Err(unreachable());
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let err = v["error"].as_str().unwrap_or(body.trim()).to_string();
    match status.as_str() {
        "202" => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if v["started"].as_bool() == Some(true) {
                println!(
                    "regenerating the brief of {} (in the background — read it with: fleet brief {})",
                    src.label().bold(),
                    shq_min(&src.id)
                );
            } else if v["queued"].as_bool() == Some(true) {
                println!(
                    "queued: another brief is being generated; {} is next",
                    src.label().bold()
                );
            } else {
                println!("already generating the brief of {}", src.label().bold());
            }
            Ok(())
        }
        "429" => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            let wait = v["retryAfterMs"]
                .as_f64()
                .map(|ms| format!(" (retry in {} min)", (ms / 60000.0).ceil().max(1.0)))
                .unwrap_or_default();
            Err(Error::exit(3, format!("{err}{wait}")))
        }
        "404" if err == "not found" => Err(Error::Other(format!(
            "the web server at {base} has no brief API — update it (fleet install / restart it)"
        ))),
        s => Err(Error::Other(format!("web server answered {s}: {err}"))),
    }
}
