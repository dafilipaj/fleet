//! `fleet repos` / `fleet repos sync` / `fleet repos install-service`: keep the git checkouts
//! under the configured roots fresh (see `core::repos`).

use std::collections::HashMap;
use std::path::Path;

use colored::Colorize;

use crate::cli::render::{ago, fit, width_of};
use crate::cli::web::{self, ServiceOpts};
use crate::core::config;
use crate::core::repos::{self, Inspect, Outcome, Repo, Settings, SyncResult};
use crate::core::tools;
use crate::error::{Error, Result};

pub const LAUNCHD_LABEL: &str = "fleet.repos";
pub const SERVICE_LOG: &str = "Library/Logs/fleet.repos.log";

fn warn_problems(s: &Settings) {
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
}

/// Repos matching `names` (directory name or path); all of them when `names` is empty.
fn select(all: Vec<Repo>, names: &[String]) -> Result<Vec<Repo>> {
    if names.is_empty() {
        return Ok(all);
    }
    let mut out = Vec::new();
    for n in names {
        let hits: Vec<&Repo> = all
            .iter()
            .filter(|r| repos::matches(n, &r.name, &r.path))
            .collect();
        if hits.is_empty() {
            return Err(Error::exit(
                1,
                format!("no repo named '{n}' under the configured roots"),
            ));
        }
        out.extend(hits.into_iter().cloned());
    }
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

/// Repo names, with the `~/…` path for any name that appears twice.
fn labels(list: &[Repo]) -> Vec<String> {
    let mut n: HashMap<&str, usize> = HashMap::new();
    for r in list {
        *n.entry(&r.name).or_default() += 1;
    }
    list.iter()
        .map(|r| {
            if n[r.name.as_str()] > 1 {
                tools::tildify(&r.path.display().to_string())
            } else {
                r.name.clone()
            }
        })
        .collect()
}

/// `30m`, `24h`, `7d`.
pub(crate) fn span(secs: i64) -> String {
    if secs % 86_400 == 0 && secs >= 86_400 && secs != 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}m", secs / 60)
    }
}

fn outcome_text(o: Outcome) -> colored::ColoredString {
    let l = o.label();
    match o {
        Outcome::Updated | Outcome::Current => l.green(),
        Outcome::Diverged | Outcome::Blocked | Outcome::FetchFailed => l.red(),
        _ => l.yellow(),
    }
}

/// `↓3 ↑1 ✎2`, or `=` when in step.
fn position(i: &Inspect) -> String {
    let mut parts = Vec::new();
    if i.behind > 0 {
        parts.push(format!("↓{}", i.behind));
    }
    if i.ahead > 0 {
        parts.push(format!("↑{}", i.ahead));
    }
    if i.dirty > 0 {
        parts.push(format!("✎{}", i.dirty));
    }
    if parts.is_empty() {
        "=".into()
    } else {
        parts.join(" ")
    }
}

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(width_of(s))))
}

