//! `fleet schedule`: one-off sessions started later (see `core::schedule`) — add, list, cancel,
//! and the part of `bots tick` that fires them.

use std::path::Path;

use chrono::{DateTime, Local, TimeZone};
use colored::Colorize;
use serde_json::json;

use crate::cli::commands::{clean_model, clean_name, with_model};
use crate::cli::jobs::when;
use crate::cli::render::{column, fit, home_rel};
use crate::core::backend::{self, Prompt};
use crate::core::discovery::Backend;
use crate::core::harness::Harness;
use crate::core::schedule::{self, Action, Entry, State, TaskLink};
use crate::core::tasks::{self, Category, Task};
use crate::core::{config, tools};
use crate::error::{Error, Result};

pub struct AddOpts {
    /// When (`schedule::parse_at`); none with `task`.
    pub at: Option<String>,
    /// A task id or URL: fire on its start date.
    pub task: Option<String>,
    /// `HH:MM` on the task's start date.
    pub time: Option<String>,
    pub prompt: Option<String>,
    pub dir: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub harness: Harness,
    pub json: bool,
}

fn local(ms: i64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Local::now)
}

fn link(t: &Task, now: i64) -> TaskLink {
    TaskLink {
        id: t.id.clone(),
        number: t.number.clone(),
        title: t.title.clone(),
        url: t.url.clone(),
        start_date: t.start_date.clone(),
        checked_at: now,
    }
}

fn task_settings() -> tasks::Settings {
    let s = tasks::Settings::load();
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
    s
}

/// `fleet schedule add` / `fleet task schedule`.
pub fn add(o: AddOpts) -> Result<()> {
    let now = Local::now();
    let now_ms = now.timestamp_millis();
    let dir = o
        .dir
        .as_deref()
        .map(tools::expand_tilde)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".into())
        });
    if !Path::new(&dir).is_dir() {
        return Err(Error::Other(format!("dir does not exist: {dir}")));
    }
    let name = o.name.as_deref().map(clean_name).transpose()?;
    let model = o.model.as_deref().map(clean_model).transpose()?.flatten();
    let gh_identity = crate::core::gh_identity::current();
    if gh_identity.is_some() && o.harness != Harness::Claude {
        return Err(Error::Other(format!(
            "a GitHub identity goes only with Claude sessions, not {}",
            o.harness.label()
        )));
    }
    let (at, time, task) = match (&o.task, &o.at) {
        (Some(r), _) => {
            let id = tasks::parse_ref(r).ok_or_else(|| {
                Error::exit(
                    2,
                    format!("'{r}' is not a task: use its id (1234) or its Productive URL"),
                )
            })?;
            let time = o
                .time
                .clone()
                .unwrap_or_else(|| schedule::DEFAULT_TASK_TIME.into());
            if schedule::parse_time(&time).is_none() {
                return Err(Error::exit(2, format!("--time '{time}' is not HH:MM")));
            }
            let (t, _, _, _) = tasks::lookup(&task_settings(), &id, true)?;
            if t.status.category == Category::Closed {
                return Err(Error::Other(format!(
                    "task #{} is closed ({})",
                    t.number, t.status.name
                )));
            }
            let at = schedule::task_at(t.start_date.as_deref(), &time);
            (at, Some(time), Some(link(&t, now_ms)))
        }
        (None, Some(a)) => {
            let at = schedule::parse_at(a, now).map_err(|e| Error::exit(2, e))?;
            (Some(at.timestamp_millis()), None, None)
        }
        (None, None) => {
            return Err(Error::exit(2, "say when: --at <time>, or --task <task>"));
        }
    };
    let prompt = o.prompt.unwrap_or_default().trim().to_string();
    let entry = schedule::update(|f| {
        let e = Entry {
            id: schedule::new_id(now_ms, &f.entries),
            at,
            time,
            task,
            dir: tools::tildify(&dir),
            prompt,
            name,
            model,
            harness: o.harness,
            gh_identity,
            created_at: now_ms,
            ..Default::default()
        };
        if !crate::core::hosts::dry_run() {
            f.entries.push(e.clone());
        }
        e
    })?;
    if o.json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }
    let prefix = if crate::core::hosts::dry_run() {
        "dry-run: would schedule".to_string()
    } else {
        format!("{} {}", "scheduled".green(), entry.id.bold())
    };
    println!(
        "{prefix} — {} on {} in {}",
        describe_when(&entry),
        config::get().self_name(),
        home_rel(&dir)
    );
    if let Some(t) = &entry.task
        && entry.at.is_some_and(|a| a <= now_ms)
    {
        println!(
            "  {} #{}'s start date has passed — it starts on the next tick",
            "note:".yellow(),
            t.number
        );
    }
    Ok(())
}

