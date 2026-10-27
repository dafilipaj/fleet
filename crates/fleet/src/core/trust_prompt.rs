//! A folder-trust dialog, without consulting a Session registry or transcript.
//!
//! The signals are the web spawner's (`web/lib/spawn.mjs`, `acceptTrustPrompt`): Claude Code's
//! two answers with the `❯` cursor on one of them, and Codex's `› 1. Trust and continue`. On
//! top of them come `answer`'s rule for a live prompt (nothing else on the screen with it) and
//! what Claude Code 2.1.311 was recorded doing (`testdata/screens/live/claude-trust-*`): while
//! the dialog listens, the terminal's cursor sits on its `❯` and only blank rows are under its
//! footer. Claude Code draws on the main screen, so when it exits (Esc) the dialog's text stays
//! there with the shell's prompt, and the cursor, under it: that is no prompt.

use serde::Serialize;
use serde_json::{Value, json};

use super::{backend::strip_ansi, prompt::VISIBLE_MARK, transcript::fnv1a};

const YES: &str = "Yes, I trust this folder";
const NO: &str = "No, exit";
const FOOTER: &str = "Enter to confirm · Esc to cancel";
/// Codex's accepting answer (C10: recognised, not yet read).
const CODEX_YES: &str = "Trust and continue";
/// At most this many lines of text between the dialog's top and its answers.
const MAX_LINES: usize = 60;
/// At most this many characters in one of them.
const MAX_LINE_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub label: String,
    pub accept: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub lines: Vec<String>,
    pub choices: Vec<Choice>,
    pub selected: usize,
}

impl Prompt {
    pub fn untouched(&self) -> bool {
        !self.choices[self.selected].accept
    }

    pub fn json(&self) -> Value {
        json!({ "kind": "trust", "harness": "claude", "id": self.id,
            "lines": self.lines, "choices": self.choices, "selected": self.selected,
            "untouched": self.untouched() })
    }
}

#[derive(Debug)]
pub struct Read {
    pub prompt: Option<Prompt>,
    /// The harness whose answers were recognised, but whose dialog is not read whole.
    pub incomplete: Option<&'static str>,
    pub detail: Option<&'static str>,
    /// `no_prompt` because a whole dialog is still drawn but the terminal has moved on below
    /// it: Claude Code exited and left its text. Its answer did not take it off the screen.
    pub stale: bool,
}

impl Read {
    pub fn json(&self) -> Value {
        if let Some(p) = &self.prompt {
            p.json()
        } else if let Some(harness) = self.incomplete {
            json!({ "kind": "trust", "harness": harness, "incomplete": true,
                "id": null, "lines": null, "choices": null, "selected": null,
                "untouched": false })
        } else {
            Value::Null
        }
    }
}

/// What tmux says about the pane in the same command as the capture, so at the same instant.
#[derive(Debug, Clone, Copy)]
pub struct Pane<'a> {
    /// Its foreground command (`pane_current_command`).
    pub command: &'a str,
    /// The terminal's cursor on the visible screen (`cursor_x`, `cursor_y`), from 0.
    pub cursor: (usize, usize),
    /// The visible screen's height in rows (`pane_height`).
    pub rows: usize,
    /// In a mode, such as copy mode (`pane_in_mode`): keys go to tmux, not to the harness.
    pub in_mode: bool,
}

/// A pane's foreground command that is its shell: the harness isn't running (yet, or any
/// more). The web spawner's `ensureStarted` check, a login shell's leading `-` dropped.
pub fn is_shell(command: &str) -> bool {
    matches!(
        command.trim().trim_start_matches('-'),
        "zsh" | "bash" | "sh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu"
    )
}

/// A frame's horizontal rule (`───` over a dialog, or `▔▔▔`), with its corners or not.
fn rule(line: &str) -> bool {
    let l = line
        .trim()
        .trim_matches(['╭', '╮', '╰', '╯', '┌', '┐', '└', '┘']);
    l.chars().count() >= 8 && l.chars().all(|c| "─━═▔▁".contains(c))
}

/// A line as drawn, without a side frame (`│ … │`) or trailing blanks, as `answer` reads it.
fn literal(line: &str) -> &str {
    let l = line.trim_end();
    let edges: &[char] = &['│', '┃', '║'];
    if l.starts_with(edges) && l.ends_with(edges) {
        let mut chars = l.chars();
        chars.next();
        chars.next_back();
        chars.as_str().trim_end()
    } else {
        l
    }
}