/// `fleet repos`: what each repo looks like now (local only, as of its last fetch).
/// `all` keeps the excluded repos too, flagged.
pub fn status(json: bool, all: bool) -> Result<()> {
    let s = Settings::load();
    warn_problems(&s);
    let (list, excluded): (Vec<Repo>, Vec<bool>) = repos::discover_all(&s)
        .into_iter()
        .filter(|(_, x)| all || !x)
        .unzip();
    let st = repos::load_state(&repos::state_path());
    let infos: Vec<Inspect> = std::thread::scope(|sc| {
        let hs: Vec<_> = list
            .iter()
            .map(|r| sc.spawn(|| repos::inspect(&r.path)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let now = chrono::Local::now();
    if json {
        let rows: Vec<serde_json::Value> = list
            .iter()
            .zip(&infos)
            .zip(&excluded)
            .map(|((r, i), x)| {
                let e = st
                    .repos
                    .get(&r.path.display().to_string())
                    .cloned()
                    .unwrap_or_default();
                serde_json::json!({
                    "name": r.name,
                    "path": tools::tildify(&r.path.display().to_string()),
                    "every": r.every,
                    "excluded": x,
                    "due": repos::is_due_state(r.every, Some(&e), &now),
                    "branch": i.branch,
                    "upstream": i.upstream,
                    "ahead": i.ahead,
                    "behind": i.behind,
                    "dirty": i.dirty,
                    "busy": i.busy,
                    "lastAttempt": e.last_attempt,
                    "lastFetch": e.last_fetch,
                    "outcome": e.outcome,
                    "detail": e.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if list.is_empty() {
        println!(
            "no git repos under {} — set roots with: fleet config set repos.roots '[\"~/Code\"]'",
            s.roots.join(", ")
        );
        return Ok(());
    }
    let names = labels(&list);
    let nw = names.iter().map(|n| width_of(n)).max().unwrap_or(4).max(4);
    let branches: Vec<String> = infos
        .iter()
        .map(|i| fit(i.branch.as_deref().unwrap_or("(detached)"), 28))
        .collect();
    let bw = branches
        .iter()
        .map(|b| width_of(b))
        .max()
        .unwrap_or(6)
        .max(6);
    println!(
        "{}",
        format!(
            "{}  {}  {:<10}  {:>6}  {:>5}  LAST",
            pad("REPO", nw),
            pad("BRANCH", bw),
            "STATE",
            "SYNCED",
            "EVERY"
        )
        .dimmed()
    );
    for ((((r, i), name), branch), x) in list
        .iter()
        .zip(&infos)
        .zip(&names)
        .zip(&branches)
        .zip(&excluded)
    {
        let e = st.repos.get(&r.path.display().to_string());
        let synced = e
            .and_then(|e| e.last_fetch)
            .map(|t| ago(Some(t)))
            .unwrap_or_else(|| "never".into());
        let last = match e.and_then(|e| e.outcome.map(|o| (o, e.detail.clone()))) {
            Some((o, Some(d))) if o.needs_attention() || o == Outcome::FetchFailed => {
                format!("{} {}", outcome_text(o), fit(&d, 60).dimmed())
            }
            Some((o, _)) => outcome_text(o).to_string(),
            None => "-".dimmed().to_string(),
        };
        let busy = i
            .busy
            .as_deref()
            .map(|b| format!(" {b}!"))
            .unwrap_or_default();
        println!(
            "{}  {}  {:<10}  {:>6}  {:>5}  {last}",
            pad(name, nw),
            pad(branch, bw),
            format!("{}{busy}", position(i)),
            synced,
            if *x { "off".into() } else { span(r.every) },
        );
    }
    Ok(())
}

pub struct SyncOpts {
    /// Repo names or paths; empty = all.
    pub names: Vec<String>,
    /// Only repos whose interval is up.
    pub due: bool,
    /// macOS notification for new problems.
    pub notify: bool,
    /// Print only updates and problems, timestamped (for the service log).
    pub quiet: bool,
    pub json: bool,
}

fn line(label: &str, r: &SyncResult) -> String {
    let mut s = format!("{label}: {}", outcome_text(r.outcome));
    if r.pulled > 0 {
        s.push_str(&format!(" +{}", r.pulled));
    }
    if let Some(b) = &r.branch
        && r.outcome != Outcome::Current
    {
        s.push_str(&format!(" ({b})"));
    }
    if let Some(db) = &r.default_branch {
        s.push_str(&format!(", {} +{}", db.branch, db.pulled));
    }
    if let Some(d) = &r.detail {
        s.push_str(&format!(" — {d}"));
    }
    s
}

pub fn sync(o: SyncOpts) -> Result<()> {
    let s = Settings::load();
    warn_problems(&s);
    let Some(_lock) = repos::lock()? else {
        if o.quiet {
            return Ok(());
        }
        return Err(Error::exit(1, "another `fleet repos sync` is running"));
    };
    let state_path = repos::state_path();
    let mut st = repos::load_state(&state_path);
    let now = chrono::Local::now();
    let mut list = select(repos::discover(&s), &o.names)?;
    if o.due {
        list.retain(|r| {
            repos::is_due_state(r.every, st.repos.get(&r.path.display().to_string()), &now)
        });
    }
    if list.is_empty() {
        if o.json {
            println!("[]");
        } else if !o.quiet {
            println!("nothing to sync");
        }
        return Ok(());
    }
    let results = repos::sync_all(&list);
    let alerts = repos::record(&mut st, &results, chrono::Utc::now().timestamp_millis());
    repos::save_state(&state_path, &st)?;

    if o.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        let names = labels(&list);
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
        for (r, name) in results.iter().zip(&names) {
            let interesting = r.outcome != Outcome::Current || r.default_branch.is_some();
            if o.quiet && !interesting {
                continue;
            }
            if o.quiet {
                println!("{stamp} {}", line(name, r));
            } else {
                println!("{}", line(name, r));
            }
        }
        if !o.quiet {
            let n = |f: &dyn Fn(&SyncResult) -> bool| results.iter().filter(|r| f(r)).count();
            println!(
                "{}",
                format!(
                    "{} repos: {} updated, {} need attention, {} failed",
                    results.len(),
                    n(&|r| r.pulled > 0 || r.default_branch.is_some()),
                    n(&|r| r.outcome.needs_attention()),
                    n(&|r| r.outcome == Outcome::FetchFailed),
                )
                .dimmed()
            );
        }
    }
    if o.notify && !alerts.is_empty() {
        let msg = alerts
            .iter()
            .map(|r| format!("{}: {}", r.name, r.outcome.label()))
            .collect::<Vec<_>>()
            .join(", ");
        crate::tui::notify::notify("fleet repos", &msg);
    }
    Ok(())
}

/// The launchd agent that runs `fleet repos sync --due` every [`repos::TICK_SECS`].
pub fn launchd_plist(exe: &Path, config_path: &Path, path_env: &str, log: &Path) -> String {
    let e = |p: &Path| web::xml_escape(&p.display().to_string());
    // Standard, not Background: a background job's I/O is throttled hard enough that a
    // catch-up fetch of a stale checkout crawls.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--local</string>
    <string>repos</string>
    <string>sync</string>
    <string>--due</string>
    <string>--notify</string>
    <string>--quiet</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>FLEET_CONFIG</key><string>{cfg}</string>
    <key>PATH</key><string>{path}</string>
  </dict>
  <key>ProcessType</key><string>Standard</string>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>{tick}</integer>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = e(exe),
        cfg = e(config_path),
        path = web::xml_escape(path_env),
        tick = repos::TICK_SECS,
        log = e(log),
    )
}

pub fn install_service(o: ServiceOpts) -> Result<()> {
    tools::deny_path_only("`fleet repos install-service`")?;
    if !cfg!(target_os = "macos") {
        return Err(Error::exit(
            1,
            format!(
                "install-service is macOS-only — elsewhere run `fleet repos sync --due --quiet` every {}m from cron or a systemd timer",
                repos::TICK_SECS / 60
            ),
        ));
    }
    let exe = crate::cli::hosts::current_exe()?;
    let home = dirs::home_dir().ok_or_else(|| Error::Other("no home directory".into()))?;
    let log = home.join(SERVICE_LOG);
    // git, plus whatever its credential helpers / ssh need (Homebrew first).
    let git = tools::find_binary("git").unwrap_or_else(|| "/usr/bin/git".into());
    let path = web::service_path(&git, &[tools::find_binary("gh")]);
    let plist = launchd_plist(&exe, &config::path(), &path, &log);
    web::launchd_agent(LAUNCHD_LABEL, &plist, &log, &o)?;
    if !o.uninstall && !o.print && !o.no_load && !crate::core::hosts::dry_run() {
        println!(
            "syncs due repos every {}m (and at login) — see them with: fleet repos",
            repos::TICK_SECS / 60
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(span(1800), "30m");
        assert_eq!(span(3600), "1h");
        assert_eq!(span(86_400), "24h");
        assert_eq!(span(7 * 86_400), "7d");
    }

    #[test]
    fn plist_runs_sync_due_on_a_timer() {
        let p = launchd_plist(
            Path::new("/x/fleet"),
            Path::new("/c.json"),
            "/usr/bin:/bin",
            Path::new("/l.log"),
        );
        assert!(p.contains("<string>repos</string>"));
        assert!(p.contains("<string>--due</string>"));
        assert!(p.contains("<string>--local</string>"));
        assert!(p.contains(&format!("<integer>{}</integer>", repos::TICK_SECS)));
        assert!(p.contains(LAUNCHD_LABEL));
    }

    #[test]
    fn duplicate_names_get_paths() {
        let r = |p: &str| Repo {
            name: Path::new(p).file_name().unwrap().to_string_lossy().into(),
            path: p.into(),
            every: 60,
        };
        let l = labels(&[r("/a/x"), r("/b/x"), r("/a/y")]);
        assert_eq!(l, vec!["/a/x", "/b/x", "y"]);
    }
}