fn describe_when(e: &Entry) -> String {
    let at = e.at.map(|a| when(local(a)));
    match (&e.task, at) {
        (Some(_), Some(at)) => format!("{at} (task start)"),
        (Some(_), None) => format!(
            "{} on start date (none yet)",
            e.time.as_deref().unwrap_or(schedule::DEFAULT_TASK_TIME)
        ),
        (None, Some(at)) => at,
        (None, None) => "never".into(),
    }
}

/// `fleet schedule [list]`.
pub fn list(json_out: bool, all: bool) -> Result<()> {
    let mut entries = schedule::load().entries;
    entries.sort_by_key(|e| (e.state != State::Pending, e.at.unwrap_or(i64::MAX)));
    if json_out {
        let v = json!({ "host": config::get().self_name(), "entries": entries });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let pending: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.state == State::Pending)
        .collect();
    if pending.is_empty() {
        println!("nothing scheduled — add with: fleet schedule add --at <time> [prompt]");
    }
    for e in &pending {
        print_entry(e);
    }
    let mut done: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.state != State::Pending)
        .collect();
    done.sort_by_key(|e| std::cmp::Reverse(e.fired_at.or(e.at).unwrap_or(e.created_at)));
    if !done.is_empty() {
        println!("\n{}", "recent".dimmed());
        for e in done.iter().take(if all { usize::MAX } else { 5 }) {
            print_entry(e);
        }
    }
    Ok(())
}

fn print_entry(e: &Entry) {
    let what = match &e.task {
        Some(t) => format!("#{} {}", t.number, t.title),
        None if e.prompt.is_empty() => "(no prompt)".into(),
        None => e.prompt.clone(),
    };
    let state = match e.state {
        State::Pending => "".normal(),
        State::Started => e.state.id().green(),
        State::Failed => e.state.id().red(),
        _ => e.state.id().yellow(),
    };
    let extra = match (e.state, &e.tmux_session, &e.reason) {
        (State::Started, Some(s), _) => format!(" → {s}"),
        (_, _, Some(r)) => format!(" — {r}"),
        _ => String::new(),
    };
    println!(
        "  {}  {}  {}  {}{state}{}",
        column(&e.id, 10).cyan(),
        column(&describe_when(e), 32),
        column(&fit(&home_rel(&tools::expand_tilde(&e.dir)), 22), 22).dimmed(),
        column(&fit(&what, 44), 46),
        extra.dimmed()
    );
}

/// `fleet schedule cancel <id>`.
pub fn cancel(id: &str) -> Result<()> {
    let e = schedule::find(&schedule::load().entries, id)?.clone();
    if e.state != State::Pending {
        return Err(Error::Other(format!("{} already {}", e.id, e.state.id())));
    }
    if crate::core::hosts::dry_run() {
        println!("dry-run: would cancel {}", e.id);
        return Ok(());
    }
    schedule::update_entry(&e.id, |x| {
        x.state = State::Cancelled;
        x.fired_at = Some(Local::now().timestamp_millis());
    })?;
    println!("{} {} ({})", "cancelled".yellow(), e.id, describe_when(&e));
    Ok(())
}

fn log(quiet: bool, line: &str) {
    if quiet {
        println!("{} {line}", Local::now().format("%Y-%m-%d %H:%M:%S"));
    } else {
        println!("{line}");
    }
}

