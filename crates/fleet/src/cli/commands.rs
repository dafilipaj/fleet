//! One function per CLI verb. `watch` lives in its own module.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use colored::Colorize;

use crate::cli::render::{home_rel, plain_table, tail, term_width};
use crate::core::backend::{self, Prompt};
use crate::core::config;
use crate::core::discovery::{self, Backend, Session, claude_home};
use crate::core::harness::{self, Harness};
use crate::core::naming;
use crate::core::title;
use crate::error::{Error, Result};

pub use crate::core::config::{NamingConfig, UiConfig};

/// The config pieces the session verbs and the dashboard read.
pub struct FleetConfig {
    pub ui: UiConfig,
    pub naming: NamingConfig,
}

/// Everything one read of the config yields.
pub struct Loaded {
    pub cfg: FleetConfig,
    /// `Some(message)` when the file exists but doesn't parse. Loading degrades
    /// to defaults on purpose, so without this the user never learns why their
    /// settings stopped applying — or why `z` no longer sticks.
    pub problem: Option<String>,
}

pub fn load_all() -> Loaded {
    let l = config::load();
    Loaded {
        cfg: FleetConfig {
            ui: l.config.tui.clone(),
            naming: l.config.naming.clone(),
        },
        problem: l
            .problem
            .as_ref()
            .map(|p| format!("config not readable: {}", home_rel(p))),
    }
}

/// `tui` config, or all-defaults when there's no config file.
pub fn ui_config() -> UiConfig {
    config::get().tui.clone()
}

/// `naming` config, or all-defaults when there's no config file.
pub fn naming_config() -> NamingConfig {
    config::get().naming.clone()
}

/// Write one `tui` key back, leaving the rest of the file alone. Best-effort by
/// contract: the caller reports the error, a dashboard toggle never takes the TUI
/// down over an unwritable or malformed config.
pub fn persist_ui(key: &str, value: &str) -> std::result::Result<(), String> {
    config::patch(
        &format!("tui.{key}"),
        serde_json::Value::String(value.into()),
    )
    .map_err(|e| e.to_string())
}

/// Resolve the command used to launch Claude in a spawned session. Hosts differ:
/// some invoke `claude`, others a wrapper/alias. Precedence: `FLEET_CMD` env var
/// → `claude` in the config → `claude`.
pub(crate) fn resolve_launcher() -> String {
    Harness::Claude.command()
}

/// A `--model` id is typed into a shell: letters, digits and `._[]-/:` only (the
/// web server enforces the same set; pi's ids are `provider/model[:thinking]`).
/// Blank means the harness's own default.
pub(crate) fn clean_model(raw: &str) -> Result<Option<String>> {
    let m = raw.trim();
    if m.is_empty() {
        return Ok(None);
    }
    if m.len() > 100
        || !m
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._[]-/:".contains(c))
    {
        return Err(Error::Other(format!(
            "invalid model id \"{m}\" — letters, digits and . _ [ ] - / : only"
        )));
    }
    Ok(Some(m.to_string()))
}

/// The launcher with `--model <id>` appended when one was picked.
pub(crate) fn with_model(launcher: &str, model: Option<&str>) -> String {
    match model {
        Some(m) => format!("{launcher} --model {}", crate::core::tools::shq(m)),
        None => launcher.to_string(),
    }
}

/// `fleet harness list`: what this host can spawn, and the models each one offers.
pub fn harness_list(json: bool, with_models: bool) -> Result<()> {
    let rows: Vec<harness::HarnessInfo> = Harness::ALL
        .iter()
        .map(|h| harness::info(*h, with_models))
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for r in &rows {
        let state = if r.installed {
            "installed".green()
        } else {
            "not found".red()
        };
        let models = if with_models && r.installed {
            format!(
                "  {} models",
                r.models.iter().filter(|m| !m.id.is_empty()).count()
            )
        } else {
            String::new()
        };
        println!(
            "{:<7} {:<10} {}{}",
            r.id.bold(),
            state,
            r.command.dimmed(),
            models.dimmed()
        );
    }
    Ok(())
}

/// The name rows are tagged with: `--as-host` (set by the dispatcher), else
/// config `self`.
pub fn host_label() -> String {
    crate::core::hosts::as_host().unwrap_or_else(|| config::get().self_name())
}

/// The rows `list` prints: discovered, tab-enriched, titled, host-tagged.
pub fn list_rows() -> Vec<Session> {
    // Checked, not `discover()`: an unreadable registry must not be recorded as "nothing
    // running" (the snapshot would forget the whole fleet).
    let (mut rows, discovered) = match discovery::discover_checked() {
        Ok(rows) => (rows, true),
        Err(e) => {
            crate::core::hosts::debug(&format!("discovery failed: {e}"));
            (Vec::new(), false)
        }
    };
    // The terminal a session lives in is part of what `list` is for; without this
    // every iTerm-backed row renders as `-`.
    discovery::enrich_iterm_tabs(&mut rows);
    // Titles the dashboard has already paid for, so `--json` carries what a
    // session is *doing* and not only what it's called. Read-only: `list` never
    // generates one.
    naming::stamp_titles(&mut rows);
    // The one title each view draws, computed once here (docs/architecture.md).
    title::stamp_display_titles(&mut rows);
    // Which session stack each row is in (read-only: no sync, no writes).
    crate::core::stack::stamp_stacks(&mut rows);
    // What runs now, for session recovery after a reboot (core::snapshot). Never fails the
    // listing; a no-op in fixture mode and when nothing changed.
    if discovered {
        crate::core::snapshot::record_quietly(&rows);
    }
    let host = host_label();
    for r in rows.iter_mut() {
        r.host = Some(host.clone());
    }
    rows
}

