//! `fleet jobs`: scheduled agent sessions (see `core::jobs`) — list, show, run now, the
//! launchd tick that fires them, pause/resume, and the service itself.

use std::path::Path;

use chrono::{DateTime, Local, TimeZone};
use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::commands::{clean_model, with_model};
use crate::cli::render::{ago, home_rel};
use crate::cli::web::{self, ServiceOpts};
use crate::core::backend::{self, Prompt};
use crate::core::config;
use crate::core::discovery::{self, Backend, Session};
use crate::core::jobs::{self, Due, Job, PromptSrc, Run, Settings, Tag};
use crate::core::tools;
use crate::error::{Error, Result};

pub const LAUNCHD_LABEL: &str = "fleet.jobs";
pub const SERVICE_LOG: &str = "Library/Logs/fleet.jobs.log";
/// A session is closed by `keep` only once it has also been quiet this long — someone
/// may have picked the conversation up.
const QUIET_BEFORE_CLOSE_MS: i64 = 3600 * 1000;

fn warn_problems(s: &Settings) {
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
}

fn job<'a>(s: &'a Settings, name: &str) -> Result<&'a Job> {
    s.get(name).ok_or_else(|| {
        let names: Vec<&str> = s.jobs.iter().map(|j| j.name.as_str()).collect();
        Error::Other(if names.is_empty() {
            format!("no bot `{name}` — none configured (config `bots`)")
        } else {
            format!("no bot `{name}` — configured: {}", names.join(", "))
        })
    })
}

fn local(ms: i64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Local::now)
}

pub(crate) fn when(t: DateTime<Local>) -> String {
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("today %H:%M").to_string()
    } else if t.date_naive() == now.date_naive().succ_opt().unwrap_or_default() {
        t.format("tomorrow %H:%M").to_string()
    } else {
        t.format("%a %d.%m. %H:%M").to_string()
    }
}

/// The live sessions of a job, from one discovery pass.
fn live_of<'a>(rows: &'a [Session], job: &str) -> Vec<&'a Session> {
    rows.iter()
        .filter(|r| r.origin.as_ref().is_some_and(|o| o.job == job))
        .collect()
}

fn job_json(j: &Job, st: &jobs::JobState, rows: &[Session], runs: usize) -> Value {
    let live: Vec<Value> = live_of(rows, &j.name)
        .into_iter()
        .map(|r| {
            json!({
                "run": r.origin.as_ref().map(|o| o.run.clone()),
                "sessionId": r.session_id,
                "tmuxSession": r.tmux_session,
                "status": r.status,
                "title": r.headline(),
            })
        })
        .collect();
    let prompt = match &j.prompt {
        PromptSrc::None => json!(null),
        PromptSrc::Inline(p) => json!({ "inline": p }),
        PromptSrc::File(f) => json!({ "file": f }),
    };
    let take = st.runs.len().saturating_sub(runs);
    json!({
        "name": j.name,
        "schedule": j.schedule,
        "scheduleText": j.cron.describe(),
        "enabled": j.enabled,
        "dir": j.dir,
        "harness": j.harness.id(),
        "model": j.model,
        "command": j.command,
        "keep": j.keep,
        "prompt": prompt,
        "nextRun": j.enabled.then(|| j.cron.next(Local::now()).map(|t| t.timestamp_millis())).flatten(),
        "lastSlot": st.last_slot,
        "runs": st.runs[take..].iter().rev().collect::<Vec<&Run>>(),
        "live": live,
    })
}