/// Label and cursor, using the web spawner's `❯ Yes, I trust` signal, with C3's
/// exact labels and optional numbering. Neither a digit nor Esc is an answer key here.
fn answer(line: &str) -> (&str, bool) {
    let l = line.trim();
    let (l, cursor) = l
        .strip_prefix('❯')
        .map_or((l, false), |s| (s.trim_start(), true));
    let digits = l.bytes().take_while(u8::is_ascii_digit).count();
    let l = if digits > 0 {
        l[digits..].strip_prefix(". ").unwrap_or(l)
    } else {
        l
    };
    (l.trim(), cursor)
}

/// The first line of the dialog's text, under its frame's rule: the heading Claude Code 2.1.311
/// opens it with. Without it the top is the last rule above the answers; without that, `clipped`.
fn top(line: &str) -> bool {
    line.trim() == "Accessing workspace:"
}

/// The line the shell echoed when the harness was started: `spawn`'s launch line (`cd <dir>
/// && <launcher> …`, `backend::launch_command`) or a `claude`/`cc` typed after a shell prompt.
/// A line of a Claude Code transcript (`⏺ Bash(cd x && claude)`) never is one.
fn launch(line: &str) -> bool {
    let l = line.trim();
    if l.starts_with(['⏺', '●', '⎿', '│']) {
        return false;
    }
    // `cd ` as a word, at the start or after a blank (behind any shell prompt).
    let spawned = std::iter::once(l)
        .chain(l.match_indices(" cd ").map(|(i, _)| &l[i + 1..]))
        .filter_map(|s| s.strip_prefix("cd "))
        .any(|after| {
            after
                .split_once(" && ")
                .is_some_and(|(dir, rest)| !dir.trim().is_empty() && !rest.trim().is_empty())
        });
    spawned
        || ["$ ", "% ", "# ", "> ", "❯ "].iter().any(|marker| {
            l.split_once(marker)
                .is_some_and(|(_, command)| claude_word(command))
        })
}

/// `command` runs `claude` or `cc` (a path to one too), behind any `NAME=value` assignments
/// and `command`, `exec`, `env` or `nohup` (`CLAUDE_CONFIG_DIR=… command claude`).
fn claude_word(command: &str) -> bool {
    command
        .split_whitespace()
        .find(|word| !assignment(word) && !matches!(*word, "command" | "exec" | "env" | "nohup"))
        .is_some_and(|word| {
            matches!(
                word.trim_matches(['\'', '"']).rsplit('/').next(),
                Some("claude" | "cc")
            )
        })
}

/// A shell word `NAME=value`.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    })
}

/// Codex's trust dialog: an answer `Trust and continue`, numbered or not, cursor or not.
fn codex_answer(line: &str) -> bool {
    let l = line.trim().trim_start_matches(['›', '❯', '>']).trim_start();
    let digits = l.bytes().take_while(u8::is_ascii_digit).count();
    let l = l[digits..].strip_prefix(". ").unwrap_or(l);
    l.trim() == CODEX_YES
}