/// A `--json` command failing: print `{ ok: false, host, kind, error }` on stdout, then
/// hand the error back (it still goes to stderr and sets the exit code).
pub fn json_failure(e: Error) -> Error {
    let mut v = e.json();
    v["host"] = host_label().into();
    println!(
        "{}",
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
    );
    e
}

pub fn list(json: bool) -> Result<()> {
    let rows = list_rows();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("{}", plain_table(&rows));
        let dormant = crate::core::snapshot::dormant_count();
        if dormant > 0 {
            println!("{}", format!("{dormant} dormant (fleet restore)").dimmed());
        }
        println!();
    }
    Ok(())
}

pub fn peek(target: &str, lines: usize) -> Result<()> {
    let s = discovery::resolve_target(target)?;
    let text = backend::peek(&s)?;
    let where_ = s.cwd.as_deref().map(home_rel).unwrap_or_default();
    println!(
        "\n{} {} ({}, {}) — {}",
        "▼".cyan(),
        s.headline().bold(),
        s.backend.label(),
        s.status,
        where_.dimmed()
    );
    let rule = "─".repeat(term_width().clamp(40, 200));
    println!("{rule}");
    println!("{}", tail(&text, lines, term_width().saturating_sub(2)));
    println!("{rule}\n");
    Ok(())
}

pub fn send(target: &str, text: &str) -> Result<()> {
    let s = discovery::resolve_target(target)?;
    backend::send(&s, text)?;
    println!(
        "sent to {} ({}): {}",
        s.headline().bold(),
        s.backend.label(),
        text
    );
    Ok(())
}

/// `fleet keys <target> <key>...`: press named keys in the session's terminal — Escape to
/// interrupt a turn, arrows and Enter to answer a menu. `--json`: `{ ok, host, target:
/// { session_id, pid }, keys }`, or `{ ok: false, host, kind, error }` (exit 1).
pub fn keys(target: &str, names: &[String], json: bool) -> Result<()> {
    let run = || -> Result<(Session, Vec<backend::Key>)> {
        let keys = parse_keys(names)?;
        let s = discovery::resolve_target(target)?;
        backend::keys(&s, &keys)?;
        Ok((s, keys))
    };
    let (s, keys) = match run() {
        Ok(done) => done,
        Err(e) if json => return Err(json_failure(e)),
        Err(e) => return Err(e),
    };
    let names: Vec<&str> = keys.iter().map(|k| k.name()).collect();
    if json {
        let v = serde_json::json!({
            "ok": true,
            "host": host_label(),
            "target": { "session_id": s.session_id, "pid": s.pid },
            "keys": names,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "keys to {} ({}): {}",
            s.headline().bold(),
            s.backend.label(),
            names.join(" ")
        );
    }
    Ok(())
}

/// Every name must be one of [`backend::Key::ALL`]; one that is not sends nothing at all.
fn parse_keys(names: &[String]) -> Result<Vec<backend::Key>> {
    if names.is_empty() {
        return Err(Error::tagged("invalid_key", "no keys given"));
    }
    names
        .iter()
        .map(|n| {
            backend::Key::parse(n).ok_or_else(|| {
                let all: Vec<&str> = backend::Key::ALL.iter().map(|k| k.name()).collect();
                Error::tagged(
                    "invalid_key",
                    format!("unknown key \"{n}\" — one of {}", all.join(", ")),
                )
            })
        })
        .collect()
}

pub use crate::core::title::{
    Hold, MAX_TITLE as MAX_RENAME, RenameOpts, RenameOutcome, apply_rename,
    clean_title as clean_name,
};

/// What `fleet rename --json` prints — one object, whatever happened.
#[derive(serde::Serialize)]
pub struct RenameReport {
    pub ok: bool,
    /// `renamed` (confirmed in the registry), `sent` (typed, not reflected yet)
    /// or `held` (nothing sent — see `held`).
    pub result: &'static str,
    pub session_id: Option<String>,
    pub pid: i64,
    pub host: String,
    /// The display title before the rename.
    pub from: String,
    /// The title asked for (trimmed and capped).
    pub title: String,
    /// `busy` / `waiting` when held.
    pub held: Option<Hold>,
    /// The tmux side; `null` when sync was off or nothing was sent.
    pub tmux: Option<TmuxReport>,
    /// One human line saying what happened.
    pub message: String,
}

#[derive(serde::Serialize)]
pub struct TmuxReport {
    pub renamed: bool,
    pub from: Option<String>,
    pub to: Option<String>,
    pub note: String,
}

impl From<&backend::TmuxSync> for TmuxReport {
    fn from(t: &backend::TmuxSync) -> Self {
        match t {
            backend::TmuxSync::Renamed { from, to } => TmuxReport {
                renamed: true,
                from: Some(from.clone()),
                to: Some(to.clone()),
                note: t.to_string(),
            },
            backend::TmuxSync::Skipped(why) => TmuxReport {
                renamed: false,
                from: None,
                to: None,
                note: why.clone(),
            },
        }
    }
}

/// Rename a session: Claude's own `/rename` (the source of truth), then the
/// tmux session name derived from it — one call. The registry (and so every
/// fleet view) picks the new title up on its next status write.
///
/// Held (busy / waiting) is an error: exit 3 with `--json` (the report is on
/// stdout), exit 1 otherwise. Nothing is queued — the caller retries when the
/// session is idle, or passes `--force`.
pub fn rename(target: &str, name: &str, no_tmux_sync: bool, force: bool, json: bool) -> Result<()> {
    let s = match discovery::resolve_target(target) {
        Ok(s) => s,
        Err(e) if json => return Err(json_failure(e.into())),
        Err(e) => return Err(e.into()),
    };
    let name = clean_name(name)?;
    let mut rows = vec![s];
    naming::stamp_titles(&mut rows);
    let s = rows.remove(0);
    let was = s.headline();
    let opts = RenameOpts {
        sync_tmux: !no_tmux_sync && naming_config().sync_tmux(),
        force,
    };
    let mut report = RenameReport {
        ok: false,
        result: "held",
        session_id: s.session_id.clone(),
        pid: s.pid,
        host: host_label(),
        from: was.clone(),
        title: name.clone(),
        held: None,
        tmux: None,
        message: String::new(),
    };
    let outcome = apply_rename(&s, &name, opts)?;
    let tmux_note = outcome.tmux_note();
    match &outcome {
        RenameOutcome::Held(h, why) => {
            report.held = Some(*h);
            report.message = why.clone();
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Err(Error::exit(3, ""));
            }
            return Err(Error::Other(why.clone()));
        }
        RenameOutcome::Sent(t) => {
            report.tmux = t.as_ref().map(TmuxReport::from);
            if !json && let Some(note) = &tmux_note {
                println!("{}", note.dimmed());
            }
        }
    }

    // Confirm from the registry rather than trusting the keystrokes landed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut confirmed = false;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        if let Some(now) = discovery::discover().iter().find(|r| r.key() == s.key())
            && now.name.as_deref() == Some(name.as_str())
        {
            confirmed = true;
            break;
        }
    }
    report.ok = true;
    if confirmed {
        report.result = "renamed";
        report.message = format!("{was} → {name}");
    } else {
        report.result = "sent";
        report.message =
            format!("sent `/rename {name}` to {was} — not reflected yet, check `fleet list`");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if confirmed {
        println!("{} → {}", was.dimmed(), name.bold());
    } else {
        println!("{}", report.message);
    }
    Ok(())
}