/// `fleet jobs [list]`.
pub fn list(json_out: bool) -> Result<()> {
    let s = Settings::load();
    warn_problems(&s);
    let rows = if s.jobs.is_empty() {
        Vec::new()
    } else {
        discovery::discover()
    };
    if json_out {
        let v: Vec<Value> = s
            .jobs
            .iter()
            .map(|j| job_json(j, &jobs::load_state(&j.name), &rows, 1))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if s.jobs.is_empty() {
        println!(
            "no bots — add one under `bots` in {}",
            home_rel(&config::path().display().to_string())
        );
        return Ok(());
    }
    let now = Local::now();
    for j in &s.jobs {
        let st = jobs::load_state(&j.name);
        let next = if j.enabled {
            j.cron.next(now).map(when).unwrap_or_else(|| "never".into())
        } else {
            "paused".into()
        };
        let last = st
            .runs
            .last()
            .map(|r| format!("{} {}", r.outcome, ago(Some(r.started_at))))
            .unwrap_or_else(|| "never ran".into());
        let live = live_of(&rows, &j.name);
        let busy = live.iter().filter(|r| r.status == "busy").count();
        let waiting = live.iter().filter(|r| r.status == "waiting").count();
        let mut live_s = format!("{} live", live.len());
        if busy + waiting > 0 {
            live_s.push_str(&format!(" ({busy} working, {waiting} need you)"));
        }
        let name = if j.enabled {
            j.name.bold()
        } else {
            j.name.dimmed()
        };
        println!(
            "{:<24} {:<28} next {:<18} last {:<20} {}",
            name,
            j.cron.describe(),
            next,
            last,
            live_s.dimmed()
        );
    }
    Ok(())
}

/// `fleet jobs show <name>`.
pub fn show(name: &str, json_out: bool) -> Result<()> {
    let s = Settings::load();
    warn_problems(&s);
    let j = job(&s, name)?;
    let st = jobs::load_state(&j.name);
    let rows = discovery::discover();
    if json_out {
        let v = job_json(j, &st, &rows, jobs::KEEP_RUNS);
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "{}  {}",
        j.name.bold(),
        if j.enabled {
            "enabled".green()
        } else {
            "paused".yellow()
        }
    );
    println!(
        "  schedule  {}  ({})  next: {}",
        j.cron.describe(),
        j.schedule,
        j.cron
            .next(Local::now())
            .map(when)
            .unwrap_or_else(|| "never".into())
    );
    println!("  dir       {}", j.dir);
    match &j.prompt {
        PromptSrc::None => println!("  prompt    (none)"),
        PromptSrc::Inline(p) => println!("  prompt    {}", crate::cli::render::fit(p, 80)),
        PromptSrc::File(f) => println!("  prompt    {f}"),
    }
    let launcher = j.command.clone().unwrap_or_else(|| j.harness.command());
    println!(
        "  launcher  {}{}",
        launcher,
        j.model
            .as_deref()
            .map(|m| format!(" --model {m}"))
            .unwrap_or_default()
    );
    if let Some(g) = &j.gh_identity {
        println!("  github    as {g} (ghIdentity)");
    }
    println!(
        "  keep      {}",
        j.keep
            .map(crate::cli::repos::span)
            .unwrap_or_else(|| "never".into())
    );
    if st.runs.is_empty() {
        println!("\n  never ran");
        return Ok(());
    }
    println!();
    for r in st.runs.iter().rev().take(10) {
        let outcome = match r.outcome.as_str() {
            "spawned" => r.outcome.green(),
            "failed" => r.outcome.red(),
            _ => r.outcome.yellow(),
        };
        println!(
            "  {}  {:<8} {}{}",
            r.id.bold(),
            r.trigger.dimmed(),
            outcome,
            r.reason
                .as_deref()
                .map(|x| format!(" — {x}"))
                .unwrap_or_default()
        );
        for sess in &r.sessions {
            let live: Vec<&Session> = rows
                .iter()
                .filter(|x| x.tmux_session.as_deref() == Some(sess.tmux_session.as_str()))
                .collect();
            let state = match (live.first(), sess.closed_at) {
                (Some(x), _) => x.status.clone(),
                (None, Some(_)) => "closed".into(),
                (None, None) => "gone".into(),
            };
            let indent = if sess.parent.is_some() {
                "      └ "
            } else {
                "    · "
            };
            println!("{indent}{:<32} {}", sess.tmux_session, state.dimmed());
        }
    }
    Ok(())
}

/// Whether any session of `job` is still working (a new run would overlap it).
fn busy_reason(rows: &[Session], job: &str) -> Option<String> {
    let busy: Vec<String> = live_of(rows, job)
        .into_iter()
        .filter(|r| r.status == "busy")
        .filter_map(|r| r.tmux_session.clone())
        .collect();
    (!busy.is_empty()).then(|| format!("still working: {}", busy.join(", ")))
}

fn push_run(job: &str, run: Run) -> Result<()> {
    let mut st = jobs::load_state(job);
    st.runs.push(run);
    jobs::save_state(job, &st)
}

fn read_prompt(j: &Job) -> std::result::Result<String, String> {
    match &j.prompt {
        PromptSrc::None => Ok(String::new()),
        PromptSrc::Inline(p) => Ok(p.clone()),
        PromptSrc::File(f) => {
            let path = tools::expand_tilde(f);
            std::fs::read_to_string(&path)
                .map(|t| strip_frontmatter(&t).to_string())
                .map_err(|e| format!("cannot read promptFile {f}: {e}"))
        }
    }
}

/// The recipe's prompt with a run's task (`bots run --prompt`) after it.
fn with_task(base: &str, task: &str) -> String {
    let base = base.trim_end();
    if base.is_empty() {
        return task.to_string();
    }
    format!("{base}\n\n{task}")
}

/// A prompt file may be a note with YAML frontmatter (`---` … `---`): the agent gets the body.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => rest[end + 5..].trim_start_matches('\n'),
        None => text,
    }
}