/// C2's identity of a dialog's text: its paragraphs (lines between blank ones), each one's
/// lines joined by a blank, so where Claude Code wraps a paragraph to the pane's width (at a
/// blank) does not change it.
fn paragraphs(lines: &[String]) -> Vec<String> {
    lines
        .split(String::is_empty)
        .map(|p| p.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "))
        .collect()
}

/// Read only the visible screen. Scrollback is never used to complete a trust dialog.
pub fn read(screen: &str, pane: &Pane) -> Read {
    let absent = |detail| Read {
        prompt: None,
        incomplete: None,
        detail: Some(detail),
        stale: false,
    };
    if is_shell(pane.command) {
        return absent("shell");
    }
    let screen = screen.rsplit_once(VISIBLE_MARK).map_or(screen, |(_, s)| s);
    // Row for row (a blank last row too: the cursor's row is counted from the bottom).
    let plain: Vec<_> = screen.lines().map(strip_ansi).collect();
    let raw: Vec<_> = plain.iter().map(String::as_str).collect();
    let lines: Vec<_> = raw.iter().map(|s| literal(s)).collect();
    let incomplete = |harness, detail| Read {
        prompt: None,
        incomplete: Some(harness),
        detail: Some(detail),
        stale: false,
    };
    // C10: Codex's dialog is recognised by the web spawner's signal, and not read until its
    // screens are recorded.
    if lines.iter().any(|l| codex_answer(l)) {
        return incomplete("codex", "layout_unsupported");
    }
    // Recognised by its accepting answer: "No, exit" alone is other dialogs' answer too (the
    // bypass-permissions warning Claude Code may show next).
    let labels: Vec<_> = lines.iter().map(|l| answer(l)).collect();
    if !labels
        .iter()
        .any(|(label, _)| label.starts_with("Yes, I trust"))
    {
        return absent("no_prompt");
    }
    let rows: Vec<_> = labels
        .iter()
        .enumerate()
        .filter(|(_, (label, _))| matches!(*label, YES | NO))
        .map(|(i, (label, cursor))| (i, *label, *cursor))
        .collect();
    if rows.len() > 2 {
        return absent("not_alone");
    }
    let unknown = labels.iter().any(|(label, _)| {
        *label != YES
            && *label != NO
            && (label.starts_with("Yes, I trust") || label.starts_with(NO))
    });
    if unknown {
        return incomplete("claude", "layout_unsupported");
    }
    if rows.len() != 2 || rows[0].1 == rows[1].1 {
        return incomplete("claude", "clipped");
    }
    let first = rows[0].0;
    let last = rows[1].0;
    if lines[first + 1..last].iter().any(|l| !l.trim().is_empty()) {
        return incomplete("claude", "layout_unsupported");
    }
    let selected: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.2)
        .map(|(i, _)| i)
        .collect();
    if selected.len() != 1 {
        return incomplete("claude", "layout_unsupported");
    }
    // Where the dialog ends: its footer, when that is the first line under its answers.
    let under = (last + 1..lines.len()).find(|&i| !lines[i].trim().is_empty());
    let end = under.filter(|&i| lines[i].trim() == FOOTER).unwrap_or(last);
    // Live: the terminal's cursor where Claude Code keeps it while the dialog listens, on the
    // selected answer's `❯`. Its row counted from the bottom: `-J` joined the rows tmux wrapped
    // above it (a long launch line), and only the dialog's own rows and blanks are below it.
    let cursor = lines.len() as isize - (pane.rows as isize - pane.cursor.1 as isize);
    if cursor > end as isize {
        // The terminal has moved on under the dialog: Claude Code exited and left its text.
        return Read {
            stale: true,
            ..absent("no_prompt")
        };
    }
    let chosen = rows[selected[0]].0;
    let column = raw[chosen].chars().take_while(|&c| c != '❯').count();
    if cursor != chosen as isize || pane.cursor.0 != column {
        return incomplete("claude", "layout_unsupported");
    }
    // Its top: the frame's top rule right above its heading, else the heading; without the
    // heading, the last rule above its answers. Neither is `clipped`: cut off.
    let header = lines[..first].iter().rposition(|l| top(l));
    let frame = match header {
        Some(h) => raw[..h]
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .filter(|&i| rule(raw[i])),
        None => raw[..first].iter().rposition(|l| rule(l)),
    };
    if frame.is_none() && raw[..first].iter().any(|l| l.starts_with(['│', '┃', '║'])) {
        return incomplete("claude", "clipped");
    }
    let Some(start) = frame.or(header) else {
        return incomplete("claude", "clipped");
    };
    let body_start = if frame == Some(start) {
        start + 1
    } else {
        start
    };
    let mut detail = None;
    // Above its top only what the shell printed before the harness started: the launch line
    // last, anything the shell printed before it above that.
    if lines[..start]
        .iter()
        .rfind(|l| !l.trim().is_empty())
        .is_some_and(|l| !launch(l))
    {
        detail = Some("not_alone");
    }
    // An input marker or a second answer list is never a warning. Don't interpret warning text.
    if lines[body_start..first].iter().any(|l| {
        let (label, cursor) = answer(l);
        cursor
            || l.trim_start().starts_with(['›', '>'])
            || (l.trim_start().starts_with(|c: char| c.is_ascii_digit()) && label != l.trim())
    }) {
        detail = Some("not_alone");
    }
    // The dialog is the last thing on the screen: under its answers its footer, then only
    // blank rows (and a side frame's bottom). One other line in the footer's place is a
    // footer Fleet does not know.
    if let Some(i) = under {
        let bottom = |l: &str| rule(l) && l.trim().starts_with(['╰', '└']);
        let more = lines[i + 1..]
            .iter()
            .any(|l| !(l.trim().is_empty() || bottom(l)));
        let input = lines[i].trim_start().starts_with(['❯', '›', '>']);
        if more || (i != end && input) {
            detail = Some("not_alone");
        } else if i != end {
            return incomplete("claude", "layout_unsupported");
        }
    }
    // In copy mode the keys would go to tmux: something else is on the pane with it.
    if pane.in_mode {
        detail = Some("not_alone");
    }
    // A run of blank lines kept as one; none before its first line or after its last; the
    // dialog's left padding, which every line has, left out.
    let mut body = Vec::<&str>::new();
    for l in &lines[body_start..first] {
        if l.is_empty() && body.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        body.push(l);
    }
    while body.last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    let indent = body
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let body: Vec<String> = body
        .iter()
        .map(|l| l.get(indent..).unwrap_or_default().to_string())
        .collect();
    if body.is_empty()
        || body.len() > MAX_LINES
        || body.iter().any(|l| l.chars().count() > MAX_LINE_CHARS)
    {
        return incomplete("claude", "clipped");
    }
    let choices: Vec<_> = rows
        .iter()
        .map(|(_, label, _)| Choice {
            label: (*label).into(),
            accept: *label == YES,
        })
        .collect();
    let identity = std::iter::once("claude".to_string())
        .chain(paragraphs(&body))
        .chain(choices.iter().map(|c| c.label.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = Prompt {
        id: format!("t1.{:08x}", fnv1a(identity.as_bytes())),
        lines: body,
        choices,
        selected: selected[0],
    };
    if detail.is_none() && !prompt.untouched() {
        detail = Some("touched");
    }
    Read {
        prompt: Some(prompt),
        incomplete: None,
        detail,
        stale: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/screens/live");

    /// A recorded screen (`capture-pane -p -e`, SGR included) and the pane it was in, as fleet
    /// reads it: with `-J`, which joins the plain folder's launch line, wrapped at 80 columns.
    fn recorded(name: &str) -> (String, Pane<'static>) {
        let path = format!("{SCREENS}/claude-trust-{name}");
        let screen = std::fs::read_to_string(format!("{path}.ans")).unwrap();
        let screen = if name.starts_with("plain-80x24") {
            screen.replacen('\n', "", 1)
        } else {
            screen
        };
        let cursor = std::fs::read_to_string(format!("{path}.cursor")).unwrap();
        let (x, y) = cursor
            .split_whitespace()
            .next()
            .unwrap()
            .split_once(',')
            .unwrap();
        let pane = Pane {
            command: "node",
            cursor: (x.parse().unwrap(), y.parse().unwrap()),
            rows: if name.contains("80x24") { 24 } else { 50 },
            in_mode: false,
        };
        (screen, pane)
    }

    fn read_recorded(name: &str) -> Read {
        let (screen, pane) = recorded(name);
        read(&screen, &pane)
    }

    /// The recorded screen as text, for the tests that rewrite it.
    fn text(name: &str) -> String {
        recorded(name)
            .0
            .lines()
            .map(strip_ansi)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    /// `screen` in a pane its height, the cursor where Claude Code keeps it: on the `❯` of the
    /// selected answer.
    fn live(screen: &str) -> Pane<'static> {
        let lines: Vec<_> = screen.lines().map(strip_ansi).collect();
        let y = lines
            .iter()
            .rposition(|l| l.contains('❯') && (l.contains(YES) || l.contains(NO)))
            .unwrap_or(0);
        Pane {
            command: "node",
            cursor: (lines[y].chars().take_while(|&c| c != '❯').count(), y),
            rows: lines.len(),
            in_mode: false,
        }
    }

    fn read_live(screen: &str) -> Read {
        read(screen, &live(screen))
    }

    #[test]
    fn c9_recorded_dialogs_read_whole_at_both_sizes_with_every_warning() {
        for kind in ["plain", "permissions"] {
            for size in ["80x24", "200x50"] {
                let read = read_recorded(&format!("{kind}-{size}"));
                assert_eq!(read.detail, None, "{kind}-{size}: {read:?}");
                let p = read.prompt.unwrap();
                assert_eq!(p.selected, 0);
                assert_eq!(p.choices[0].label, "No, exit");
                assert!(!p.choices[0].accept);
                assert_eq!(p.choices[1].label, "Yes, I trust this folder");
                assert!(p.choices[1].accept);
                assert!(p.untouched());
                assert!(
                    p.id.strip_prefix("t1.").is_some_and(|h| h.len() == 8
                        && h.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
                    "{}",
                    p.id
                );
                // From its top (the heading under the rule) to the line above its first
                // answer; the dialog's padding, the rule and the link's OSC 8 left out.
                assert_eq!(p.lines[0], "Accessing workspace:");
                let folder = if kind == "plain" { "plain" } else { "perms" };
                assert_eq!(
                    p.lines[2],
                    format!("/Users/dariofilipaj/.trustcap-scratch/{folder}")
                );
                assert_eq!(p.lines.last().unwrap(), "Security guide");
                assert!(
                    !p.lines
                        .iter()
                        .any(|l| l.starts_with(' ') && !l.starts_with("  Bash("))
                );
                if kind == "permissions" {
                    assert!(p.lines.contains(
                        &"⚠ This folder pre-approves 12 tool permissions in .claude/settings.json:"
                            .to_string()
                    ));
                    assert!(p.lines.iter().any(|l| l.ends_with(", and 4 more")), "{p:?}");
                    assert!(p.lines.contains(
                        &"These will apply without asking. Only proceed if you trust this configuration."
                            .to_string()
                    ));
                }
            }
        }
        // A --resume launch shows the same dialog first.
        let resume = read_recorded("permissions-200x50-resume");
        assert_eq!(resume.detail, None, "{resume:?}");
        assert_eq!(
            resume.prompt.unwrap().id,
            read_recorded("permissions-200x50").prompt.unwrap().id
        );
    }

    #[test]
    fn c2_the_same_folder_at_80x24_and_200x50_has_one_id() {
        for kind in ["plain", "permissions"] {
            let narrow = read_recorded(&format!("{kind}-80x24")).prompt.unwrap();
            let wide = read_recorded(&format!("{kind}-200x50")).prompt.unwrap();
            // Claude Code wraps its text to the width: other lines, the same paragraphs.
            assert_ne!(narrow.lines, wide.lines);
            assert_eq!(narrow.id, wide.id, "{kind}");
        }
        let plain = read_recorded("plain-200x50").prompt.unwrap().id;
        let permissions = read_recorded("permissions-200x50").prompt.unwrap().id;
        assert_ne!(plain, permissions);
        // Another folder, or another word in a warning: another id.
        let s = text("permissions-200x50");
        for other in [
            s.replace("scratch/perms", "scratch/other"),
            s.replace("Bash(echo:*)", "Bash(rm:*)"),
            s.replace("and 4 more", "and 5 more"),
        ] {
            assert_ne!(read_live(&other).prompt.unwrap().id, permissions);
        }
    }

    #[test]
    fn c2_a_token_longer_than_the_width_is_cut_by_ink_and_gives_another_id() {
        // Ink hard-wraps a folder path longer than the pane mid-token; the paragraph's lines
        // are joined by a blank, so the id of such a dialog still changes with the width (the
        // safe side: `different_prompt`, never a wrong accept). Same width, same id.
        let long = format!(
            "/Users/dariofilipaj/.trustcap-scratch/{}",
            "a-very-long-folder-name-".repeat(5)
        );
        assert!(long.len() > 100);
        let wide =
            text("plain-200x50").replace("/Users/dariofilipaj/.trustcap-scratch/plain", &long);
        assert!(wide.contains(&long));
        let (head, tail) = long.split_at(79);
        let narrow = wide.replace(&long, &format!("{head}\n {tail}"));
        let wide_id = read_live(&wide).prompt.unwrap().id;
        let narrow_id = read_live(&narrow).prompt.unwrap().id;
        assert_ne!(wide_id, narrow_id);
        assert_eq!(narrow_id, read_live(&narrow).prompt.unwrap().id);
    }

    #[test]
    fn c3_an_accepted_dialog_leaves_claude_code_ui_and_no_prompt() {
        for t in ["0.5", "2", "5"] {
            let name = format!("perms-200x50-after-accept-{t}s");
            let (screen, pane) = recorded(&name);
            let r = read(&screen, &pane);
            assert_eq!(r.detail, Some("no_prompt"), "{name}: {r:?}");
            assert!(r.prompt.is_none() && r.incomplete.is_none(), "{name}");
            // Not the stale text of an exited Claude Code: the pane's foreground runs.
            assert!(!r.stale, "{name}");
            assert!(!screen.contains("Accessing workspace:"), "{name}");
        }
    }

    #[test]
    fn c3_down_and_up_move_the_cursor_and_keep_the_id() {
        for name in [
            "plain-80x24",
            "plain-200x50",
            "permissions-80x24",
            "permissions-200x50",
        ] {
            let id = read_recorded(name).prompt.unwrap().id;
            let down = read_recorded(&format!("{name}-down"));
            assert_eq!(down.detail, Some("touched"), "{name}: {down:?}");
            let p = down.prompt.unwrap();
            assert_eq!((p.selected, p.id.as_str()), (1, id.as_str()));
            let up = read_recorded(&format!("{name}-up"));
            assert_eq!(up.detail, None, "{name}: {up:?}");
            assert_eq!(up.prompt.unwrap().id, id);
        }
    }

    #[test]
    fn c3_a_dialog_left_above_the_shell_after_esc_is_no_prompt() {
        for name in [
            "plain-80x24-after-esc",
            "plain-200x50-after-esc",
            "permissions-80x24-after-esc",
            "permissions-200x50-after-esc",
            "permissions-200x50-resume-after-esc",
        ] {
            let (screen, pane) = recorded(name);
            // A foreground fleet doesn't take for a shell (`pwsh`, a wrapper still running).
            let r = read(&screen, &pane);
            assert_eq!(r.detail, Some("no_prompt"), "{name}: {r:?}");
            assert!(
                r.prompt.is_none() && r.incomplete.is_none() && r.stale,
                "{name}"
            );
            assert!(r.json().is_null());
            let r = read(
                &screen,
                &Pane {
                    command: "-zsh",
                    ..pane
                },
            );
            assert_eq!(r.detail, Some("shell"), "{name}");
            // Even with the cursor on its `❯`, the shell's prompt under its footer is something
            // else on the screen: never answerable.
            let r = read(&screen, &live(&screen));
            assert_eq!(r.detail, Some("not_alone"), "{name}: {r:?}");
        }
        // Claude Code gone before the shell drew its prompt: the cursor under the footer.
        let (screen, pane) = recorded("plain-200x50");
        let r = read(
            &screen,
            &Pane {
                cursor: (0, 18),
                ..pane
            },
        );
        assert_eq!((r.detail, r.stale), (Some("no_prompt"), true), "{r:?}");
    }

    #[test]
    fn c3_the_cursor_anywhere_else_on_the_dialog_is_a_layout_fleet_does_not_know() {
        let (screen, pane) = recorded("plain-200x50");
        for cursor in [(0, 14), (2, 14), (1, 15), (0, 0), (1, 17)] {
            let r = read(&screen, &Pane { cursor, ..pane });
            assert_eq!(
                (r.detail, r.incomplete),
                (Some("layout_unsupported"), Some("claude")),
                "{cursor:?}: {r:?}"
            );
            assert!(!r.stale);
        }
        let r = read(
            &screen,
            &Pane {
                in_mode: true,
                ..pane
            },
        );
        assert_eq!(r.detail, Some("not_alone"), "copy mode: {r:?}");
    }

    #[test]
    fn c3_the_launch_line_and_what_the_shell_printed_before_it_may_stand_above() {
        // The dialog alone, from its rule down, recorded where the shell had scrolled away.
        let (s, pane) = recorded("permissions-80x24");
        let id = read(&s, &pane).prompt.unwrap().id;
        for above in [
            // spawn's own line, whatever the launcher and the shell prompt
            "sh-3.2$ cd '/work/example' && '/opt/bin/claude-wrapper' --resume=x",
            "➜  example git:(main) ✗ cd '/work/example' && claude",
            "~/work/example on main\n❯ cd '/work/example' && claude",
            "Last login: Mon Oct 26 09:00:00\n\nuser@host example % cd /work/example && cc",
            // typed by hand, as the recordings were
            "user@host:~/example$ claude",
            "% /usr/local/bin/claude --resume=x",
            "user@host perms % CLAUDE_CONFIG_DIR=/x/cfg command claude --model haiku",
            "$ env FOO=1 exec claude",
        ] {
            // Lines above it don't move the cursor: tmux counts its row from the top.
            let n = above.lines().count();
            let pane = Pane {
                cursor: (pane.cursor.0, pane.cursor.1 + n),
                rows: pane.rows + n,
                ..pane
            };
            let r = read(&format!("{above}\n{s}"), &pane);
            assert_eq!(r.detail, None, "{above:?}: {r:?}");
            assert_eq!(r.prompt.unwrap().id, id, "{above:?}");
        }
        for above in [
            "⏺ Bash(cd /work/example && claude)",
            "  ⎿  cd /work/example && claude",
            "user@host example % cd /work/example && claude\nsomething it printed",
            "$ cd /work/example",
            "% claudette",
            "% FOO=1 claudette",
            "$ echo claude",
            "❯ Describe a claude session",
            "1. other answer",
        ] {
            let n = above.lines().count();
            let pane = Pane {
                cursor: (pane.cursor.0, pane.cursor.1 + n),
                rows: pane.rows + n,
                ..pane
            };
            let r = read(&format!("{above}\n{s}"), &pane);
            assert_eq!(r.detail, Some("not_alone"), "{above:?}: {r:?}");
        }
    }

    #[test]
    fn c3_under_its_answers_only_its_footer_then_blank_rows() {
        let s = text("plain-200x50");
        let r = read_live(&s.replace(FOOTER, ""));
        assert_eq!(r.detail, None, "no footer: {r:?}");
        for (with, detail) in [
            ("Esc to exit".to_string(), "layout_unsupported"),
            (format!("{FOOTER}\nsomething else"), "not_alone"),
            (format!("{FOOTER}\n\n\nuser@host plain %"), "not_alone"),
            (format!("{FOOTER}\na\nb\nc\nd"), "not_alone"),
            (format!("{FOOTER}\n> "), "not_alone"),
            (format!("{FOOTER}\n│ ❯ draft │"), "not_alone"),
            (format!("{FOOTER}\n{}", "─".repeat(40)), "not_alone"),
            ("> ".to_string(), "not_alone"),
        ] {
            let r = read_live(&s.replace(FOOTER, &with));
            assert_eq!(r.detail, Some(detail), "{with:?}: {r:?}");
        }
    }

    #[test]
    fn c3_recognised_by_its_accepting_answer_only() {
        // Claude Code's bypass-permissions warning answers "No, exit" too: not a trust prompt.
        let bypass = "WARNING: Claude Code running in Bypass Permissions mode\n\n\
                      ❯ 1. No, exit\n  2. Yes, I accept\n\nEnter to confirm · Esc to cancel";
        let r = read_live(bypass);
        assert_eq!(r.detail, Some("no_prompt"), "{r:?}");
        assert!(r.prompt.is_none() && r.incomplete.is_none() && !r.stale);
        // Its accepting answer alone, "No, exit" cut off under the screen: clipped.
        let s = text("plain-200x50");
        let cut: String = s
            .lines()
            .take_while(|l| !l.contains("No, exit"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n ❯ Yes, I trust this folder";
        let r = read_live(&cut);
        assert_eq!((r.detail, r.incomplete), (Some("clipped"), Some("claude")));
    }

    #[test]
    fn c3_its_top_is_the_rule_over_its_heading_not_one_the_shell_printed() {
        let s = text("plain-200x50");
        let id = read_live(&s).prompt.unwrap().id;
        let rule = "─".repeat(80);
        // A rule further up, in what the shell printed, is not the dialog's.
        let r = read_live(&format!("{rule}\n{s}"));
        assert_eq!(r.detail, None, "{r:?}");
        assert_eq!(r.prompt.unwrap().id, id);
        // Without its rule, the heading is its top.
        let bare = s.replacen(&"─".repeat(200), "", 1);
        let r = read_live(&bare);
        assert_eq!(r.detail, None, "{r:?}");
        assert_eq!(r.prompt.unwrap().id, id);
        // Without the heading it knows, the last rule above the answers is the top.
        let unknown = s.replace("Accessing workspace:", "A heading of another version");
        let framed = read_live(&unknown);
        assert_eq!(framed.detail, None, "{framed:?}");
        assert_eq!(
            framed.prompt.unwrap().lines[0],
            "A heading of another version"
        );
        // Neither: its top is off the screen.
        let headless = bare.replace("Accessing workspace:", "A heading of another version");
        assert_eq!(read_live(&headless).detail, Some("clipped"));
    }

    #[test]
    fn c2_side_frames_and_padding_are_not_the_dialog() {
        let s = text("plain-200x50");
        let original = read_live(&s).prompt.unwrap();
        let dialog = s
            .lines()
            .skip_while(|l| !l.contains("Accessing workspace:"))
            .map(|l| format!("│{l}│"))
            .collect::<Vec<_>>()
            .join("\n");
        let framed = format!("╭{}╮\n{dialog}\n╰{}╯", "─".repeat(40), "─".repeat(40));
        let r = read_live(&framed);
        assert_eq!(r.detail, None, "{r:?}");
        let p = r.prompt.unwrap();
        assert_eq!((p.id, p.lines), (original.id, original.lines));
        // Its sides with its top off the screen: clipped.
        assert_eq!(read_live(&dialog).detail, Some("clipped"));
    }

    #[test]
    fn c3_quoted_old_touched_or_accompanied_prompts_are_never_answerable() {
        let s = text("permissions-80x24");
        for screen in [
            format!("❯ Tell me about trust\n⏺ Quoted dialog:\n{s}"),
            format!("{s}❯ draft text"),
            s.replace("Security guide", "❯ 1. Another answer list"),
            format!("{s}────────────────────\n❯ \n────────────────────"),
        ] {
            assert_eq!(read_live(&screen).detail, Some("not_alone"), "{screen}");
        }
        let pane = Pane {
            command: "zsh",
            ..live(&s)
        };
        assert_eq!(read(&s, &pane).detail, Some("shell"));
        assert!(read(&s, &pane).prompt.is_none());
        let history = format!("{s}{}\n$", crate::core::prompt::VISIBLE_MARK);
        assert_eq!(read_live(&history).detail, Some("no_prompt"));
    }

    #[test]
    fn c2_c6_incomplete_unknown_and_codex_layouts_have_no_identity() {
        let s = text("plain-200x50");
        for (screen, detail) in [
            (s.lines().skip(4).collect::<Vec<_>>().join("\n"), "clipped"),
            (
                s.replace(YES, "Yes, I trust this folder (new)"),
                "layout_unsupported",
            ),
            (s.replace(FOOTER, "New footer"), "layout_unsupported"),
            (
                s.replace("❯ No, exit\n", "❯ No, exit\n   Maybe\n"),
                "layout_unsupported",
            ),
            (s.replace("Security guide", &"x".repeat(2001)), "clipped"),
            (
                s.replace("Security guide", &"warning\n".repeat(61)),
                "clipped",
            ),
            (
                "Trust this folder?\n› 1. Trust and continue\n  2. Exit".into(),
                "layout_unsupported",
            ),
            (
                "  1. Trust and continue\n› 2. Quit".into(),
                "layout_unsupported",
            ),
        ] {
            let r = read_live(&screen);
            assert_eq!(r.detail, Some(detail), "{r:?}");
            assert!(r.prompt.is_none());
            assert!(r.incomplete.is_some());
            assert_eq!(r.json()["incomplete"], true);
            assert_eq!(
                r.json()["harness"],
                if screen.contains("Trust and continue") {
                    "codex"
                } else {
                    "claude"
                }
            );
        }
    }

    #[test]
    fn c3_numbered_or_plain_answers_in_either_order() {
        let s = text("plain-200x50");
        for screen in [
            s.replace("❯ No, exit", "❯ 1. No, exit")
                .replace("  Yes, I trust", "  2. Yes, I trust"),
            s.replace(
                " ❯ No, exit\n   Yes, I trust this folder\n",
                "   Yes, I trust this folder\n ❯ No, exit\n",
            ),
        ] {
            let r = read_live(&screen);
            assert_eq!(r.detail, None, "{r:?}");
            assert!(r.prompt.unwrap().untouched());
        }
    }

    #[test]
    fn c2_blank_runs_are_kept_once() {
        let s = text("plain-200x50");
        let p = read_live(&s).prompt.unwrap();
        assert_eq!(p.lines[1], "", "{:?}", p.lines);
        for more in [
            s.replace("Security guide\n", "Security guide\n\n\n"),
            s.replace("scratch/plain\n", "scratch/plain\n\n\n\n"),
        ] {
            let repeated = read_live(&more).prompt.unwrap();
            assert_eq!(repeated.lines, p.lines);
            assert_eq!(repeated.id, p.id);
        }
    }
}