// --- name ---------------------------------------------------------------------

/// How `fleet name` was asked to run.
#[derive(Default, Clone, Copy)]
pub struct NameOpts {
    /// Every session whose name is still Claude's cwd+hash fallback.
    pub all: bool,
    /// Actually send `/rename`. Without it this prints suggestions and stops.
    pub apply: bool,
    /// The explicit form of the default — suggest, touch nothing.
    pub dry_run: bool,
    pub no_tmux_sync: bool,
    /// Ignore the cached name and overwrite it with a fresh one.
    pub refresh: bool,
}

/// Suggest (and optionally apply) LLM-generated names.
///
/// The scriptable half of the `N` / `Ctrl-N` keys: same generator, same cache,
/// same guards, usable from a skill or a shell without opening the dashboard.
/// What the `name` verb is allowed to do, from the config and the flags.
///
/// The only thing that stops `naming.enabled = false` from spawning a
/// `claude` per session: the headline it prints says nothing will happen, and
/// for a while that was all it was.
fn gen_opts(cfg: &NamingConfig, o: NameOpts, fixture: bool) -> naming::GenOpts {
    naming::GenOpts {
        allow_llm: cfg.enabled() && !fixture,
        refresh: o.refresh,
    }
}

pub fn name(target: Option<String>, o: NameOpts) -> Result<()> {
    let cfg = naming_config();
    let how = gen_opts(&cfg, o, discovery::is_fixture());
    let targets: Vec<Session> = match (&target, o.all) {
        (Some(t), false) => vec![discovery::resolve_target(t)?],
        (None, true) => discovery::discover()
            .into_iter()
            .filter(Session::is_derived_name)
            .collect(),
        (Some(_), true) => {
            return Err(Error::Other("pass a target or --all, not both".into()));
        }
        (None, false) => {
            return Err(Error::Other(
                "name what? pass a session target (sessionId prefix, name, or pid) or --all".into(),
            ));
        }
    };
    if targets.is_empty() {
        println!("no sessions still carry a Claude-derived name — nothing to rename");
        return Ok(());
    }

    let apply = o.apply && !o.dry_run;
    let rename_opts = RenameOpts {
        sync_tmux: !o.no_tmux_sync && cfg.sync_tmux(),
        force: false,
    };
    // Off in config, or a canned fleet: the heuristic answers and no child is
    // ever spawned — `how.allow_llm` is what carries that all the way to
    // `naming::suggest`, which is the only thing that can actually enforce it.
    if !how.allow_llm {
        println!(
            "{}",
            "naming is inert here (fixture mode or naming.enabled = false) — showing the branch/title guess"
                .dimmed()
        );
    }

    // A still-unnamed session alone in a tmux session somebody named takes that
    // name as its title (no model call): the tmux name is derived from the title
    // now, so generating one would clobber the name the user picked.
    let mut results = Vec::new();
    let mut rest = Vec::new();
    for s in targets {
        match title::adoptable_tmux_name(&s) {
            Some(n) => results.push((
                s,
                Ok(naming::Suggestion {
                    name: n,
                    source: naming::NameSource::Tmux,
                    note: None,
                }),
            )),
            None => rest.push(s),
        }
    }
    if !rest.is_empty() {
        results.extend(generate(&rest, &cfg, how));
    }
    for (s, result) in results {
        let was = s.label();
        let suggestion = match result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{} {}: {}", "✕".red(), was.bold(), e.dimmed());
                continue;
            }
        };
        let new = &suggestion.name;
        if !apply {
            println!(
                "{}  →  {}   {}",
                was.dimmed(),
                new.bold(),
                format!("({})", suggestion.source.label()).dimmed()
            );
            continue;
        }
        match apply_rename(&s, new, rename_opts) {
            Ok(outcome @ RenameOutcome::Sent(_)) => {
                println!("{}  →  {}", was.dimmed(), new.bold());
                if let Some(note) = outcome.tmux_note() {
                    println!("   {}", note.dimmed());
                }
            }
            Ok(RenameOutcome::Held(_, why)) => println!("{} {}", "⏸".yellow(), why.dimmed()),
            Err(e) => eprintln!("{} {was}: {e}", "✕".red()),
        }
    }
    Ok(())
}

