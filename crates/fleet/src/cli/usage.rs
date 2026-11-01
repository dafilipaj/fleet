//! `fleet usage`: the Claude and ChatGPT (Codex) subscriptions' usage limits, as bars.

use chrono::{DateTime, Local, Utc};
use colored::Colorize;

use crate::core::usage::{self, Limit, Usage};
use crate::error::{Error, Result};

const BAR: usize = 20;

pub fn run(json: bool, refresh: bool) -> Result<()> {
    let u = usage::get_all(
        &crate::core::discovery::claude_home(),
        &crate::core::harness::codex_home(),
        refresh,
    )
    .map_err(Error::Other)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&u)?);
    } else {
        let now = Utc::now();
        let mut out = render(&u, now);
        if let Some(o) = &u.openai {
            out.push('\n');
            out.push_str(&render(o, now));
        }
        print!("{out}");
    }
    Ok(())
}

fn render(u: &Usage, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    let openai = u.provider == "openai";
    let title = if openai {
        "ChatGPT usage"
    } else {
        "Claude usage"
    };
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
        title.bold(),
        who.map(|w| format!(" — {w}")).unwrap_or_default()
    ));
    let width = u
        .limits
        .iter()
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(0);
    for l in &u.limits {
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
    if u.limits.is_empty()
        && let Some(e) = &u.error
    {
        out.push_str(&format!("  {}\n", e.yellow()));
        return out;
    }
    if let Some(x) = u.extra_usage.as_ref().filter(|_| openai) {
        let text = match (x.enabled, x.balance) {
            (false, _) => "none".to_string(),
            (true, Some(b)) => format!("{b:.2} left"),
            (true, None) => "on".to_string(),
        };
        out.push_str(&format!("  {:<width$}  {}\n", "Credits", text.dimmed()));
    } else if let Some(x) = &u.extra_usage {
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
    if l.percent >= 90.0 || matches!(l.severity.as_str(), "critical" | "exceeded" | "blocked") {
        s.red().to_string()
    } else if l.percent >= 70.0 || l.severity == "warning" {
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