/// Start one run of `j` now: its record first (so the spawn lands in it), then the session.
fn start(j: &Job, trigger: &str, slot: Option<i64>) -> Result<Run> {
    let now = Local::now();
    let id = jobs::run_id(now, trigger != "schedule");
    let mut run = Run {
        id: id.clone(),
        trigger: trigger.into(),
        started_at: now.timestamp_millis(),
        slot,
        outcome: "spawned".into(),
        ..Default::default()
    };
    let fail = |mut run: Run, why: String| -> Result<Run> {
        run.outcome = "failed".into();
        run.reason = Some(why.clone());
        let mut st = jobs::load_state(&j.name);
        match st.runs.iter_mut().rev().find(|r| r.id == run.id) {
            Some(r) => {
                r.outcome = run.outcome.clone();
                r.reason = run.reason.clone();
            }
            None => st.runs.push(run.clone()),
        }
        jobs::save_state(&j.name, &st)?;
        Err(Error::Other(format!("{}: {why}", j.name)))
    };
    let dir = tools::expand_tilde(&j.dir);
    if !Path::new(&dir).is_dir() {
        return fail(run, format!("dir does not exist: {}", j.dir));
    }
    let model = match j.model.as_deref().map(clean_model).transpose() {
        Ok(m) => m.flatten(),
        Err(e) => return fail(run, e.to_string()),
    };
    let text = match read_prompt(j) {
        Ok(t) => t,
        Err(e) => return fail(run, e),
    };
    // A bot session closes itself once its work is done and nothing waits on the user.
    let text = if text.trim().is_empty() {
        text
    } else {
        crate::core::done::with_instruction(&text)
    };
    let prompt_file = jobs::prompt_path(&j.name, &id);
    if !text.trim().is_empty() {
        if let Some(d) = prompt_file.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&prompt_file, &text)?;
        run.prompt_file = Some(tools::tildify(&prompt_file.display().to_string()));
    }
    let tag = Tag {
        run: format!("{}/{id}", j.name),
        command: j.command.clone(),
    };
    let launcher = with_model(
        j.command
            .as_deref()
            .map(String::from)
            .unwrap_or_else(|| j.harness.command())
            .as_str(),
        model.as_deref(),
    );
    let launcher =
        match crate::core::gh_identity::wrap(&launcher, j.harness, j.gh_identity.as_deref()) {
            Ok(l) => l,
            Err(e) => return fail(run, e.to_string()),
        };
    let name = jobs::session_name(&j.name, &id);
    let path_s = prompt_file.display().to_string();
    let prompt = if run.prompt_file.is_some() {
        Prompt::File(&path_s)
    } else {
        Prompt::None
    };
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(&dir, prompt, Some(&name), &launcher, j.harness);
        println!("dry-run: would start {} ({}): {line}", j.name, tag.run);
        return Ok(run);
    }
    push_run(&j.name, run.clone())?;
    match backend::spawn_tagged(
        Backend::Tmux,
        &dir,
        prompt,
        Some(&name),
        None,
        false,
        &launcher,
        j.harness,
        Some(&tag),
    ) {
        Ok(_) => Ok(jobs::load_state(&j.name)
            .runs
            .into_iter()
            .rev()
            .find(|r| r.id == id)
            .unwrap_or(run)),
        Err(e) => fail(run, e.to_string()),
    }
}