/// Generate a suggestion per session, two model calls in flight at a time.
/// Order follows `targets` so the output is stable.
fn generate(
    targets: &[Session],
    cfg: &NamingConfig,
    how: naming::GenOpts,
) -> Vec<(Session, std::result::Result<naming::Suggestion, String>)> {
    let (pool, rx) = naming::NamePool::start(cfg.model(), how);
    for s in targets {
        pool.enqueue(naming::NameJob {
            key: s.key(),
            session: s.clone(),
            purpose: naming::Purpose::Bulk,
        });
    }
    let mut by_key: HashMap<String, std::result::Result<naming::Suggestion, String>> =
        HashMap::new();
    // Counted by answers collected, not by messages read: `Unavailable` is a
    // one-off aside and must not consume a session's slot.
    while by_key.len() < targets.len() {
        match rx.recv() {
            Ok(naming::NameMsg::Named {
                key, name, source, ..
            }) => {
                by_key.insert(
                    key,
                    Ok(naming::Suggestion {
                        name,
                        source,
                        note: None,
                    }),
                );
            }
            Ok(naming::NameMsg::Failed { key, err, .. }) => {
                by_key.insert(key, Err(err));
            }
            // Said once per run by contract; the per-session lines carry the
            // detail that matters here.
            Ok(naming::NameMsg::Unavailable(_)) => continue,
            // Every worker is gone — nothing more is coming.
            Err(_) => break,
        }
    }
    drop(pool);
    targets
        .iter()
        .map(|s| {
            let r = by_key
                .remove(&s.key())
                .unwrap_or_else(|| Err("no answer from the naming worker".into()));
            (s.clone(), r)
        })
        .collect()
}

/// Everything `spawn` and `handoff` share about *where* the new session lands.
#[derive(Default)]
pub struct SpawnOpts {
    pub dir: Option<String>,
    pub backend: Option<Backend>,
    /// Claude display name for the new session (`claude -n`).
    pub name: Option<String>,
    /// Model for the new session (`<harness> --model <id>`); None = the harness's default.
    pub model: Option<String>,
    /// Which coding agent to launch (default Claude).
    pub harness: Harness,
    pub tmux_session: Option<String>,
    pub window: bool,
}

impl SpawnOpts {
    /// Fill in the defaults that need the environment: cwd, and the backend we're
    /// sitting in. Also validates, so a bad dir/name fails before anything opens.
    pub(crate) fn resolve(&self) -> Result<(String, Backend, Option<String>)> {
        let dir = self
            .dir
            .as_deref()
            .map(crate::core::tools::expand_tilde)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".into())
            });
        if !Path::new(&dir).exists() {
            return Err(Error::tagged(
                "dir_missing",
                format!("dir does not exist: {dir}"),
            ));
        }
        let backend = self.backend.unwrap_or_else(default_backend);
        let name = self
            .name
            .as_deref()
            .map(clean_name)
            .transpose()
            .map_err(|e| e.tag("name_invalid"))?;
        Ok((dir, backend, name))
    }
}

/// Where a new session goes when nobody said: tmux when we are inside tmux, or
/// reached over ssh (a remote host's iTerm is on a screen nobody is looking
/// at), or not on macOS; iTerm otherwise.
fn default_backend() -> Backend {
    if std::env::var_os("TMUX").is_some()
        || std::env::var_os("SSH_CONNECTION").is_some()
        || !cfg!(target_os = "macos")
    {
        Backend::Tmux
    } else {
        Backend::Iterm
    }
}

/// `fleet spawn`. With `json`, stdout carries one object either way: what was spawned
/// ([`spawned_json`]), or `{ ok: false, kind, error }` — and the exit code is non-zero.
pub fn spawn(
    prompt: Option<String>,
    from: Option<String>,
    opts: SpawnOpts,
    json: bool,
) -> Result<()> {
    let out = match from {
        Some(src) => spawn_from(&src, prompt, opts, json),
        None => spawn_new(prompt, opts, json),
    };
    if json && let Err(e) = &out {
        println!("{}", spawn_error_json(e, &host_label()));
    }
    out
}

/// The model id a spawn runs, validated (`None` = the harness's default).
fn spawn_model(opts: &SpawnOpts) -> Result<Option<String>> {
    opts.model
        .as_deref()
        .map(clean_model)
        .transpose()
        .map(Option::flatten)
        .map_err(|e| e.tag("model_invalid"))
}

