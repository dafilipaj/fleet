//! `fleet tasks` / `fleet task …`: your open Productive tasks by column (see `core::tasks`).

use colored::Colorize;

use crate::cli::render::{ago, column, fit, term_width, width_of};
use crate::core::tasks::{self, Category, Column, Mode, Settings, Task};
use crate::error::{Error, Result};

pub struct Opts {
    pub json: bool,
    pub column: Option<String>,
    pub mode: Mode,
}

fn settings() -> Settings {
    let s = Settings::load();
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
    s
}

/// `today`, `in 2d`, `3d late`.
fn due_label(t: &Task) -> String {
    let Some(d) = t
        .due_date
        .as_deref()
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    else {
        return String::new();
    };
    let n = (d - chrono::Local::now().date_naive()).num_days();
    match n {
        0 => "today".into(),
        n if n < 0 => format!("{}d late", -n),
        n => format!("in {n}d"),
    }
}

fn status_cell(t: &Task, w: usize) -> colored::ColoredString {
    let s = column(&t.status.name, w);
    match t.status.category {
        Category::Started => s.blue(),
        Category::Closed => s.dimmed(),
        Category::NotStarted => s.normal(),
    }
}

pub fn run(o: Opts) -> Result<()> {
    let s = settings();
    let only = match &o.column {
        None => None,
        Some(c) => Some(Column::parse(c).ok_or_else(|| {
            let ids: Vec<&str> = Column::ALL.iter().map(|c| c.id()).collect();
            Error::exit(
                2,
                format!("unknown column '{c}' (one of: {})", ids.join(", ")),
            )
        })?),
    };
    let now = chrono::Utc::now().timestamp_millis();
    let mut view = tasks::get(&s, o.mode, now)?;
    view.snap
        .tasks
        .retain(|t| only.is_none_or(|c| t.column == c));

    if o.json {
        let columns: Vec<serde_json::Value> = Column::ALL
            .iter()
            .filter(|c| only.is_none_or(|o| o == **c))
            .map(|c| {
                serde_json::json!({
                    "id": c.id(),
                    "title": c.title(),
                    "count": view.snap.tasks.iter().filter(|t| t.column == *c).count(),
                })
            })
            .collect();
        let out = serde_json::json!({
            "user": view.snap.user,
            "organization": view.snap.organization,
            "scope": view.snap.scope,
            "fetchedAt": view.snap.fetched_at,
            "stale": view.stale,
            "error": view.error,
            "truncated": view.snap.truncated,
            "columns": columns,
            "tasks": view.snap.tasks,
            "statuses": view.snap.statuses,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if let Some(e) = &view.error {
        eprintln!(
            "{} {e} — showing the cache from {} ago",
            "warning:".yellow(),
            ago(Some(view.snap.fetched_at))
        );
    }
    if view.snap.truncated {
        eprintln!(
            "{} more than 1000 open tasks; some are missing (narrow tasks.projects)",
            "warning:".yellow()
        );
    }
    let width = term_width();
    let all = &view.snap.tasks;
    let num_w = all.iter().map(|t| width_of(&t.id)).max().unwrap_or(0);
    let status_w = all
        .iter()
        .map(|t| width_of(&t.status.name))
        .max()
        .unwrap_or(0)
        .min(16);
    let project_w = all
        .iter()
        .map(|t| width_of(&t.project.name))
        .max()
        .unwrap_or(0)
        .min(22);
    let mut first = true;
    for c in Column::ALL {
        let rows: Vec<&Task> = all.iter().filter(|t| t.column == c).collect();
        if rows.is_empty() && only != Some(c) {
            continue;
        }
        if !first {
            println!();
        }
        first = false;
        println!("{} {}", c.title().bold(), rows.len().to_string().dimmed());
        for t in rows {
            let due = due_label(t);
            let used = 2 + num_w + 2 + status_w + 2 + project_w + 2 + 10;
            let title_w = width.saturating_sub(used).max(10);
            let due_cell = column(&due, 9);
            let due_cell = if due.contains("late") || due == "today" {
                due_cell.red()
            } else if due.is_empty() {
                due_cell.normal()
            } else {
                due_cell.yellow()
            };
            println!(
                "  {}  {}  {}  {}  {}",
                column(&t.id, num_w).cyan(),
                status_cell(t, status_w),
                column(&fit(&t.project.name, project_w), project_w).dimmed(),
                due_cell,
                fit(&t.title, title_w),
            );
        }
    }
    if first {
        println!("No open tasks assigned to you.");
    }
    let who = view
        .snap
        .user
        .as_ref()
        .map(|u| u.name.clone())
        .unwrap_or_default();
    let org = view
        .snap
        .organization
        .as_ref()
        .map(|o| format!(" in {}", o.name))
        .unwrap_or_default();
    println!(
        "\n{}",
        format!(
            "fetched {} ago as {who}{org}",
            ago(Some(view.snap.fetched_at))
        )
        .dimmed()
    );
    Ok(())
}

fn parse_ref(r: &str) -> Result<String> {
    tasks::parse_ref(r).ok_or_else(|| {
        Error::exit(
            2,
            format!("'{r}' is not a task: use its id (1234) or its Productive URL"),
        )
    })
}

/// `fleet task show <ref>`.
pub fn show(r: &str, json: bool) -> Result<()> {
    let id = parse_ref(r)?;
    let (t, statuses, _, _) = tasks::lookup(&settings(), &id, false)?;
    if json {
        let mut v = serde_json::to_value(&t)?;
        v["contextLine"] = tasks::context_line(&t).into();
        v["statuses"] = serde_json::to_value(&statuses)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{} {}", format!("#{}", t.number).cyan(), t.title.bold());
    println!("{}", t.url.dimmed());
    let mut meta = vec![t.project.name.clone()];
    if let Some(l) = &t.task_list {
        meta.push(l.clone());
    }
    meta.push(t.status.name.clone());
    if let Some(d) = &t.due_date {
        meta.push(format!("due {d} ({})", due_label(&t)));
    }
    if t.todos > 0 {
        meta.push(format!("todos {}/{}", t.todos - t.open_todos, t.todos));
    }
    println!("{}", meta.join(" · "));
    if !t.description.is_empty() {
        println!("\n{}", t.description);
    }
    Ok(())
}

/// `fleet task status <ref> [status]`: without a status, the workflow's statuses.
pub fn status(r: &str, want: Option<&str>, json: bool) -> Result<()> {
    let id = parse_ref(r)?;
    let s = settings();
    let Some(want) = want else {
        let (t, statuses, _, _) = tasks::lookup(&s, &id, false)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&statuses)?);
            return Ok(());
        }
        for st in &statuses {
            let mark = if st.id == t.status.id { "●" } else { " " };
            println!("{mark} {}  {}", column(&st.id, 8).dimmed(), st.name);
        }
        return Ok(());
    };
    let dry = crate::core::hosts::dry_run();
    let t = tasks::set_status(&s, &id, want, dry)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "task": t, "applied": !dry }))?
        );
    } else if dry {
        println!("dry-run: would move #{} to {}", t.number, t.status.name);
    } else {
        println!(
            "{} #{} “{}” to {}",
            "moved".green(),
            t.number,
            t.title,
            t.status.name.bold()
        );
    }
    Ok(())
}

/// `fleet task spawn <ref> [prompt]`: a new session whose first prompt is the task's context
/// line + `prompt`.
pub fn spawn(r: &str, prompt: Option<String>, opts: crate::cli::commands::SpawnOpts) -> Result<()> {
    let id = parse_ref(r)?;
    let (t, _, _, _) = tasks::lookup(&settings(), &id, false)?;
    let line = tasks::context_line(&t);
    let first = match prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{line} {p}"),
        None => line,
    };
    crate::cli::commands::spawn(Some(first), None, opts, false)
}