fn describe(r: &Run) -> String {
    let sess = r
        .sessions
        .first()
        .map(|s| format!(" in tmux session {}", s.tmux_session))
        .unwrap_or_default();
    format!("run {}{sess}", r.id)
}

/// `fleet jobs run <name>`: start a run now, whatever the schedule says.
/// `fleet bots run <name> [--prompt <text>]`: a run now; `--prompt` is added after the recipe's
/// prompt — a task for this run (e.g. acting on the bot's own report), with the recipe's context.
pub fn run_now(name: &str, force: bool, prompt: Option<String>) -> Result<()> {
    let s = Settings::load();
    warn_problems(&s);
    let recipe = job(&s, name)?;
    let custom = match prompt.filter(|p| !p.trim().is_empty()) {
        Some(task) => {
            let base = read_prompt(recipe).map_err(|e| Error::Other(format!("{name}: {e}")))?;
            Some(Job {
                prompt: PromptSrc::Inline(with_task(&base, &task)),
                ..recipe.clone()
            })
        }
        None => None,
    };
    let j = custom.as_ref().unwrap_or(recipe);
    if !force && let Some(why) = busy_reason(&discovery::discover(), &j.name) {
        return Err(Error::Other(format!(
            "{}: {why} — pass --force to start another run anyway",
            j.name
        )));
    }
    let r = start(j, "manual", None)?;
    println!("{} {} — {}", "→".cyan(), j.name.bold(), describe(&r));
    Ok(())
}

fn log(quiet: bool, line: &str) {
    if quiet {
        println!("{} {line}", Local::now().format("%Y-%m-%d %H:%M:%S"));
    } else {
        println!("{line}");
    }
}