/// Refuse to type a launcher that cannot start: the shell would just print "command not
/// found" in a session that looks spawned. Judged from `dir`, where the shell will run it.
fn require_launcher(harness: Harness, launcher: &str, dir: &str) -> Result<()> {
    if harness::check_launcher(launcher, Path::new(dir)) != harness::LauncherCheck::Missing {
        return Ok(());
    }
    let word = launcher.split_whitespace().next().unwrap_or(launcher);
    let why = if word.contains('/') {
        "is not an executable file (relative to the spawn's --dir)"
    } else {
        "is neither on PATH nor known to a login shell"
    };
    Err(Error::tagged(
        "harness_missing",
        format!(
            "{} cannot start here: `{word}` {why} (launcher `{launcher}`)",
            harness.label()
        ),
    ))
}

/// `fleet spawn --json` on success. `backend_kind` is the backend a dry run would use; a real
/// spawn reports the one it used.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawned_json(
    host: &str,
    s: Option<&backend::Spawned>,
    backend_kind: Backend,
    dir: &str,
    harness: Harness,
    model: Option<&str>,
    name: Option<&str>,
    command: &str,
    from: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "dry_run": s.is_none(),
        "host": host,
        "backend": s.map_or(backend_kind, |s| s.backend).label(),
        "tmux_session": s.and_then(|s| s.tmux_session.clone()),
        "tmux_window": s.and_then(|s| s.tmux_window.clone()),
        "handle": s.and_then(|s| s.handle.clone()),
        "created_session": s.is_some_and(|s| s.created_session),
        "dir": dir,
        "harness": harness.id(),
        "model": model,
        "name": name,
        "command": s.map(|s| s.command.as_str()).unwrap_or(command),
        "prompt_file": s.and_then(|s| s.prompt_file.clone()),
        "from": from,
    })
}

/// `spawn --json` failed before it reached a host (an unknown `-H`, a `--dir` that cannot
/// travel): print the failure object for `host` and hand the error back.
pub fn spawn_json_failure(e: Error, host: &str) -> Error {
    println!("{}", spawn_error_json(&e, host));
    e
}

/// Longest a remote `spawn --json` may take: the ssh hop, plus a login-shell launcher check.
const REMOTE_SPAWN_TIMEOUT: Duration = Duration::from_secs(90);

/// `fleet -H <host> spawn --json`: run it there and print exactly one object here.
///
/// - `-n`: nothing is contacted; the object names the command that would run.
/// - The host's own `--json` answer is passed through as it is (with its exit code).
/// - ssh failing to connect: `host_unreachable` (exit 4); fleet missing there:
///   `fleet_missing` (127); a hop over [`REMOTE_SPAWN_TIMEOUT`]: `remote_timeout`; anything
///   else that is not a JSON object (an older fleet without `--json`): `remote_failed`; ssh
///   that cannot even be started (not on PATH): `ssh_failed` (exit 1).
pub fn spawn_remote_json(r: &crate::core::hosts::Remote, args: &[String]) -> Result<i32> {
    use crate::core::hosts;
    let c = hosts::ssh_command(
        &r.dest,
        &hosts::remote_fleet_command(r, args),
        hosts::Tty::Never,
    );
    if hosts::dry_run() {
        let v = serde_json::json!({
            "ok": true,
            "dry_run": true,
            "host": r.name,
            "remote": true,
            "command": hosts::display_command(&c),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
    }
    let out = match hosts::capture_remote(r, args, REMOTE_SPAWN_TIMEOUT) {
        Ok(out) => out,
        // ssh itself could not be started (not installed, not on PATH): still one object.
        Err(e) => {
            return Err(spawn_json_failure(
                Error::tagged("ssh_failed", format!("spawn on {}: {e}", r.name)),
                &r.name,
            ));
        }
    };
    if !out.stderr.trim().is_empty() {
        eprint!("{}", out.stderr);
    }
    let answer = serde_json::from_str::<serde_json::Value>(out.stdout.trim())
        .ok()
        .filter(serde_json::Value::is_object);
    if let (Some(v), false) = (answer, out.timed_out) {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(out.code.unwrap_or(1));
    }
    let (kind, code) = match (out.timed_out, out.code) {
        (true, _) => ("remote_timeout", 1),
        (_, Some(255)) => ("host_unreachable", hosts::EXIT_UNREACHABLE),
        (_, Some(127)) => ("fleet_missing", 127),
        (_, c) => ("remote_failed", c.filter(|c| *c != 0).unwrap_or(1)),
    };
    let msg = if kind == "host_unreachable" {
        hosts::unreachable_message(r)
    } else {
        format!("spawn on {}: {}", r.name, out.why(r))
    };
    println!(
        "{}",
        spawn_error_json(&Error::tagged(kind, msg.clone()), &r.name)
    );
    eprintln!("fleet: {msg}");
    Ok(code)
}

/// `fleet spawn --json` on failure. `partial`: where the terminal is when it was created before
/// the spawn failed (so the caller can find and close it), else `null`.
pub(crate) fn spawn_error_json(e: &Error, host: &str) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "host": host,
        "kind": e.kind(),
        "error": e.to_string(),
        "partial": e.partial().map(|p| serde_json::json!({
            "backend": p.backend.label(),
            "tmux_session": p.tmux_session,
            "tmux_window": p.tmux_window,
            "handle": p.handle,
            "created_session": p.created_session,
            "command": p.command,
            "prompt_file": p.prompt_file,
        })),
    })
}

