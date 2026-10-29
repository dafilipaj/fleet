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
/// `fleet gh-identity list`: the configured GitHub identities.
pub fn gh_identity_list(json: bool) -> Result<()> {
    let rows = crate::core::gh_identity::all();
    if json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|i| serde_json::json!({ "name": i.name, "login": i.login, "command": i.command }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no GitHub identities (config ghIdentities)");
    }
    for i in &rows {
        println!("{:<16} {:<28} {}", i.name, i.login, i.command.dimmed());
    }
    Ok(())
}

/// `fleet gh-identity launcher <name>`: what a session with that identity is launched with.
pub fn gh_identity_launcher(name: &str, launcher: Option<&str>) -> Result<()> {
    let base = launcher
        .map(str::to_string)
        .unwrap_or_else(resolve_launcher);
    println!(
        "{}",
        crate::core::gh_identity::wrap(&base, Harness::Claude, Some(name))?
    );
    Ok(())
}

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
    // Which sessions reported themselves done (`fleet done`) and were not prompted since.
    crate::core::done::stamp_done(&mut rows);
    // The branch + GitHub repo of each row's checkout (views link PRs by it, on any host).
    crate::core::git::stamp_git(&mut rows);
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
    let s = discovery::resolve(target).map_err(Error::Other)?;
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
    let s = discovery::resolve(target).map_err(Error::Other)?;
    backend::send(&s, text)?;
    println!(
        "sent to {} ({}): {}",
        s.headline().bold(),
        s.backend.label(),
        text
    );
    Ok(())
}

/// `fleet done`: mark (or `--cancel`, unmark) the session this runs in as finished.
pub fn done(note: Option<String>, cancel: bool) -> Result<()> {
    let pane = std::env::var("TMUX_PANE").map_err(|_| {
        Error::Other(
            "fleet done runs inside the session's own tmux pane ($TMUX_PANE is not set)".into(),
        )
    })?;
    let m = crate::core::done::mark(&discovery::discover(), &pane, note, cancel)?;
    if cancel {
        println!("{}: no longer marked done", m.tmux_session);
    } else {
        println!(
            "{}: marked done — Fleet shows it, the user closes it (fleet done --cancel to undo)",
            m.tmux_session
        );
    }
    Ok(())
}

/// `fleet ask-done <target>`: ask a session to report itself done once finished.
pub fn ask_done(target: &str) -> Result<()> {
    let s = discovery::resolve(target).map_err(Error::Other)?;
    if s.backend != Backend::Tmux {
        return Err(Error::Other(format!(
            "{} is not in tmux — only a tmux session can report itself done",
            s.headline()
        )));
    }
    if s.status == "waiting" {
        return Err(Error::Other(format!(
            "{} is waiting on you — answer it first (the keys would land in its prompt)",
            s.headline()
        )));
    }
    backend::send(&s, crate::core::done::INSTRUCTION)?;
    println!(
        "asked {} to report itself done when finished",
        s.headline().bold()
    );
    Ok(())
}

/// `fleet sleep`: end a session but keep it dormant (see [`crate::core::sleep`]).
pub fn sleep(target: &str, json: bool) -> Result<()> {
    let s = discovery::resolve(target).map_err(Error::Other)?;
    let r = crate::core::sleep::sleep(&s)?;
    if json {
        let mut v = serde_json::to_value(&r)?;
        v["host"] = serde_json::Value::String(host_label());
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if r.dry_run {
        return Ok(());
    }
    let target = r.dormant.as_ref().map(|d| d.target.as_str()).unwrap_or("");
    println!(
        "{} is dormant ({}) — bring it back: fleet restore {}",
        r.title.bold(),
        r.terminal.replace('-', " "),
        crate::core::tools::shq_min(target)
    );
    Ok(())
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
    let s = discovery::resolve(target).map_err(Error::Other)?;
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
    /// With `all`: only sessions [`naming::is_settled`] — a second prompt or a brief.
    pub settled: bool,
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
        (Some(t), false) => vec![discovery::resolve(t).map_err(Error::Other)?],
        (None, true) => discovery::discover()
            .into_iter()
            .filter(Session::is_derived_name)
            .filter(|s| !o.settled || naming::is_settled(s))
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
    /// The session closes itself once finished ([`crate::core::done`]): tmux, the instruction
    /// below the prompt.
    pub ask_done: bool,
    /// Start linked to Slack: tmux, no prompt at launch — it goes in with the link's intro.
    pub slack: Option<crate::cli::slack::SpawnLink>,
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
            return Err(Error::Other(format!("dir does not exist: {dir}")));
        }
        let backend = self.backend.unwrap_or_else(default_backend);
        let name = self.name.as_deref().map(clean_name).transpose()?;
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

pub fn spawn(prompt: Option<String>, from: Option<String>, opts: SpawnOpts) -> Result<()> {
    if let Some(src) = from {
        return spawn_from(&src, prompt, opts);
    }
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = opts
        .model
        .as_deref()
        .map(clean_model)
        .transpose()?
        .flatten();
    let prompt = prompt.unwrap_or_default();
    let (prompt, backend_kind) = if opts.ask_done {
        (crate::core::done::with_instruction(&prompt), Backend::Tmux)
    } else {
        (prompt, backend_kind)
    };
    if let Some(link) = &opts.slack {
        return spawn_linked(
            &dir,
            name.as_deref(),
            &prompt,
            model.as_deref(),
            &opts,
            link,
        );
    }
    let launcher = crate::core::gh_identity::wrap_current(
        &with_model(&opts.harness.command(), model.as_deref()),
        opts.harness,
    )?;
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::Inline(&prompt),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
        println!(
            "dry-run: would spawn a {} session: {line}",
            backend_kind.label()
        );
        return Ok(());
    }
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
    if prompt.is_empty() {
        println!("{desc} in {}", home_rel(&dir));
    } else {
        println!("{desc} in {} — \"{prompt}\"", home_rel(&dir));
    }
    Ok(())
}