/// `fleet jobs tick`: what the launchd agent runs every minute — fire due jobs, record missed
/// slots, close the idle sessions of runs past their `keep`; then the scheduled sessions
/// (`fleet schedule`).
pub fn tick(quiet: bool) -> Result<()> {
    let s = Settings::load();
    if !quiet {
        warn_problems(&s);
    }
    let now = Local::now();
    // One discovery pass, only when something needs it.
    let mut rows: Option<Vec<Session>> = None;
    let rows_now = |rows: &mut Option<Vec<Session>>| -> Vec<Session> {
        rows.get_or_insert_with(discovery::discover).clone()
    };
    let mut failed = 0;
    for j in &s.jobs {
        let mut st = jobs::load_state(&j.name);
        let (d, last) = jobs::due(&j.cron, j.enabled, st.last_slot, now);
        let changed = st.last_slot != Some(last);
        st.last_slot = Some(last);
        match d {
            Due::Nothing | Due::Arm => {
                if changed {
                    jobs::save_state(&j.name, &st)?;
                }
            }
            Due::Missed(slot) => {
                st.runs.push(Run {
                    id: jobs::run_id(local(slot), false),
                    trigger: "schedule".into(),
                    started_at: now.timestamp_millis(),
                    slot: Some(slot),
                    outcome: "missed".into(),
                    reason: Some(format!(
                        "{} was too long ago (machine off or asleep?)",
                        when(local(slot))
                    )),
                    ..Default::default()
                });
                jobs::save_state(&j.name, &st)?;
                log(
                    quiet,
                    &format!("{}: missed the {} run", j.name, when(local(slot))),
                );
            }
            Due::Fire(slot) => {
                jobs::save_state(&j.name, &st)?;
                if let Some(why) = busy_reason(&rows_now(&mut rows), &j.name) {
                    push_run(
                        &j.name,
                        Run {
                            id: jobs::run_id(now, false),
                            trigger: "schedule".into(),
                            started_at: now.timestamp_millis(),
                            slot: Some(slot),
                            outcome: "skipped".into(),
                            reason: Some(why.clone()),
                            ..Default::default()
                        },
                    )?;
                    log(quiet, &format!("{}: skipped — {why}", j.name));
                    continue;
                }
                match start(j, "schedule", Some(slot)) {
                    Ok(r) => {
                        rows = None; // a new session exists now
                        log(quiet, &format!("{}: started {}", j.name, describe(&r)));
                    }
                    Err(e) => {
                        failed += 1;
                        log(quiet, &format!("{}: failed — {e}", j.name));
                    }
                }
            }
        }
        cleanup(j, now, quiet, &mut rows)?;
    }
    if !crate::core::done::load().marks.is_empty() {
        for line in crate::core::done::prune(&rows_now(&mut rows))? {
            log(quiet, &line);
        }
    }
    failed += crate::cli::schedule::tick(quiet)?;
    if failed > 0 {
        return Err(Error::exit(
            1,
            format!("{failed} bot(s) or scheduled session(s) failed to start"),
        ));
    }
    Ok(())
}

/// Close the sessions of `j`'s runs that are past `keep`, idle and quiet; mark the ones that
/// are already gone.
fn cleanup(
    j: &Job,
    now: DateTime<Local>,
    quiet: bool,
    rows: &mut Option<Vec<Session>>,
) -> Result<()> {
    let Some(keep) = j.keep else {
        return Ok(());
    };
    let now_ms = now.timestamp_millis();
    let mut st = jobs::load_state(&j.name);
    let due = |r: &Run| {
        r.started_at + keep * 1000 < now_ms && r.sessions.iter().any(|s| s.closed_at.is_none())
    };
    if !st.runs.iter().any(due) {
        return Ok(());
    }
    let live = rows.get_or_insert_with(discovery::discover).clone();
    let tmux = backend::tmux_session_names();
    let mut changed = false;
    for r in st.runs.iter_mut() {
        if !due(r) {
            continue;
        }
        for s in r.sessions.iter_mut().filter(|s| s.closed_at.is_none()) {
            if !tmux.contains(&s.tmux_session) {
                s.closed_at = Some(now_ms);
                changed = true;
                continue;
            }
            let agents: Vec<&Session> = live
                .iter()
                .filter(|x| x.tmux_session.as_deref() == Some(s.tmux_session.as_str()))
                .collect();
            // Idle and quiet, or the agent already exited and left a shell behind.
            let closable = agents.iter().all(|x| {
                x.status == "idle"
                    && x.updated_at
                        .is_none_or(|u| now_ms - u > QUIET_BEFORE_CLOSE_MS)
            });
            if !closable {
                continue;
            }
            match crate::core::tmux::kill_session(&s.tmux_session) {
                Ok(()) => {
                    s.closed_at = Some(now_ms);
                    changed = true;
                    log(
                        quiet,
                        &format!("{}: closed {} (run {})", j.name, s.tmux_session, r.id),
                    );
                }
                Err(e) => log(
                    quiet,
                    &format!("{}: could not close {}: {e}", j.name, s.tmux_session),
                ),
            }
        }
    }
    if changed {
        jobs::save_state(&j.name, &st)?;
        *rows = None;
    }
    Ok(())
}