fn spawn_new(prompt: Option<String>, opts: SpawnOpts, json: bool) -> Result<()> {
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = spawn_model(&opts)?;
    let prompt = prompt.unwrap_or_default();
    let launcher = with_model(&opts.harness.command(), model.as_deref());
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::Inline(&prompt),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
        let backend_kind = backend::spawn_backend(backend_kind);
        if json {
            let v = spawned_json(
                &host_label(),
                None,
                backend_kind,
                &dir,
                opts.harness,
                model.as_deref(),
                name.as_deref(),
                &line,
                None,
            );
            println!("{}", serde_json::to_string_pretty(&v)?);
        } else {
            println!(
                "dry-run: would spawn a {} session: {line}",
                backend_kind.label()
            );
        }
        return Ok(());
    }
    require_launcher(opts.harness, &launcher, &dir)?;
    let desc = backend::spawn(
        backend_kind,
        &dir,
        Prompt::Inline(&prompt),
        name.as_deref(),
        opts.tmux_session.as_deref(),
        opts.window,
        &launcher,
        opts.harness,
    )?;
    if json {
        let v = spawned_json(
            &host_label(),
            Some(&desc),
            backend_kind,
            &dir,
            opts.harness,
            model.as_deref(),
            name.as_deref(),
            "",
            None,
        );
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else if prompt.is_empty() {
        println!("{desc} in {}", home_rel(&dir));
    } else {
        println!("{desc} in {} — \"{prompt}\"", home_rel(&dir));
    }
    Ok(())
}

/// `spawn --from <session>`: a new session in the source's cwd (on this host — `-H` already
/// brought us to the source's host) whose first prompt is the brief's continue prompt, plus
/// `extra` after a blank line. The prompt goes through a file, like a handoff brief: it is
/// multi-line, and the file stays as the record of what was handed on.
fn spawn_from(source: &str, extra: Option<String>, mut opts: SpawnOpts, json: bool) -> Result<()> {
    let c = crate::cli::brief::continue_from(source)?;
    if opts.dir.is_none() {
        opts.dir = Some(c.cwd.clone().ok_or_else(|| {
            Error::tagged(
                "dir_missing",
                format!("no working directory known for {} — pass --dir", c.label),
            )
        })?);
    }
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = spawn_model(&opts)?;
    let from = serde_json::json!({ "session_id": c.id, "label": c.label });
    let mut text = c.prompt.clone();
    if let Some(e) = extra.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        text.push_str("\n\n");
        text.push_str(e);
    }
    let now = chrono::Local::now();
    let short: String = c.id.chars().take(8).collect();
    let path = handoff_dir().join(format!(
        "{}-continue-{short}.md",
        now.format("%Y%m%d-%H%M%S")
    ));
    let path_s = path.display().to_string();
    let launcher = with_model(&opts.harness.command(), model.as_deref());
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::File(&path_s),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
        let backend_kind = backend::spawn_backend(backend_kind);
        if json {
            let mut v = spawned_json(
                &host_label(),
                None,
                backend_kind,
                &dir,
                opts.harness,
                model.as_deref(),
                name.as_deref(),
                &line,
                Some(from),
            );
            v["prompt_file"] = path_s.clone().into();
            println!("{}", serde_json::to_string_pretty(&v)?);
            return Ok(());
        }
        println!(
            "dry-run: would spawn a {} session continuing {}: {line}",
            backend_kind.label(),
            c.label
        );
        println!(
            "dry-run: first prompt ({} — not written):\n{text}",
            home_rel(&path_s)
        );
        return Ok(());
    }
    require_launcher(opts.harness, &launcher, &dir)?;
    std::fs::create_dir_all(handoff_dir())?;
    std::fs::write(&path, &text)?;
    let desc = backend::spawn(
        backend_kind,
        &dir,
        Prompt::File(&path_s),
        name.as_deref(),
        opts.tmux_session.as_deref(),
        opts.window,
        &launcher,
        opts.harness,
    )?;
    if json {
        let v = spawned_json(
            &host_label(),
            Some(&desc),
            backend_kind,
            &dir,
            opts.harness,
            model.as_deref(),
            name.as_deref(),
            "",
            Some(from),
        );
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "{} {desc} in {} — continuing {} (prompt: {})",
        "→".cyan(),
        home_rel(&dir),
        c.label.bold(),
        home_rel(&path_s).dimmed()
    );
    Ok(())
}

// --- handoff -----------------------------------------------------------------

/// Where handoff briefs are kept. They outlive the spawn on purpose: the record
/// of what was handed off, and a file the new session can re-read at any point.
pub(crate) fn handoff_dir() -> PathBuf {
    claude_home().join("fleet-handoffs")
}

/// A filename-safe stem from the brief's first line. Shares its slugger with
/// the name generator — two spellings of "make this safe to use as a name" is
/// one too many.
fn slug(text: &str) -> String {
    let out = naming::slugify(text, 40);
    if out.is_empty() {
        "handoff".into()
    } else {
        out
    }
}

/// The text the receiving session wakes up to: a line of provenance, then the brief.
fn compose(brief: &str, dir: &str, from: Option<&Session>, stamp: &str) -> String {
    let origin = match from {
        Some(s) => format!(
            "another Claude session ({}, in {})",
            s.label(),
            s.cwd.as_deref().map(home_rel).unwrap_or_else(|| "?".into())
        ),
        None => "another Claude session".to_string(),
    };
    let reply = from
        .map(|s| {
            format!(
                "\nWhen you're done (or blocked), report back with: `fleet send {} \"<your update>\"`.\n",
                s.label()
            )
        })
        .unwrap_or_default();
    format!(
        "You're picking up work handed off from {origin} at {stamp}. \
You're running in {}. Work autonomously within the brief below — where it sets a \
scope or a constraint, that wins over your defaults.\n{reply}\n--- Brief ---\n\n{}\n",
        home_rel(dir),
        brief.trim()
    )
}