/// The scheduled-session part of `bots tick`: re-read tasks, fire what is due, record what was
/// missed. → how many failed to start.
pub fn tick(quiet: bool) -> Result<usize> {
    let now = Local::now().timestamp_millis();
    let pending: Vec<Entry> = schedule::load()
        .entries
        .into_iter()
        .filter(|e| e.state == State::Pending)
        .collect();
    let dry = crate::core::hosts::dry_run();
    let mut settings: Option<tasks::Settings> = None;
    let mut failed = 0;
    for mut e in pending {
        let mut task: Option<Task> = None;
        loop {
            match schedule::decide(&e, now, task.is_some()) {
                Action::Wait => break,
                Action::Refresh => {
                    let Some(l) = e.task.clone() else { break };
                    let s = settings.get_or_insert_with(tasks::Settings::load);
                    let t = match tasks::lookup(s, &l.id, true) {
                        Ok((t, ..)) => t,
                        Err(err) => {
                            log(
                                quiet,
                                &format!("{}: could not read task #{}: {err}", e.id, l.number),
                            );
                            // Not due yet: try again in an hour, not every minute.
                            if !dry && e.at.is_none_or(|a| a > now) {
                                schedule::update_entry(&e.id, |x| {
                                    if let Some(t) = x.task.as_mut() {
                                        t.checked_at = now;
                                    }
                                })?;
                            }
                            break;
                        }
                    };
                    let time = e
                        .time
                        .clone()
                        .unwrap_or_else(|| schedule::DEFAULT_TASK_TIME.into());
                    e.task = Some(link(&t, now));
                    e.at = schedule::task_at(t.start_date.as_deref(), &time);
                    if t.status.category == Category::Closed {
                        e.state = State::Skipped;
                        e.reason = Some(format!("task closed ({})", t.status.name));
                        e.fired_at = Some(now);
                        log(
                            quiet,
                            &format!("{}: skipped — task #{} is closed", e.id, t.number),
                        );
                    } else if t.start_date != l.start_date {
                        log(
                            quiet,
                            &format!("{}: task #{} moved — {}", e.id, t.number, describe_when(&e)),
                        );
                    }
                    if !dry {
                        let fresh = e.clone();
                        schedule::update_entry(&e.id, |x| *x = fresh)?;
                    }
                    if e.state != State::Pending {
                        break;
                    }
                    task = Some(t);
                }
                Action::Missed => {
                    log(
                        quiet,
                        &format!(
                            "{}: missed — {} was too long ago (machine off or asleep?)",
                            e.id,
                            describe_when(&e)
                        ),
                    );
                    if !dry {
                        schedule::update_entry(&e.id, |x| {
                            x.state = State::Missed;
                            x.fired_at = Some(now);
                            x.reason = Some("too long past its time when a tick saw it".into());
                        })?;
                    }
                    break;
                }
                Action::Fire => {
                    match fire(&e, task.as_ref()) {
                        Ok(Some(session)) => {
                            log(quiet, &format!("{}: started tmux session {session}", e.id));
                            schedule::update_entry(&e.id, |x| {
                                x.state = State::Started;
                                x.fired_at = Some(now);
                                x.tmux_session = Some(session);
                            })?;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            failed += 1;
                            log(quiet, &format!("{}: failed — {err}", e.id));
                            if !dry {
                                schedule::update_entry(&e.id, |x| {
                                    x.state = State::Failed;
                                    x.fired_at = Some(now);
                                    x.reason = Some(err.to_string());
                                })?;
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    Ok(failed)
}

/// Start `e`'s session. → its tmux session (`None` on a dry run).
fn fire(e: &Entry, task: Option<&Task>) -> Result<Option<String>> {
    let dir = tools::expand_tilde(&e.dir);
    if !Path::new(&dir).is_dir() {
        return Err(Error::Other(format!("dir does not exist: {}", e.dir)));
    }
    let prompt = match task {
        Some(t) => {
            let line = tasks::context_line(t);
            if e.prompt.is_empty() {
                line
            } else {
                format!("{line} {}", e.prompt)
            }
        }
        None => e.prompt.clone(),
    };
    let model = e.model.as_deref().map(clean_model).transpose()?.flatten();
    let launcher = crate::core::gh_identity::wrap(
        &with_model(&e.harness.command(), model.as_deref()),
        e.harness,
        e.gh_identity.as_deref(),
    )?;
    let name = schedule::default_name(e);
    if crate::core::hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::Inline(&prompt),
            name.as_deref(),
            &launcher,
            e.harness,
        );
        println!("dry-run: would start {}: {line}", e.id);
        return Ok(None);
    }
    let (desc, session) = backend::spawn_tagged(
        Backend::Tmux,
        &dir,
        Prompt::Inline(&prompt),
        name.as_deref(),
        None,
        false,
        &launcher,
        e.harness,
        None,
    )?;
    Ok(Some(session.unwrap_or(desc)))
}