/// `spawn --slack`: start in tmux with no prompt, link the session once it has registered, and
/// let the link's intro carry the prompt — so it never works on it before it knows it is in Slack.
fn spawn_linked(
    dir: &str,
    name: Option<&str>,
    prompt: &str,
    model: Option<&str>,
    opts: &SpawnOpts,
    link: &crate::cli::slack::SpawnLink,
) -> Result<()> {
    let launcher = crate::core::gh_identity::wrap_current(
        &with_model(&opts.harness.command(), model),
        opts.harness,
    )?;
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(dir, Prompt::Inline(""), name, &launcher, opts.harness);
        println!("dry-run: would spawn a tmux session: {line}");
        println!(
            "dry-run: then link it to {} for {}",
            link.url, link.duration
        );
        return Ok(());
    }
    crate::cli::slack::check_linkable()?;
    let tag = crate::core::jobs::Tag::from_env();
    let (desc, tmux) = backend::spawn_tagged(
        Backend::Tmux,
        dir,
        Prompt::Inline(""),
        name,
        opts.tmux_session.as_deref(),
        false,
        &launcher,
        opts.harness,
        tag.as_ref(),
    )?;
    println!("{desc} in {}", home_rel(dir));
    let tmux =
        tmux.ok_or_else(|| Error::Other("the session has no tmux session to link".into()))?;
    crate::cli::slack::link_spawned(&tmux, link, prompt)
}

/// `spawn --from <session>`: a new session in the source's cwd (on this host — `-H` already
/// brought us to the source's host) whose first prompt is the brief's continue prompt, plus
/// `extra` (the task; the default one when empty) below its separator. The prompt goes through a file, like a handoff brief: it is
/// multi-line, and the file stays as the record of what was handed on.
fn spawn_from(source: &str, extra: Option<String>, mut opts: SpawnOpts) -> Result<()> {
    let c = crate::cli::brief::continue_from(source)?;
    if opts.dir.is_none() {
        opts.dir = Some(c.cwd.clone().ok_or_else(|| {
            Error::Other(format!(
                "no working directory known for {} — pass --dir",
                c.label
            ))
        })?);
    }
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = opts
        .model
        .as_deref()
        .map(clean_model)
        .transpose()?
        .flatten();
    let task = extra.as_deref().map(str::trim).filter(|e| !e.is_empty());
    let text = format!(
        "{}\n\n{}",
        c.prompt,
        task.unwrap_or(crate::core::brief::CONTINUE_DEFAULT_TASK)
    );
    let now = chrono::Local::now();
    let short: String = c.id.chars().take(8).collect();
    let path = handoff_dir().join(format!(
        "{}-continue-{short}.md",
        now.format("%Y%m%d-%H%M%S")
    ));
    let path_s = path.display().to_string();
    let launcher = crate::core::gh_identity::wrap_current(
        &with_model(&opts.harness.command(), model.as_deref()),
        opts.harness,
    )?;
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::File(&path_s),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
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
        let launcher = crate::core::gh_identity::wrap_current(
            &with_model(&opts.harness.command(), model.as_deref()),
            opts.harness,
        )?;
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
    let launcher = crate::core::gh_identity::wrap_current(
        &with_model(&opts.harness.command(), model.as_deref()),
        opts.harness,
    )?;
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