/// Read the brief from an explicit argument, a file (`-` = stdin), or piped stdin.
fn read_brief(brief: Option<String>, file: Option<String>) -> Result<String> {
    let text = match (brief, file) {
        (Some(b), _) => b,
        (None, Some(f)) if f == "-" => read_stdin()?,
        (None, Some(f)) => std::fs::read_to_string(&f)
            .map_err(|e| Error::Other(format!("cannot read brief from {f}: {e}")))?,
        (None, None) => read_stdin()?,
    };
    if text.trim().is_empty() {
        return Err(Error::Other(
            "empty brief — pass it as an argument, via --file, or on stdin".into(),
        ));
    }
    Ok(text)
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Same directory, whether or not symlinks and trailing slashes agree.
fn same_dir(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| PathBuf::from(p))
            .display()
            .to_string()
    };
    norm(a) == norm(b)
}

/// Poll the registry for the session that just appeared in `dir`. Claude takes a
/// few seconds to register, so this is worth waiting on — it gives the caller a
/// name to `peek`/`send` with instead of "go look for it".
pub(crate) fn await_new_session(
    before: &HashSet<String>,
    dir: &str,
    timeout: Duration,
) -> Option<Session> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(700));
        if let Some(s) = discovery::discover().into_iter().find(|s| {
            !before.contains(&s.key()) && s.cwd.as_deref().is_some_and(|c| same_dir(c, dir))
        }) {
            return Some(s);
        }
    }
    None
}

