//! `fleet prs`: the open pull requests that need you, by column (see `core::prs`).

use colored::Colorize;

use crate::cli::render::{ago, column, fit, term_width, width_of};
use crate::core::prs::{self, Column, Mode, Pr, Rotting, Settings};
use crate::core::repos;
use crate::error::{Error, Result};

pub struct Opts {
    pub json: bool,
    pub column: Option<String>,
    pub mode: Mode,
}

fn checks_cell(c: Option<&str>) -> colored::ColoredString {
    match c {
        Some("success") => "✓".green(),
        Some("failure") | Some("error") => "✗".red(),
        Some("pending") | Some("expected") => "●".yellow(),
        _ => " ".normal(),
    }
}

/// The age cell, coloured by how long the PR has waited (`fleet prs` rotting).
fn age_cell(p: &Pr, text: String) -> colored::ColoredString {
    match p.rotting {
        Rotting::Fresh => text.dimmed(),
        Rotting::Warming => text.green(),
        Rotting::Stale => text.yellow(),
        Rotting::Rotting => text.truecolor(255, 165, 0),
        Rotting::Critical => text.red().bold(),
    }
}

/// `new commits · worktree` and the like; in Unseen mentions, what was said.
fn note(pr: &Pr) -> String {
    let mut parts = Vec::new();
    if let Some(m) = pr
        .mention
        .as_ref()
        .filter(|_| pr.column == Column::UnseenMentions)
    {
        parts.push(format!("“{}”", m.excerpt));
    }
    if pr.my_review.as_ref().is_some_and(|r| r.new_commits) {
        parts.push("new commits".to_string());
    }
    if pr.column == Column::WaitingOnMe && pr.draft {
        parts.push("draft".into());
    }
    if pr.local.as_ref().is_some_and(|l| l.worktree.is_some()) {
        parts.push("worktree".into());
    }
    parts.join(" · ")
}

pub fn run(o: Opts) -> Result<()> {
    let s = Settings::load();
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
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
    let mut view = prs::get(&s, o.mode, now)?;
    view.snap.prs.retain(|p| only.is_none_or(|c| p.column == c));
    prs::link_local(&mut view.snap.prs, &repos::Settings::load());

    if o.json {
        let columns: Vec<serde_json::Value> = Column::ALL
            .iter()
            .filter(|c| only.is_none_or(|o| o == **c))
            .map(|c| {
                serde_json::json!({
                    "id": c.id(),
                    "title": c.title(),
                    "count": view.snap.prs.iter().filter(|p| p.column == *c).count(),
                })
            })
            .collect();
        let out = serde_json::json!({
            "user": view.snap.user,
            "scope": view.snap.scope,
            "fetchedAt": view.snap.fetched_at,
            "stale": view.stale,
            "error": view.error,
            "truncated": view.snap.truncated,
            "columns": columns,
            "prs": view.snap.prs,
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
            "{} a search returned more than 200 PRs; some are missing (narrow prs.orgs / prs.repos)",
            "warning:".yellow()
        );
    }
    let width = term_width();
    let refs: Vec<String> = view
        .snap
        .prs
        .iter()
        .map(|p| format!("{}#{}", p.repo, p.number))
        .collect();
    let ref_w = refs.iter().map(|r| width_of(r)).max().unwrap_or(0);
    let mut first = true;
    for c in Column::ALL {
        let rows: Vec<(usize, &Pr)> = view
            .snap
            .prs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.column == c)
            .collect();
        if rows.is_empty() && only != Some(c) {
            continue;
        }
        if !first {
            println!();
        }
        first = false;
        println!("{} {}", c.title().bold(), rows.len().to_string().dimmed());
        let who: Vec<String> = rows
            .iter()
            .map(|(_, p)| match c {
                Column::WaitingOnMe | Column::WaitingOnAuthor => format!("@{}", p.author),
                Column::UnseenMentions => p
                    .mention
                    .as_ref()
                    .map(|m| format!("@{}", m.author))
                    .unwrap_or_default(),
                _ => String::new(),
            })
            .collect();
        let who_w = who.iter().map(|w| width_of(w)).max().unwrap_or(0).min(20);
        for ((i, p), who) in rows.iter().zip(&who) {
            let tail = note(p);
            // ref, checks, size, age, author; the title and the note share what is left.
            let used = 2 + ref_w + 2 + 2 + 3 + 5 + if who_w > 0 { who_w + 2 } else { 0 };
            // The quote gets at most half of what is left; the title the rest.
            let tail = if c == Column::UnseenMentions {
                fit(&tail, (width.saturating_sub(used) / 2).max(10))
            } else {
                tail
            };
            let title_w = width
                .saturating_sub(
                    used + if tail.is_empty() {
                        0
                    } else {
                        width_of(&tail) + 2
                    },
                )
                .max(10);
            let who_cell = if who_w > 0 {
                format!("{}  ", column(who, who_w))
            } else {
                String::new()
            };
            println!(
                "  {}  {} {} {}  {}{}{}",
                column(&refs[*i], ref_w).cyan(),
                checks_cell(p.checks.as_deref()),
                column(&p.size, 2).dimmed(),
                age_cell(p, column(&ago(Some(prs::waiting_since(p))), 4)),
                who_cell.dimmed(),
                fit(&p.title, title_w),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("  {}", tail.yellow())
                },
            );
        }
    }
    if first {
        println!("No open pull requests need you.");
    }
    let fetched = format!(
        "fetched {} ago as @{}{}",
        ago(Some(view.snap.fetched_at)),
        view.snap.user,
        if view.snap.scope.is_empty() {
            String::new()
        } else {
            format!(" · {}", view.snap.scope)
        }
    );
    println!("\n{}", fetched.dimmed());
    Ok(())
}

