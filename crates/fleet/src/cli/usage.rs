//! `fleet usage`: the Claude subscription's and Codex's usage limits, as bars.

use chrono::{DateTime, Local, Utc};
use colored::Colorize;

use crate::core::usage::{self, FailedReport, Limit, ProviderUsage, Report, Usage};
use crate::core::{harness, usage_codex};
use crate::error::{Error, Result};

const BAR: usize = 20;

pub fn run(json: bool, refresh: bool) -> Result<()> {
    let claude = usage::get(&crate::core::discovery::claude_home(), refresh);
    let codex = usage_codex::get(&harness::codex_home());
    let now = Utc::now();
    match claude {
        // With no Claude read at all the command still fails (exit 1, the reason on
        // stderr), as it always has. Before that, `--json` prints `{ error, providers }`
        // (no Claude top-level fields) and the text output the Codex section.
        Err(e) => {
            if json {
                let r = FailedReport::new(&e, codex);
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", render_codex(&codex, now));
            }
            Err(Error::Other(e))
        }
        Ok(u) if json => {
            println!("{}", serde_json::to_string_pretty(&Report::new(u, codex))?);
            Ok(())
        }
        Ok(u) => {
            let codex = render_codex(&codex, now);
            let gap = if codex.is_empty() { "" } else { "\n" };
            print!("{}{gap}{codex}", render(&u, now));
            Ok(())
        }
    }
}

/// The Codex section; nothing when Codex isn't installed.
fn render_codex(p: &ProviderUsage, now: DateTime<Utc>) -> String {
    if p.status == "not_installed" {
        return String::new();
    }
    let plan = p.plan.as_ref().map(|x| format!(" — {x}"));
    let mut out = format!("{}{}\n", "Codex usage".bold(), plan.unwrap_or_default());
    out.push_str(&rows(&p.limits, now));
    let note = match (&p.observed_at, &p.error) {
        (Some(at), _) => ago(at, now).map(|a| format!("as of the last Codex turn, {a}")),
        (None, e) => e.clone(),
    };
    if let Some(n) = note {
        out.push_str(&format!("  {}\n", n.dimmed()));
    }
    out
}

/// `12m ago`, `3h 5m ago`, `2d ago`.
fn ago(ts: &str, now: DateTime<Utc>) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let mins = (now - t).num_minutes().max(0);
    Some(if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 24 * 60 {
        format!("{}h {}m ago", mins / 60, mins % 60)
    } else {
        format!("{}d ago", mins / (24 * 60))
    })
}

/// One aligned row per limit.
fn rows(limits: &[Limit], now: DateTime<Utc>) -> String {
    let width = label_width(limits);
    let mut out = String::new();
    for l in limits {
        out.push_str(&format!(
            "  {:<width$}  {}  {:>4}  {}\n",
            l.label,
            bar(l),
            format!("{:.0}%", l.percent),
            l.resets_at
                .as_deref()
                .and_then(|r| resets(r, now))
                .unwrap_or_default()
                .dimmed(),
        ));
    }
    out
}

fn label_width(limits: &[Limit]) -> usize {
    limits
        .iter()
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(0)
}

fn render(u: &Usage, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    let who = u.account.as_ref().map(|a| {
        let name = a
            .email
            .clone()
            .or(a.organization.clone())
            .unwrap_or_default();
        match &a.plan_label {
            Some(p) if !name.is_empty() => format!("{name} ({p})"),
            Some(p) => p.clone(),
            None => name,
        }
    });
    out.push_str(&format!(
        "{}{}\n",
        "Claude usage".bold(),
        who.map(|w| format!(" — {w}")).unwrap_or_default()
    ));
    let width = label_width(&u.limits);
    out.push_str(&rows(&u.limits, now));
    if let Some(x) = &u.extra_usage {
        let cur = x.currency.as_deref().unwrap_or("");
        let text = match (x.enabled, x.used, x.limit) {
            (false, _, _) => "off".to_string(),
            (true, Some(used), Some(limit)) => format!("{used:.2} / {limit:.2} {cur}"),
            (true, Some(used), None) => format!("{used:.2} {cur} spent"),
            (true, None, _) => "on".to_string(),
        };
        out.push_str(&format!("  {:<width$}  {}\n", "Extra usage", text.dimmed()));
    }
    if u.stale {
        out.push_str(&format!(
            "{}\n",
            format!(
                "as of {} — {}",
                local_time(&u.fetched_at).unwrap_or_default(),
                u.error.as_deref().unwrap_or("refresh failed")
            )
            .yellow()
        ));
    }
    out
}

fn bar(l: &Limit) -> String {
    let filled = ((l.percent / 100.0) * BAR as f64)
        .round()
        .clamp(0.0, BAR as f64) as usize;
    let s = format!("{}{}", "█".repeat(filled), "░".repeat(BAR - filled));
    let band = usage::severity_for(l.percent);
    if band == "critical" || matches!(l.severity.as_str(), "critical" | "exceeded" | "blocked") {
        s.red().to_string()
    } else if band == "warning" || l.severity == "warning" {
        s.yellow().to_string()
    } else {
        s.green().to_string()
    }
}

fn local_time(ts: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Local);
    Some(t.format("%H:%M").to_string())
}

/// `resets in 2h 20m` within a day, `resets Fri 14:00` otherwise.
fn resets(ts: &str, now: DateTime<Utc>) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let mins = (t - now).num_minutes().max(0);
    Some(if mins < 60 {
        format!("resets in {mins}m")
    } else if mins < 24 * 60 {
        format!("resets in {}h {}m", mins / 60, mins % 60)
    } else {
        format!("resets {}", t.with_timezone(&Local).format("%a %H:%M"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_section() {
        let now = DateTime::parse_from_rfc3339("2026-10-16T11:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let limit = Limit {
            kind: "session".into(),
            group: "session".into(),
            label: "5h limit".into(),
            model: None,
            percent: 50.0,
            severity: "normal".into(),
            resets_at: Some("2026-10-16T12:00:00+00:00".into()),
            active: true,
        };
        let mut p = ProviderUsage {
            id: "codex".into(),
            label: "Codex".into(),
            status: "ok".into(),
            plan: Some("Plus".into()),
            limits: vec![limit],
            fetched_at: Some(now.to_rfc3339()),
            observed_at: Some("2026-10-16T10:30:00Z".into()),
            error: None,
            credits: None,
        };
        let text = render_codex(&p, now);
        for want in [
            "Codex usage",
            " — Plus",
            "5h limit",
            "50%",
            "resets in 1h 0m",
            "30m ago",
        ] {
            assert!(text.contains(want), "{want:?} not in {text:?}");
        }
        p.status = "no_data".into();
        p.limits.clear();
        p.observed_at = None;
        p.error = Some("no Codex sessions yet".into());
        assert!(render_codex(&p, now).contains("no Codex sessions yet"));
        p.status = "not_installed".into();
        assert_eq!(render_codex(&p, now), "");
    }

    #[test]
    fn reset_times() {
        let now = DateTime::parse_from_rfc3339("2026-09-29T20:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            resets("2026-09-29T20:45:00+00:00", now).unwrap(),
            "resets in 45m"
        );
        assert_eq!(
            resets("2026-09-29T22:20:00+00:00", now).unwrap(),
            "resets in 2h 20m"
        );
        assert!(
            resets("2026-10-02T12:00:00+00:00", now)
                .unwrap()
                .starts_with("resets ")
        );
    }
}
