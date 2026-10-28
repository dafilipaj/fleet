//! Shared formatting helpers for the plain (non-TUI) output.
//!
//! Everything here measures text in **display columns**, not chars: session names
//! are user-chosen and routinely carry CJK or emoji, which a `chars()` count
//! reports as half as wide as the terminal actually draws them.

use colored::{ColoredString, Colorize};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::core::discovery::Session;

/// How wide the terminal is, with a sane fallback when stdout isn't one.
pub fn term_width() -> usize {
    crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(120)
}

pub fn home_rel(path: &str) -> String {
    match dirs::home_dir().and_then(|h| h.to_str().map(String::from)) {
        Some(home) if path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_string(),
    }
}

/// Display width in terminal columns.
pub fn width_of(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Compact age from a ms-epoch timestamp: 12s / 4m / 3h / 2d.
pub fn ago(updated_at: Option<i64>) -> String {
    let Some(ms) = updated_at else {
        return "?".into();
    };
    let secs = ((chrono::Utc::now().timestamp_millis() - ms).max(0)) as f64 / 1000.0;
    if secs < 90.0 {
        format!("{}s", secs.round())
    } else if secs < 5400.0 {
        format!("{}m", (secs / 60.0).round())
    } else if secs < 172_800.0 {
        format!("{}h", (secs / 3600.0).round())
    } else {
        format!("{}d", (secs / 86400.0).round())
    }
}

/// Last `n` non-blank-prefixed lines, each hard-capped to the terminal width.
pub fn tail(text: &str, n: usize, width: usize) -> String {
    let capped: Vec<String> = text.trim_end().lines().map(|l| fit(l, width)).collect();
    let start = capped
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let slice = &capped[start..];
    let from = slice.len().saturating_sub(n);
    slice[from..].join("\n")
}

/// Truncate to at most `width` display columns, marking the cut with `…`.
/// Never pads — use [`column`] when you want a fixed-width cell.
pub fn fit(text: &str, width: usize) -> String {
    if width_of(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    // Leave one column for the ellipsis, and stop before a wide char would
    // straddle the boundary.
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    // A run of wide chars can leave a column short of `budget`; pad it so the
    // cell still lands on exactly `width`.
    while used < budget {
        out.push(' ');
        used += 1;
    }
    out.push('…');
    out
}

/// Pad to exactly `width` display columns, or truncate with an ellipsis so a long
/// name can't shove the columns behind it out of line. Names are user-chosen now,
/// so assume nothing.
pub fn column(text: &str, width: usize) -> String {
    let n = width_of(text);
    if n <= width {
        return format!("{text}{}", " ".repeat(width - n));
    }
    fit(text, width)
}

/// Squash a transcript-derived string into one safe line: control characters are
/// dropped (they corrupt a ratatui `Line`) and runs of whitespace collapse.
pub fn clean_text(text: &str) -> String {
    let stripped: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The terminal a session lives in, as one short token: `⧉ <tmux session>`,
/// `▣ <iTerm tab>`, or `-` when we know neither.
pub fn terminal_label(s: &Session) -> String {
    if let Some(t) = s.tmux_session.as_deref().filter(|t| !t.is_empty()) {
        return format!("⧉ {t}");
    }
    if let Some(t) = s.tab.as_deref().filter(|t| !t.is_empty()) {
        return format!("▣ {t}");
    }
    "-".to_string()
}

/// Glyph + word for a registry status. `waiting` is the one that matters: the
/// session is blocked on the user and nothing else will move until they look.
pub fn status_words(status: &str) -> (&'static str, &str) {
    match status {
        "busy" => ("●", "working"),
        "idle" => ("○", "idle"),
        "waiting" => ("⏸", "needs you"),
        other => ("·", other),
    }
}

/// The CTX column: `ctx 62%` padded to 8, dim under 60%, yellow to 85%, red past
/// it; blank when the session's usage is unknown.
pub fn ctx_cell(s: &Session) -> ColoredString {
    use crate::core::context::{Level, level};
    let Some(c) = &s.context else {
        return column("", 8).normal();
    };
    let text = column(&format!("ctx {}%", c.pct.min(999)), 8);
    match level(c.pct) {
        Level::Low => text.dimmed(),
        Level::Warn => text.yellow(),
        Level::Hot => text.red(),
    }
}

pub fn plain_table(rows: &[Session]) -> String {
    plain_table_titled(rows, None)
}

/// [`plain_table`] with the host named in the header (`fleet list --all-hosts`).
pub fn plain_table_titled(rows: &[Session], host: Option<&str>) -> String {
    let width = term_width().clamp(40, 200);
    let mut out = vec![
        format!(
            "\nFLEET{} — {} live session{}",
            host.map(|h| format!(" @ {h}")).unwrap_or_default(),
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        ),
        "─".repeat(width),
    ];
    if rows.is_empty() {
        out.push("(no running claude sessions found)".into());
        return out.join("\n");
    }
    // Data-driven identity column: as wide as the widest headline, within reason.
    let name_w = rows
        .iter()
        .map(|r| width_of(&r.headline()))
        .max()
        .unwrap_or(12)
        .clamp(8, 32);
    let term_w = rows
        .iter()
        .map(|r| width_of(&terminal_label(r)))
        .max()
        .unwrap_or(3)
        .clamp(3, 20);
    let any_ctx = rows.iter().any(|r| r.context.is_some());
    for r in rows {
        // Pad *before* colouring: `colored` writes raw escapes and ignores the
        // format width, so `{:<9}` on a ColoredString does nothing.
        let (dot, word) = status_words(&r.status);
        let done = r.done.is_some() && r.status == "idle";
        let state = column(if done { "done" } else { word }, 9);
        let (dot, state) = match r.status.as_str() {
            _ if done => ("✓".cyan(), state.cyan()),
            "busy" => (dot.green(), state.green()),
            "idle" => (dot.dimmed(), state.dimmed()),
            "waiting" => (dot.yellow(), state.yellow().bold()),
            _ => (dot.normal(), state.normal()),
        };
        let label = column(&r.headline(), name_w);
        let where_ = r.cwd.as_deref().map(home_rel).unwrap_or_else(|| "?".into());
        let ctx = if any_ctx {
            format!("{} ", ctx_cell(r))
        } else {
            String::new()
        };
        out.push(format!(
            "{dot} {} {state} {:>4}  {ctx}{:<5} {} {}",
            label.bold(),
            ago(r.updated_at),
            r.backend.label(),
            column(&terminal_label(r), term_w).cyan(),
            where_.dimmed(),
        ));
        let mut title = clean_text(r.title.as_deref().unwrap_or("(no prompt captured)"));
        if let Some(bg) = r.background.as_ref().filter(|b| !b.is_empty()) {
            title = format!("[{}] {title}", background_words(bg));
        }
        if let Some(note) = r
            .done
            .as_ref()
            .filter(|_| done)
            .and_then(|d| d.note.as_deref())
        {
            title = format!("[done: {}] {title}", clean_text(note));
        }
        out.push(format!("  {}", fit(&title, width.saturating_sub(2))));
    }
    out.join("\n")
}

/// `2 agents, 1 shell` — what runs beside the main thread.
pub fn background_words(bg: &crate::core::background::Background) -> String {
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if !bg.agents.is_empty() {
        parts.push(plural(bg.agents.len(), "agent"));
    }
    if bg.shells > 0 {
        parts.push(plural(bg.shells as usize, "shell"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // `list` shows what a session is *doing*, exactly like the dashboard does —
    // the session name only stands in while nothing has titled it.
    #[test]
    fn the_table_leads_with_the_headline() {
        let mut s = Session {
            pid: 7,
            name: Some("app-9d".into()),
            name_source: Some("derived".into()),
            cwd: Some("/home/x/Code/app".into()),
            status: "idle".into(),
            title: Some("why is the statusline blank".into()),
            ..Default::default()
        };
        // Untitled: the heuristic slug of the first prompt stands in.
        let out = plain_table(std::slice::from_ref(&s));
        assert!(out.contains("why-is-the-statusline-blank"), "{out}");

        s.gen_title = Some("statusline-blank".into());
        let out = plain_table(std::slice::from_ref(&s));
        assert!(out.contains("statusline-blank"), "{out}");
        assert!(!out.contains("app-9d"), "{out}");
    }

    #[test]
    fn the_table_has_a_ctx_column_when_usage_is_known() {
        let mut s = Session {
            pid: 7,
            status: "idle".into(),
            ..Default::default()
        };
        assert!(!plain_table(std::slice::from_ref(&s)).contains("ctx"));
        s.context = Some(crate::core::context::ContextUsage::new(
            90_000, 200_000, None,
        ));
        assert!(plain_table(std::slice::from_ref(&s)).contains("ctx 45%"));
    }

    #[test]
    fn columns_pad_and_truncate() {
        assert_eq!(column("app-23", 9), "app-23   ");
        assert_eq!(column("a-very-long-session-name", 9), "a-very-l…");
        assert_eq!(column("exactly9c", 9), "exactly9c");
    }

    // The whole point of unicode-width: `测试-session` is 10 chars but 12 columns,
    // and a chars()-based pad mis-aligns every row after it.
    #[test]
    fn columns_measure_display_width_not_chars() {
        assert_eq!(width_of("测试-session"), 12);
        assert_eq!("测试-session".chars().count(), 10);

        let c = column("测试-session", 16);
        assert_eq!(width_of(&c), 16);

        // Truncation lands on exactly the requested width even when the cut
        // falls in the middle of a double-wide char.
        for w in 2..12 {
            assert_eq!(width_of(&fit("测试测试测试", w)), w, "fit to {w}");
        }
        assert_eq!(fit("测试测试", 5), "测试…");

        assert_eq!(width_of("🚀-deploy"), 9);
        assert_eq!(width_of(&column("🚀-deploy", 12)), 12);
        assert_eq!(width_of(&fit("🚀-deploy-the-thing", 9)), 9);
    }

    #[test]
    fn fit_is_a_no_op_when_it_fits() {
        assert_eq!(fit("short", 20), "short");
        assert_eq!(fit("", 0), "");
    }

    #[test]
    fn clean_text_strips_control_chars() {
        assert_eq!(clean_text("a\x1b[31mb\tc\n d"), "a [31mb c d");
        assert_eq!(clean_text("  spaced   out \r\n"), "spaced out");
    }

    #[test]
    fn terminal_label_prefers_tmux() {
        let mut s = Session::default();
        assert_eq!(terminal_label(&s), "-");
        s.tab = Some("zsh".into());
        assert_eq!(terminal_label(&s), "▣ zsh");
        s.tmux_session = Some("api-server".into());
        assert_eq!(terminal_label(&s), "⧉ api-server");
    }

    #[test]
    fn age_and_tail() {
        assert_eq!(ago(None), "?");
        assert!(ago(Some(chrono::Utc::now().timestamp_millis() - 120_000)).ends_with('m'));
        assert_eq!(tail("\n\na\nb\nc\n", 2, 80), "b\nc");
    }
}