/// `fleet bots enable|disable <name>`.
pub fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    let s = Settings::load();
    let section = job(&s, name)?.section;
    config::patch(&format!("{section}.{name}.enabled"), Value::Bool(enabled))?;
    if enabled {
        let j = Settings::load();
        let next = j
            .get(name)
            .and_then(|j| j.cron.next(Local::now()))
            .map(when)
            .unwrap_or_else(|| "never".into());
        println!("{name}: enabled — next run {next}");
    } else {
        println!("{name}: paused");
    }
    Ok(())
}

pub fn launchd_plist(exe: &Path, config_path: &Path, path_env: &str, log: &Path) -> String {
    let e = |p: &Path| web::xml_escape(&p.display().to_string());
    // Interactive: a session the tick starts may be the first in a fresh tmux server, which
    // inherits this job's QoS — Low Power Mode throttles a Background/Standard one hard.
    // AbandonProcessGroup: that tmux server must outlive the tick.
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
    <string>jobs</string>
    <string>tick</string>
    <string>--quiet</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>FLEET_CONFIG</key><string>{cfg}</string>
    <key>PATH</key><string>{path}</string>
  </dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>AbandonProcessGroup</key><true/>
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
        tick = jobs::TICK_SECS,
        log = e(log),
    )
}

/// `fleet jobs install-service`.
pub fn install_service(o: ServiceOpts) -> Result<()> {
    tools::deny_path_only("`fleet jobs install-service`")?;
    if !cfg!(target_os = "macos") {
        return Err(Error::exit(
            1,
            "install-service is macOS-only — elsewhere run `fleet bots tick --quiet` every minute from cron or a systemd timer",
        ));
    }
    let exe = crate::cli::hosts::current_exe()?;
    let home = dirs::home_dir().ok_or_else(|| Error::Other("no home directory".into()))?;
    let log = home.join(SERVICE_LOG);
    let tmux = tools::find_binary("tmux").unwrap_or_else(|| "/opt/homebrew/bin/tmux".into());
    let path = web::service_path(
        &tmux,
        &[tools::find_binary("claude"), tools::find_binary("node")],
    );
    let plist = launchd_plist(&exe, &config::path(), &path, &log);
    web::launchd_agent(LAUNCHD_LABEL, &plist, &log, &o)?;
    if !o.uninstall && !o.print && !o.no_load && !crate::core::hosts::dry_run() {
        println!("runs due bots every minute — see them with: fleet bots");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_task_follows_the_recipe_prompt() {
        assert_eq!(
            with_task("Deploy ctx.\n", "comment on: 1"),
            "Deploy ctx.\n\ncomment on: 1"
        );
        assert_eq!(with_task("  ", "comment on: 1"), "comment on: 1");
    }

    #[test]
    fn prompt_files_lose_their_frontmatter() {
        assert_eq!(
            strip_frontmatter("---\ntitle: x\n---\n\nDo it.\n"),
            "Do it.\n"
        );
        assert_eq!(strip_frontmatter("Do it.\n---\nmore"), "Do it.\n---\nmore");
        assert_eq!(strip_frontmatter("---\nunterminated"), "---\nunterminated");
    }

    #[test]
    fn plist_ticks_every_minute_and_outlives_itself() {
        let p = launchd_plist(
            Path::new("/x/fleet"),
            Path::new("/c.json"),
            "/usr/bin:/bin",
            Path::new("/l.log"),
        );
        assert!(p.contains("<string>jobs</string>"));
        assert!(p.contains("<string>tick</string>"));
        assert!(p.contains("<string>--local</string>"));
        assert!(p.contains("<integer>60</integer>"));
        assert!(p.contains("<key>AbandonProcessGroup</key><true/>"));
        assert!(p.contains(LAUNCHD_LABEL));
    }
}