/// `fleet pr seen <ref>`: its mentions so far are seen.
pub fn seen(r: &str, json: bool) -> Result<()> {
    let pr_ref = parse_ref(r)?;
    let owner = prs::owner_of(&pr_ref)?;
    let key = prs::seen_key(&owner, &pr_ref.repo, pr_ref.number);
    let now = chrono::Utc::now().timestamp_millis();
    let dry = crate::core::hosts::dry_run();
    if !dry {
        prs::mark_seen(&prs::seen_path(), &key, now)?;
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "pr": key, "seenAt": now, "applied": !dry })
            )?
        );
    } else if dry {
        println!("dry-run: would mark the mentions on {key} seen");
    } else {
        println!("{} the mentions on {key}", "seen".green());
    }
    Ok(())
}

fn parse_ref(r: &str) -> Result<prs::PrRef> {
    prs::PrRef::parse(r).ok_or_else(|| {
        Error::exit(
            2,
            format!("'{r}' is not a pull request: use owner/repo#123, repo#123 or its URL"),
        )
    })
}

/// `fleet pr checkout <ref>`: the PR's worktree (made when missing). → the worktree.
pub fn checkout(r: &str, json: bool) -> Result<prs::Worktree> {
    let pr_ref = parse_ref(r)?;
    let s = Settings::load();
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
    let head = prs::head(&pr_ref)?;
    let dry = crate::core::hosts::dry_run();
    let wt = prs::checkout(&s, &repos::Settings::load(), head, dry)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&wt)?);
    } else if dry && wt.created {
        println!(
            "dry-run: would check out {}/{}#{} on {} in {}",
            wt.pr.owner,
            wt.pr.repo,
            wt.pr.number,
            wt.branch,
            crate::cli::render::home_rel(&wt.path)
        );
    } else if wt.created {
        println!(
            "{} {}/{}#{} on {} in {}",
            "checked out".green(),
            wt.pr.owner,
            wt.pr.repo,
            wt.pr.number,
            wt.branch.cyan(),
            crate::cli::render::home_rel(&wt.path)
        );
    } else {
        println!(
            "{}/{}#{} is already checked out on {} in {}",
            wt.pr.owner,
            wt.pr.repo,
            wt.pr.number,
            wt.branch.cyan(),
            crate::cli::render::home_rel(&wt.path)
        );
    }
    Ok(wt)
}

/// `fleet pr spawn <ref> [prompt]`: check the PR out, then a new session in its worktree whose
/// first prompt is the PR's context line (what it is, where it is checked out) + `prompt`.
pub fn spawn(
    r: &str,
    prompt: Option<String>,
    mut opts: crate::cli::commands::SpawnOpts,
) -> Result<()> {
    // A backend FLEET_PATH_ONLY refuses fails before the checkout, not after it.
    opts.backend()?;
    let wt = checkout(r, false)?;
    let extra = prompt.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let first = match extra {
        Some(p) => format!("{} {p}", wt.context_line),
        None => wt.context_line.clone(),
    };
    if crate::core::hosts::dry_run() && wt.created {
        println!("dry-run: then would spawn a session there, first prompt: {first}");
        return Ok(());
    }
    opts.dir = Some(wt.path.clone());
    crate::cli::commands::spawn(Some(first), None, opts, false)
}

/// `fleet pr cleanup-worktrees [--apply]`: the worktrees under `prs.worktrees` whose PR is done
/// — listed (default) or removed (`--apply`, or nothing under `-n`).
pub fn cleanup_worktrees(apply: bool, json: bool) -> Result<()> {
    let s = Settings::load();
    for p in &s.problems {
        eprintln!("{} {p}", "warning:".yellow());
    }
    let live: Vec<String> = crate::core::discovery::discover()
        .into_iter()
        .filter_map(|x| x.cwd)
        .collect();
    let mut rows = prs::plan_cleanup(&s, &repos::Settings::load(), &live, &prs::pr_for_branch);
    let apply = apply && !crate::core::hosts::dry_run();
    if apply {
        prs::apply_cleanup(&mut rows);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "applied": apply, "worktrees": rows })
            )?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "No worktrees under {}.",
            crate::cli::render::home_rel(&crate::core::tools::expand_tilde(&s.worktrees))
        );
        return Ok(());
    }
    let remove = rows
        .iter()
        .filter(|r| r.action == prs::CleanupAction::Remove)
        .count();
    for r in &rows {
        let path = crate::cli::render::home_rel(&r.path);
        let pr =
            r.pr.as_ref()
                .map(|p| format!("#{} ", p.number))
                .unwrap_or_default();
        let (mark, text) = match (&r.action, &r.error) {
            (_, Some(e)) => ("✗".red(), format!("{path}  {}", e.red())),
            (prs::CleanupAction::Remove, None) if apply => (
                "✓".green(),
                format!("{path}  {}", format!("removed — {}", r.reason).dimmed()),
            ),
            (prs::CleanupAction::Remove, None) => {
                ("−".yellow(), format!("{path}  {}", r.reason.dimmed()))
            }
            (prs::CleanupAction::Keep, None) => (
                " ".normal(),
                format!(
                    "{}  {}",
                    path.dimmed(),
                    format!("{pr}kept: {}", r.reason).dimmed()
                ),
            ),
        };
        println!("{mark} {text}");
    }
    if !apply && remove > 0 {
        println!("\n{remove} to remove — run `fleet pr cleanup-worktrees --apply` to remove them");
    } else if remove == 0 {
        println!("\nNothing to remove.");
    }
    Ok(())
}