pub fn handoff(
    brief: Option<String>,
    file: Option<String>,
    opts: SpawnOpts,
    wait: bool,
) -> Result<()> {
    let brief = read_brief(brief, file)?;
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = opts
        .model
        .as_deref()
        .map(clean_model)
        .transpose()?
        .flatten();

    let from = discovery::origin();
    let now = chrono::Local::now();
    let text = compose(
        &brief,
        &dir,
        from.as_ref(),
        &now.format("%Y-%m-%d %H:%M").to_string(),
    );

    let path = handoff_dir().join(format!(
        "{}-{}.md",
        now.format("%Y%m%d-%H%M%S"),
        slug(&brief)
    ));
    if crate::core::hosts::dry_run() {
        let launcher = with_model(&opts.harness.command(), model.as_deref());
        let path_s = path.display().to_string();
        let line = backend::launch_command(
            &dir,
            Prompt::File(&path_s),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
        println!(
            "dry-run: would hand off to a {} session: {line}",
            backend_kind.label()
        );
        println!(
            "dry-run: brief ({} — not written):\n{text}",
            home_rel(&path_s)
        );
        return Ok(());
    }
    std::fs::create_dir_all(handoff_dir())?;
    std::fs::write(&path, &text)?;

    let before: HashSet<String> = discovery::discover().iter().map(Session::key).collect();
    let launcher = with_model(&opts.harness.command(), model.as_deref());
    let desc = backend::spawn(
        backend_kind,
        &dir,
        Prompt::File(&path.display().to_string()),
        name.as_deref(),
        opts.tmux_session.as_deref(),
        opts.window,
        &launcher,
        opts.harness,
    )?;
    println!(
        "{} {desc} in {} — brief: {}",
        "→".cyan(),
        home_rel(&dir),
        home_rel(&path.display().to_string()).dimmed()
    );

    if wait {
        // Returns as soon as the session appears; the ceiling only bites when the
        // spawn failed. A cold Claude boot behind a profile that loads secrets is
        // comfortably past 30s, so don't set this tight.
        match await_new_session(&before, &dir, Duration::from_secs(75)) {
            Some(s) => println!(
                "  picked up as {} — steer it with `fleet send {} \"…\"`",
                s.label().bold(),
                s.label()
            ),
            None => println!("  (not registered yet — `fleet list` in a moment)"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_json_has_the_contract_fields() {
        let s = backend::Spawned {
            backend: Backend::Tmux,
            desc: "spawned tmux session api".into(),
            tmux_session: Some("api".into()),
            tmux_window: Some("api:0".into()),
            handle: Some("%3".into()),
            created_session: true,
            command: "cd '/w' && codex -- 'go'".into(),
            prompt_file: None,
        };
        let v = spawned_json(
            "laptop",
            Some(&s),
            Backend::Tmux,
            "/w",
            Harness::Codex,
            Some("gpt-6"),
            Some("api"),
            "",
            None,
        );
        assert_eq!(
            v,
            serde_json::json!({
                "ok": true, "dry_run": false, "host": "laptop", "backend": "tmux",
                "tmux_session": "api", "tmux_window": "api:0", "handle": "%3",
                "created_session": true, "dir": "/w", "harness": "codex", "model": "gpt-6",
                "name": "api", "command": "cd '/w' && codex -- 'go'", "prompt_file": null,
                "from": null,
            })
        );
        let e = Error::tagged("name_taken", "taken");
        assert_eq!(
            spawn_error_json(&e, "laptop"),
            serde_json::json!({ "ok": false, "host": "laptop", "kind": "name_taken", "error": "taken", "partial": null })
        );
        // The terminal exists although the spawn failed: where it is comes along.
        let e = Error::Partial {
            source: Box::new(Error::tagged("backend_failed", "send-keys failed")),
            partial: Box::new(s.clone()),
        };
        let v = spawn_error_json(&e, "laptop");
        assert_eq!(v["kind"], "backend_failed");
        assert_eq!(v["error"], "send-keys failed");
        assert_eq!(
            v["partial"],
            serde_json::json!({
                "backend": "tmux", "tmux_session": "api", "tmux_window": "api:0", "handle": "%3",
                "created_session": true, "command": "cd '/w' && codex -- 'go'", "prompt_file": null,
            })
        );
        assert_eq!(e.tag("other").kind(), "backend_failed");
        // Untagged failures still say what they are.
        let e = Error::Other("boom".into());
        assert_eq!(spawn_error_json(&e, "laptop")["kind"], "other");
        assert_eq!(e.tag("backend_failed").kind(), "backend_failed");
        assert_eq!(
            Error::tagged("dir_missing", "x").tag("other").kind(),
            "dir_missing"
        );
    }

    // `enabled = false` printed "nothing will happen here" and then spawned a
    // `claude` per session anyway: nothing carried the setting past this point.
    #[test]
    fn naming_disabled_in_config_never_allows_the_model() {
        let off = NamingConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!gen_opts(&off, NameOpts::default(), false).allow_llm);
        // A fixture is the other way to make it inert.
        assert!(!gen_opts(&NamingConfig::default(), NameOpts::default(), true).allow_llm);
        // The default is the feature doing its job.
        assert!(gen_opts(&NamingConfig::default(), NameOpts::default(), false).allow_llm);
        // …and `--refresh` rides along.
        let refresh = NameOpts {
            refresh: true,
            ..Default::default()
        };
        assert!(gen_opts(&NamingConfig::default(), refresh, false).refresh);
        assert!(!gen_opts(&NamingConfig::default(), NameOpts::default(), false).refresh);
    }

    // A name goes out as `/rename <name>` into a live TUI and then becomes a
    // tmux session name, so neither end wants 300 characters of it.
    #[test]
    fn names_are_trimmed_validated_and_capped() {
        assert_eq!(clean_name("  cache-warmup  ").unwrap(), "cache-warmup");
        assert!(clean_name("   ").is_err());
        assert!(clean_name("two\nlines").is_err());
        let long = clean_name(&"a".repeat(300)).unwrap();
        assert_eq!(long.chars().count(), MAX_RENAME);
        // Character-counted: a multi-byte name must not be split mid-codepoint.
        let wide = clean_name(&"é".repeat(300)).unwrap();
        assert_eq!(wide.chars().count(), MAX_RENAME);
    }

    // The id is typed into a shell after `--model`: nothing but the id charset gets through.
    #[test]
    fn model_ids_are_validated_and_quoted() {
        assert_eq!(
            clean_model("  claude-sonnet-5 ").unwrap().as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            clean_model("claude-opus-5-5[1m]").unwrap().as_deref(),
            Some("claude-opus-5-5[1m]")
        );
        assert_eq!(clean_model("  ").unwrap(), None);
        for bad in ["a b", "x';id'", "$(id)", "a;b", "`id`", &"x".repeat(101)] {
            assert!(clean_model(bad).is_err(), "{bad}");
        }
        assert_eq!(
            with_model("claude", Some("claude-haiku-4-5-20251001")),
            "claude --model 'claude-haiku-4-5-20251001'"
        );
        assert_eq!(with_model("cc", None), "cc");
    }

    #[test]
    fn slugs_come_from_the_first_meaningful_line() {
        assert_eq!(
            slug("# Fix the flaky login test\n\nmore"),
            "fix-the-flaky-login-test"
        );
        assert_eq!(slug("\n\nCache warmup: scale it"), "cache-warmup-scale-it");
        assert_eq!(slug("!!! ???"), "handoff");
        assert!(slug(&"word ".repeat(50)).len() <= 40);
    }

    #[test]
    fn brief_carries_provenance_and_the_reply_path() {
        let text = compose("Do the thing.", "/tmp", None, "2026-08-17 10:00");
        assert!(text.contains("handed off from another Claude session"));
        assert!(text.contains("Do the thing."));
        // No known origin -> nothing to report back to.
        assert!(!text.contains("fleet send"));
    }

    #[test]
    fn known_origin_gets_a_report_back_instruction() {
        let from = Session {
            pid: 1,
            session_id: Some("abcdef123".into()),
            name: Some("app-f9".into()),
            cwd: Some("/tmp".into()),
            status: "busy".into(),
            updated_at: None,
            tty: None,
            backend: Backend::Iterm,
            handle: None,
            tab: None,
            tmux_session: None,
            name_source: None,
            waiting_for: None,
            title: None,
            gen_title: None,
            display_title: None,
            host: None,
            context: None,
            stack: None,
            ..Default::default()
        };
        let text = compose("Do it.", "/tmp", Some(&from), "2026-08-17 10:00");
        assert!(text.contains("(app-f9, in /tmp)"));
        assert!(text.contains("fleet send app-f9"));
    }

    #[test]
    fn brief_sources_are_ordered_and_validated() {
        assert_eq!(
            read_brief(Some("inline".into()), Some("/nope".into())).unwrap(),
            "inline"
        );
        assert!(read_brief(Some("   ".into()), None).is_err());
        assert!(read_brief(None, Some("/definitely/not/here.md".into())).is_err());
    }
}
