//! Prompts a session's terminal shows, read off its screen (`tmux capture-pane`): Claude
//! Code's permission prompts ("Do you want to proceed?") and `AskUserQuestion` form, and
//! Codex's approval prompts ("Would you like to run the following command?").
//!
//! A prompt counts only when it is the **live** one: its numbered answers sit at the bottom of
//! the screen with at most a short footer under them and no input box (composer). The same
//! words quoted in the conversation are followed by more conversation and the composer, so
//! they never count.

use serde::Serialize;

use crate::core::harness::Harness;
use crate::core::questions::Question;
use crate::core::transcript::fnv1a;

/// At most this many non-empty lines may follow a live prompt's last answer (its footer:
/// "Esc to cancel · Tab to amend", "Press enter to confirm or esc to cancel").
const MAX_FOOTER_LINES: usize = 4;
/// How far above its answers a Codex prompt's title may be (the command, its reason and the
/// environment lie between them).
const MAX_CODEX_PROMPT_LINES: usize = 60;
/// How far above its answers a Claude Code prompt's frame may be: a tall command pushes it
/// up, into the scrollback the caller captures.
const MAX_CLAUDE_PROMPT_LINES: usize = 1000;
/// How far above its first answer a question form's top may be.
const MAX_FORM_HEAD_LINES: usize = 40;

/// The line a capture puts between scrollback and the visible screen. Only the visible
/// screen decides whether a prompt is live; the scrollback above only extends it upward (the
/// top of a tall prompt). A text without it is all visible.
pub const VISIBLE_MARK: &str = "\u{1}fleet:visible\u{1}";

/// The screen's lines without their frames, and the index of the first visible one.
fn screen_lines(screen: &str) -> (Vec<&str>, usize) {
    let mut lines = Vec::new();
    let mut visible_from = 0;
    for l in screen.lines() {
        if l == VISIBLE_MARK {
            visible_from = lines.len();
        } else {
            lines.push(content(l));
        }
    }
    (lines, visible_from)
}

/// Characters drawing boxes around a prompt or the screen, never part of its text.
const FRAME: &[char] = &['│', '┃', '╭', '╮', '╰', '╯', '║'];

/// Cursor markers a selection list puts before the focused answer.
const MARKERS: &[char] = &['❯', '›', '>'];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    /// 1-based, as on screen.
    pub n: usize,
    pub label: String,
    /// The one key that picks this answer outright (tmux key name): Claude Code takes the
    /// answer's number, Codex the shortcut in its label (`(y)`, `(esc)`). `None`: pick it by
    /// moving the cursor.
    #[serde(skip)]
    pub key: Option<String>,
}

/// A permission / approval prompt on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenPrompt {
    /// Fingerprint of the prompt's wording (title, detail, answers): pass it back with
    /// `--prompt` so the answer lands only on this very prompt.
    pub id: String,
    /// The question it asks ("Do you want to proceed?").
    pub title: String,
    /// What it is about: the lines between its top and its answers (a command, a file).
    pub detail: Vec<String>,
    pub choices: Vec<Choice>,
    /// The answer the cursor is on (1 when untouched).
    pub selected: usize,
}

/// What reading a permission prompt off the screen found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptRead {
    None,
    /// A prompt is there, but not all of it: what it is about is out of the captured text, so
    /// it could not be told apart from another prompt with the same answers.
    Incomplete(String),
    Prompt(ScreenPrompt),
}

/// A screen line without its frame and surrounding whitespace.
fn content(line: &str) -> &str {
    line.trim().trim_matches(FRAME).trim()
}

/// A screen line as drawn, only without a side frame (`│ … │`: a box character at column 0
/// and one at the end) and trailing blanks: its indentation kept. A prompt's identity is made
/// of these, because whitespace is part of a command (`os.remove(x)` inside or outside an `if`
/// block is another command). And a rule at column 0 is the prompt's frame, while the lines
/// inside the frame are padded: a rule-like line of a command is indented, never mistaken for
/// the frame. So a line that starts with blanks keeps them, box characters at both ends or not:
/// an indented `╭────╮` or `│x│` is a line of the command, never one drawn from column 0.
fn literal(line: &str) -> &str {
    let l = line.trim_end();
    let mut ends = l.chars();
    match (ends.next(), ends.next_back()) {
        (Some(a), Some(b)) if FRAME.contains(&a) && FRAME.contains(&b) => {
            l[a.len_utf8()..l.len() - b.len_utf8()].trim_end()
        }
        _ => l,
    }
}

/// [`literal`] for every line [`screen_lines`] returns, at the same indexes.
fn literal_lines(screen: &str) -> Vec<&str> {
    screen
        .lines()
        .filter(|l| *l != VISIBLE_MARK)
        .map(literal)
        .collect()
}

/// A horizontal rule: solid (`───`, the top of Claude Code's prompt frame) or dashed (`╌╌╌`,
/// around the command inside it).
fn is_rule(c: &str) -> bool {
    c.chars().count() >= 8 && c.chars().all(|ch| "─━═╌╍┄┅-▔▁".contains(ch))
}

fn is_solid_rule(c: &str) -> bool {
    c.chars().count() >= 8 && c.chars().all(|ch| "─━═".contains(ch))
}

/// `❯ 1. Yes` → (cursor here, 1, "Yes").
fn option_line(c: &str) -> Option<(bool, usize, &str)> {
    let (cursor, rest) = match c.strip_prefix(MARKERS) {
        Some(r) => (true, r.trim_start()),
        None => (false, c),
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let n: usize = rest[..digits].parse().ok()?;
    let label = rest[digits..].strip_prefix(". ")?.trim();
    (n >= 1 && !label.is_empty()).then_some((cursor, n, label))
}

/// The input box: a prompt marker with no numbered answer after it.
fn is_composer(c: &str) -> bool {
    c.starts_with(MARKERS) && option_line(c).is_none() && unnumbered_row(c).is_none()
}

fn is_title(c: &str) -> bool {
    const STARTS: [&str; 5] = [
        "Do you want to",
        "Would you like to",
        "Allow ",
        "Do you trust",
        "Proceed?",
    ];
    STARTS.iter().any(|s| c.starts_with(s)) || c.contains("needs your approval")
}

/// The non-empty lines under the last numbered answer, if the screen ends in a live answer
/// list: (index of that answer, the footer lines).
fn live_answers<'a>(lines: &[&'a str], visible_from: usize) -> Option<(usize, Vec<&'a str>)> {
    let last = lines.iter().rposition(|c| option_line(c).is_some())?;
    if last < visible_from {
        return None; // only in the scrollback: no longer on screen
    }
    let footer: Vec<&str> = lines[last + 1..]
        .iter()
        .copied()
        .filter(|c| !c.is_empty())
        .collect();
    if footer.len() > MAX_FOOTER_LINES || footer.iter().any(|c| is_composer(c)) {
        return None;
    }
    Some((last, footer))
}

/// Claude Code's `AskUserQuestion` form (its footer says "Enter to select").
pub fn is_question_form(screen: &str) -> bool {
    let (lines, visible_from) = screen_lines(screen);
    live_answers(&lines, visible_from)
        .is_some_and(|(_, footer)| footer.iter().any(|c| c.contains("Enter to select")))
}

/// The key a Codex answer's label names as its shortcut: `… (y)` → `y`, `… (esc)` → `Escape`.
fn codex_shortcut(label: &str) -> Option<String> {
    let inner = label.strip_suffix(')')?.rsplit_once('(')?.1;
    match inner {
        "esc" => Some("Escape".into()),
        k if k.chars().count() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Some(k.into())
        }
        _ => None,
    }
}

/// Does the live list ending at `last` have the shape of Codex's approval answers, read whole
/// or not? An answer (or a line it wraps onto) names its shortcut (`(y)`, `(p)`, `(esc)`), or
/// the footer is the approval's ("Press enter to confirm or esc to cancel", wrapped or not).
/// Its popups (/model, /approvals, /review: "… or esc to go back"), its transcript pager and a
/// numbered draft have neither.
fn codex_answer_shape(lines: &[&str], last: usize, footer: &[&str]) -> bool {
    let answers = lines[..=last].iter().rev().take_while(|c| !c.is_empty());
    answers.chain(footer).any(|c| codex_shortcut(c).is_some())
        || footer.join(" ").contains("esc to cancel")
}

/// Is the live list typed into the input box — a draft, not a prompt? Claude Code draws its
/// input box between two plain rules from column 0, pi its editor too: a raw screen line that
/// is such a rule under the list (not a boxed prompt's bottom, `╰───╯`, nor a padded line).
/// Codex's composer has a footer of its own: "? for shortcuts", "92% context left", "⏎ send ⌃J
/// newline …". Neither is ever under a live prompt's answers.
fn in_input_box(raw_below: &[&str], footer: &[&str], claude: bool) -> bool {
    const COMPOSER: [&str; 3] = ["? for shortcuts", "context left", "⏎ send"];
    let composer = |c: &&str| COMPOSER.iter().any(|m| c.contains(m));
    raw_below.iter().any(|l| is_solid_rule(l.trim_end()))
        || (!claude && footer.iter().any(composer))
}

/// The permission / approval prompt the screen ends in, if any, read the way `harness` draws
/// it. Claude Code frames its prompt (a solid rule above it) and puts what it is about inside
/// the frame; Codex has no frame and puts it between the title and the answers.
pub fn permission_prompt(screen: &str, harness: Harness) -> PromptRead {
    let (lines, visible_from) = screen_lines(screen);
    let Some((last, footer)) = live_answers(&lines, visible_from) else {
        return PromptRead::None;
    };
    if footer.iter().any(|c| c.contains("Enter to select")) {
        return PromptRead::None; // an AskUserQuestion form: answered with --question
    }
    let claude = harness == Harness::Claude;
    let raw: Vec<&str> = screen.lines().filter(|l| *l != VISIBLE_MARK).collect();
    if in_input_box(&raw[last + 1..], &footer, claude) {
        return PromptRead::None;
    }
    // Codex (and pi, read by its rules): a list with the approval's answer shape is an approval,
    // and one that cannot be read whole is never taken for no prompt — a rename typed into it
    // would be its answer, and after a key it may be the one answered or a queued one. A list
    // without that shape (a popup, the transcript pager, a draft) is an approval only when it
    // is read whole under its title.
    let shaped = !claude && codex_answer_shape(&lines, last, &footer);
    let unread = |why: &str| {
        if shaped {
            PromptRead::Incomplete(why.into())
        } else {
            PromptRead::None
        }
    };
    const UNREAD_ANSWERS: &str = "the approval prompt's answers cannot be read whole";
    // Walk up the answer list: numbered N, N-1, … 1; a label that wraps continues on
    // un-numbered lines under it (Claude Code: up to 3; Codex wraps a long one further: the
    // command prefix in "don't ask again for commands that start with …").
    let Some((_, mut expect, _)) = option_line(lines[last]) else {
        return PromptRead::None;
    };
    let mut choices: Vec<(bool, Choice)> = Vec::new();
    let mut wrapped: Vec<&str> = Vec::new();
    let mut i = last;
    let top = loop {
        let c = lines[i];
        if let Some((cursor, n, label)) = option_line(c) {
            if n != expect {
                return unread(UNREAD_ANSWERS);
            }
            let mut full = label.to_string();
            for w in wrapped.drain(..).rev() {
                full.push(' ');
                full.push_str(w);
            }
            choices.push((
                cursor,
                Choice {
                    n,
                    label: full,
                    key: None,
                },
            ));
            if n == 1 {
                break i;
            }
            expect -= 1;
        } else if !c.is_empty() && (wrapped.len() < 3 || !claude) && !is_rule(c) {
            wrapped.push(c);
        } else {
            return unread(UNREAD_ANSWERS);
        }
        let Some(up) = i.checked_sub(1) else {
            return unread(UNREAD_ANSWERS);
        };
        i = up;
    };
    choices.reverse();
    if choices.len() < 2 {
        return unread(UNREAD_ANSWERS);
    }
    let cursors: Vec<usize> = choices
        .iter()
        .filter(|(cur, _)| *cur)
        .map(|(_, c)| c.n)
        .collect();
    let [selected] = cursors[..] else {
        return unread(UNREAD_ANSWERS);
    };
    let reach = if claude {
        MAX_CLAUDE_PROMPT_LINES
    } else {
        MAX_CODEX_PROMPT_LINES
    };
    let mut floor = top.saturating_sub(reach);
    let literal = literal_lines(screen);
    if !claude {
        // Codex draws no frame: its title, command and reason share one indent, under the last
        // conversation item (`•`, `›`), which starts at column 0. All between it and the
        // answers is the prompt; out of reach — or not on the captured text at all (a fresh or
        // cleared pane) — the prompt cannot be told whole. It is one if it has the answer shape
        // or a title in sight.
        let Some(above) = (floor..top)
            .rev()
            .find(|&j| literal[j].starts_with(|c: char| !c.is_whitespace()))
        else {
            const TOPLESS: &str = "the top of the approval prompt is off the captured screen";
            return if (floor..top).any(|j| is_title(lines[j])) {
                PromptRead::Incomplete(TOPLESS.into())
            } else {
                unread(TOPLESS)
            };
        };
        floor = above + 1;
    }
    // The title: the nearest prompt-like line above the answers. In a Codex prompt it must be
    // the only one, and read as a title: a line of its command that does ("Allow …") or ends
    // in "?" would otherwise cut the real title and everything above it out of the prompt. So
    // a Codex approval with none — not even a line ending in "?" — is not told whole either;
    // a list with no title and without the answer shape is no approval at all.
    let titles: Vec<usize> = (floor..top).filter(|&j| is_title(lines[j])).collect();
    let asks: Vec<usize> = (floor..top).filter(|&j| lines[j].ends_with('?')).collect();
    let t = match (claude, &titles[..], &asks[..]) {
        (true, [.., t], _) | (true, [], [.., t]) | (false, [t], _) => *t,
        (true, [], []) => return PromptRead::None,
        (false, [], _) => return unread("no line of the approval prompt reads as its title"),
        (false, _, _) => {
            return PromptRead::Incomplete(
                "more than one line of the approval prompt reads as its title (one is inside its command)".into(),
            );
        }
    };
    // A rule of the frame: drawn from column 0. A rule-like line inside it is padded content.
    let frame_rule = |j: usize| is_rule(lines[j]) && !literal[j].starts_with(char::is_whitespace);
    let detail_range = if claude {
        // Everything inside the frame, which starts at the solid rule above the title. A
        // frame out of sight would leave the prompt unidentifiable: refuse it.
        let Some(rule) = (floor..t)
            .rev()
            .find(|&j| frame_rule(j) && is_solid_rule(lines[j]))
        else {
            return PromptRead::Incomplete(
                "the top of the permission prompt is off the captured screen".into(),
            );
        };
        // The frame's own shape, exactly: its top, then either no other rule before the title
        // or one dashed pair around the command, with the header (" Bash command") between
        // the top and the pair. (The top is the nearest solid rule, so every rule under it is
        // dashed.) Any other rule at column 0 — a lone or a third dashed one, a pair right under
        // the top — means the rule taken for the top may be inside the command: the frame
        // cannot be told, and nothing is guessed.
        let inner: Vec<usize> = (rule + 1..t).filter(|&j| frame_rule(j)).collect();
        let shaped = match inner[..] {
            [] => true,
            [open, _] => (rule + 1..open).any(|j| !lines[j].is_empty()),
            _ => false,
        };
        if !shaped {
            return PromptRead::Incomplete(
                "the permission prompt's frame is ambiguous (a rule inside its command)".into(),
            );
        }
        rule + 1..top
    } else {
        floor..top
    };
    let detail: Vec<String> = detail_range
        .clone()
        .filter(|&j| j != t && !lines[j].is_empty())
        // Claude Code: all but the frame (a rule-like line of the command is the command);
        // Codex draws no frame, and its rules are decoration.
        .filter(|&j| {
            !if claude {
                frame_rule(j)
            } else {
                is_rule(lines[j])
            }
        })
        .map(|j| lines[j].to_string())
        .collect();
    let title = lines[t].to_string();
    let mut choices: Vec<Choice> = choices
        .into_iter()
        .map(|(_, mut c)| {
            c.key = match harness {
                Harness::Claude if c.n <= 9 => Some(c.n.to_string()),
                Harness::Codex => codex_shortcut(&c.label),
                _ => None,
            };
            c
        })
        .collect();
    // A key two answers claim picks neither for sure: those answers go by the cursor.
    let keys: Vec<Option<String>> = choices.iter().map(|c| c.key.clone()).collect();
    for c in &mut choices {
        if c.key.is_some() && keys.iter().filter(|k| **k == c.key).count() > 1 {
            c.key = None;
        }
    }
    // The identity: the title and every line of what it is about exactly as drawn — the
    // command's indentation, blank lines and rule-like lines included — then the answers. Not
    // Claude Code's frame rules: as wide as the pane, they would make one prompt another at
    // another width.
    let mut wording = literal[t].to_string();
    for j in detail_range.filter(|&j| j != t && !(claude && frame_rule(j))) {
        wording.push('\n');
        wording.push_str(literal[j]);
    }
    for c in &choices {
        wording.push_str(&format!("\n{}. {}", c.n, c.label));
    }
    PromptRead::Prompt(ScreenPrompt {
        id: format!("p2.{:08x}", fnv1a(wording.as_bytes())),
        title,
        detail,
        choices,
        selected,
    })
}

// ------------------------------------------------------------------ question forms

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One row of a question form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRow {
    /// The number on screen; `None` for multi-select's "Submit" / "Next" row.
    pub n: Option<usize>,
    /// Without its checkbox.
    pub label: String,
    /// Multi-select: whether the box is ticked; `None` without a box.
    pub checked: Option<bool>,
    /// The option's description (wrapped lines joined).
    pub description: String,
    pub cursor: bool,
}

/// Claude Code's `AskUserQuestion` form as the screen shows it: one tab per question, the
/// active question and its rows — the options, "Type something", (multi-select) "Submit" /
/// "Next", "Chat about this".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// (header, touched: ☒ — answered, or an option ticked).
    pub tabs: Vec<(String, bool)>,
    /// The active question, its wrapped lines joined.
    pub question: String,
    pub rows: Vec<FormRow>,
}

/// "Submit" / "Next" in a multi-select form (no number), with or without the cursor.
fn unnumbered_row(c: &str) -> Option<bool> {
    let (cursor, rest) = match c.strip_prefix(MARKERS) {
        Some(r) => (true, r.trim_start()),
        None => (false, c),
    };
    matches!(rest, "Submit" | "Next").then_some(cursor)
}

/// `[ ] Apple` / `[✔] Apple` → (Some(ticked), "Apple").
fn checkbox(label: &str) -> (Option<bool>, &str) {
    if let Some(rest) = label.strip_prefix("[ ]") {
        return (Some(false), rest.trim_start());
    }
    for tick in ["[✔]", "[✓]", "[x]", "[X]", "[■]"] {
        if let Some(rest) = label.strip_prefix(tick) {
            return (Some(true), rest.trim_start());
        }
    }
    (None, label)
}

/// `←  ☐ Color  ☒ Fruits  ✔ Submit  →` → [("Color", false), ("Fruits", true)].
fn parse_tabs(c: &str) -> Option<Vec<(String, bool)>> {
    if !c.starts_with('←') && !c.contains('☐') && !c.contains('☒') {
        return None;
    }
    let mut tabs = Vec::new();
    let mut rest = c;
    while let Some(i) = rest.find(['☐', '☒']) {
        let touched = rest[i..].starts_with('☒');
        let after = &rest[i + '☐'.len_utf8()..];
        let end = after.find(['☐', '☒', '✔', '→']).unwrap_or(after.len());
        tabs.push((after[..end].trim().to_string(), touched));
        rest = &after[end..];
    }
    (!tabs.is_empty()).then_some(tabs)
}

/// The question form the screen ends in, if any.
pub fn parse_form(screen: &str) -> Option<Form> {
    let (lines, visible_from) = screen_lines(screen);
    let (last, footer) = live_answers(&lines, visible_from)?;
    if !footer.iter().any(|c| c.contains("Enter to select")) {
        return None;
    }
    // Up from the last numbered row ("Chat about this") to row 1, over descriptions, the
    // Submit/Next row and the rule before "Chat about this".
    let mut first = None;
    let mut expect = option_line(lines[last])?.1;
    for i in (0..=last).rev() {
        if let Some((_, n, _)) = option_line(lines[i]) {
            if n != expect {
                return None;
            }
            if n == 1 {
                first = Some(i);
                break;
            }
            expect -= 1;
        }
        if last - i > 200 {
            return None;
        }
    }
    let first = first?;
    // The head: the tabs line and the question, up to the form's top rule.
    let floor = first.saturating_sub(MAX_FORM_HEAD_LINES);
    let mut tabs = Vec::new();
    let mut question = Vec::new();
    for j in (floor..first).rev() {
        let c = lines[j];
        if is_solid_rule(c) {
            break;
        }
        if let Some(t) = parse_tabs(c) {
            tabs = t;
            break;
        }
        if !c.is_empty() {
            question.push(c);
        }
    }
    question.reverse();
    let mut rows: Vec<FormRow> = Vec::new();
    for c in &lines[first..=last] {
        if c.is_empty() || is_rule(c) {
            continue;
        }
        if let Some((cursor, n, label)) = option_line(c) {
            let (checked, label) = checkbox(label);
            rows.push(FormRow {
                n: Some(n),
                label: label.to_string(),
                checked,
                description: String::new(),
                cursor,
            });
        } else if let Some(cursor) = unnumbered_row(c) {
            let label = c.trim_start_matches(MARKERS).trim().to_string();
            rows.push(FormRow {
                n: None,
                label,
                checked: None,
                description: String::new(),
                cursor,
            });
        } else if let Some(row) = rows.last_mut() {
            if !row.description.is_empty() {
                row.description.push(' ');
            }
            row.description.push_str(c);
        }
    }
    Some(Form {
        tabs,
        question: squash(&question.join(" ")),
        rows,
    })
}

impl Form {
    /// The row the cursor is on (exactly one).
    pub fn cursor(&self) -> Option<usize> {
        let mut at = self.rows.iter().enumerate().filter(|(_, r)| r.cursor);
        let (i, _) = at.next()?;
        at.next().is_none().then_some(i)
    }

    /// Which of `questions` the form shows, checked whole against them: the tabs (one per
    /// question, by header), the active question's full text, its options in order with
    /// their descriptions, then "Type something" (or what was typed there) and the rest.
    pub fn shows(&self, questions: &[Question]) -> Result<usize, String> {
        if !self.tabs.is_empty() {
            let headers: Vec<&str> = self.tabs.iter().map(|(h, _)| h.as_str()).collect();
            let want: Vec<&str> = questions.iter().map(|q| q.header.trim()).collect();
            if headers.len() != want.len()
                || headers
                    .iter()
                    .zip(&want)
                    .any(|(h, w)| !w.is_empty() && h != w)
            {
                return Err(format!(
                    "the form's tabs ({}) are not this question's",
                    headers.join(", ")
                ));
            }
        } else if questions.len() != 1 {
            return Err("the form has no tabs, the question has several parts".into());
        }
        let Some(active) = questions
            .iter()
            .position(|q| squash(&q.question) == self.question)
        else {
            return Err("the form asks another question".into());
        };
        let q = &questions[active];
        let k = q.options.len();
        if self.rows.len() < k + 2 {
            return Err("the form has other answers".into());
        }
        for (row, opt) in self.rows.iter().zip(&q.options) {
            if row.n.is_none()
                || squash(&row.label) != squash(&opt.label)
                || (!opt.description.trim().is_empty()
                    && squash(&row.description) != squash(&opt.description))
            {
                return Err(format!(
                    "the form's answer \"{}\" is not this question's",
                    row.label
                ));
            }
            if q.multi_select != row.checked.is_some() {
                return Err(
                    "the form's kind (single / multi-select) is not this question's".into(),
                );
            }
        }
        if self.rows[k].n != Some(k + 1) {
            return Err("the form has other answers".into());
        }
        if q.multi_select && self.rows[k + 1].n.is_some() {
            return Err("the multi-select form has no Submit / Next row".into());
        }
        Ok(active)
    }

    /// Nothing done to it yet: every tab unanswered, no box ticked, nothing typed, the cursor
    /// on the first answer.
    pub fn untouched(&self, q: &Question) -> bool {
        let k = q.options.len();
        self.tabs.iter().all(|(_, touched)| !touched)
            && self.rows.iter().all(|r| r.checked != Some(true))
            && self
                .rows
                .get(k)
                .is_some_and(|r| r.label.starts_with("Type something"))
            && self.cursor() == Some(0)
    }

    /// The ticked rows of a multi-select (indexes into `rows`).
    pub fn ticked(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.checked == Some(true))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Is the screen showing this `AskUserQuestion` prompt, untouched, as its live form?
pub fn screen_shows_question(screen: &str, questions: &[Question]) -> bool {
    parse_form(screen).is_some_and(|f| f.shows(questions) == Ok(0) && f.untouched(&questions[0]))
}

/// The review screen after the last question: the tabs, each question with the answer it
/// got (as shown: Claude Code joins a multi-select's picks and typed text with `", "`), and
/// whether "Submit answers" is focused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub tabs: Vec<(String, bool)>,
    pub items: Vec<(String, String)>,
    pub submit_focused: bool,
}

/// The review screen the screen ends in, if any.
///
/// ```text
/// ←  ☒ Color  ☒ Fruits  ✔ Submit  →
/// Review your answers
///  ● Pick a color
///    → Blue
///  ● Pick fruits
///    → Apple, kiwi  fruit
/// Ready to submit your answers?
/// ❯ 1. Submit answers
///   2. Cancel
/// ```
pub fn parse_review(screen: &str) -> Option<Review> {
    let (lines, visible_from) = screen_lines(screen);
    let (last, _) = live_answers(&lines, visible_from)?;
    let submit = (0..=last)
        .rev()
        .take(4)
        .find(|&i| option_line(lines[i]).is_some_and(|(_, _, l)| l == "Submit answers"))?;
    let head = (0..submit)
        .rev()
        .take(MAX_FORM_HEAD_LINES * 4)
        .find(|&i| lines[i] == "Review your answers")?;
    let tabs = head
        .checked_sub(1)
        .and_then(|i| parse_tabs(lines[i]))
        .unwrap_or_default();
    let mut items: Vec<(String, String)> = Vec::new();
    let mut answer_open = false;
    for c in &lines[head + 1..submit] {
        if c.is_empty() || c.starts_with("Ready to submit") {
            answer_open = false;
            continue;
        }
        if let Some(q) = c.strip_prefix("● ") {
            items.push((q.to_string(), String::new()));
            answer_open = false;
        } else if let Some(a) = c.strip_prefix("→ ") {
            let item = items.last_mut()?;
            item.1 = a.to_string();
            answer_open = true;
        } else if answer_open {
            // A long answer wraps: the TUI breaks it at a space.
            let item = items.last_mut()?;
            item.1.push(' ');
            item.1.push_str(c);
        } else {
            return None;
        }
    }
    Some(Review {
        tabs,
        items,
        submit_focused: option_line(lines[submit]).is_some_and(|(cur, _, _)| cur),
    })
}

/// The review screen after the last question, its "Submit answers" focused.
pub fn review_submit_focused(screen: &str) -> bool {
    parse_review(screen).is_some_and(|r| r.submit_focused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::questions::normalize_questions;
    use serde_json::json;

    fn prompt(screen: &str, h: Harness) -> ScreenPrompt {
        match permission_prompt(screen, h) {
            PromptRead::Prompt(p) => p,
            other => panic!("no prompt: {other:?}"),
        }
    }

    // The layout of Claude Code 2.1's permission prompt (wording synthetic).
    const CLAUDE_BASH: &str = "\
❯ Clean the build output
⏺ Removing the build output
  ⎿  $ rm -rf build

────────────────────────────────────────────────────────────────────────────────
 Bash command

 Remove the build output
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 rm -rf build
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to
      /w/app from this project
   3. No

 Esc to cancel · Tab to amend
";

    const CODEX_EXEC: &str = "\
› Run the shell command: touch probe.txt

• Running touch probe.txt


  Would you like to run the following command?

  Environment: local

  Reason: May I create probe.txt? The workspace is read-only.

  $ touch probe.txt


› 1. Yes, proceed (y)
  2. Yes, and don't ask again for commands that start with `touch probe-
     file.txt` (p)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";

    #[test]
    fn claude_permission_prompt_is_read_with_its_choices_and_keys() {
        let p = prompt(CLAUDE_BASH, Harness::Claude);
        assert_eq!(p.title, "Do you want to proceed?");
        assert_eq!(p.selected, 1);
        assert_eq!(
            p.choices
                .iter()
                .map(|c| (c.label.as_str(), c.key.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("Yes", Some("1")),
                (
                    "Yes, and always allow access to /w/app from this project",
                    Some("2")
                ),
                ("No", Some("3"))
            ]
        );
        assert_eq!(
            p.detail,
            ["Bash command", "Remove the build output", "rm -rf build"]
        );
        assert!(p.id.starts_with("p2."));
        let other = prompt(
            &CLAUDE_BASH.replace("rm -rf build", "rm -rf dist"),
            Harness::Claude,
        );
        assert_ne!(p.id, other.id);
        assert!(!is_question_form(CLAUDE_BASH));
    }

    #[test]
    fn a_command_taller_than_the_window_still_identifies_the_prompt() {
        let tall = |cmd: &str| {
            let body: String = (0..80).map(|i| format!(" {cmd} part {i}\n")).collect();
            CLAUDE_BASH.replace(" rm -rf build\n╌", &format!("{body}╌"))
        };
        let a = prompt(&tall("echo a"), Harness::Claude);
        let b = prompt(&tall("echo b"), Harness::Claude);
        assert_eq!(a.detail.len(), 82);
        assert_ne!(a.id, b.id, "different commands, different prompts");
        // Only the visible window (frame scrolled off): refused, not fingerprinted blind.
        let full = tall("echo a");
        let window: Vec<&str> = full.lines().rev().take(45).collect();
        let window: String = window.into_iter().rev().map(|l| format!("{l}\n")).collect();
        assert!(matches!(
            permission_prompt(&window, Harness::Claude),
            PromptRead::Incomplete(_)
        ));
    }

    // Codex draws no frame: its title, command and reason share one indent. A command line that
    // reads like a title ("Allow …") once became the title, and everything above it — the real
    // title, the reason, the head of the command — left the id: `rm -rf` and `echo` collided.
    #[test]
    fn a_codex_command_line_that_reads_like_a_title_never_hides_the_lines_above_it() {
        let with = |first: &str| {
            CODEX_EXEC.replace(
                "  $ touch probe.txt\n",
                &format!("  $ bash -lc '{first}\n  Allow listing\n  ls'\n"),
            )
        };
        let reads = [
            permission_prompt(&with("rm -rf /w/app"), Harness::Codex),
            permission_prompt(&with("echo hi"), Harness::Codex),
        ];
        // Two title-like lines in one prompt: which is the title cannot be told. Refused.
        for r in &reads {
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
        // The id covers the whole prompt, from the conversation above it down to the answers:
        // two commands that differ only above a line ending in "?" are two prompts.
        let ask = |first: &str| {
            CODEX_EXEC.replace(
                "  $ touch probe.txt\n",
                &format!("  $ bash -lc '{first}\n  echo really?\n  ls'\n"),
            )
        };
        let a = prompt(&ask("rm -rf /w/app"), Harness::Codex);
        let b = prompt(&ask("echo hi"), Harness::Codex);
        assert_eq!(a.title, "Would you like to run the following command?");
        assert_ne!(a.id, b.id);
        assert!(a.detail.iter().any(|d| d.contains("rm -rf /w/app")));
    }

    // The recorded Codex screens, at 40, 60 and full width: one title each, read whole.
    #[test]
    fn recorded_codex_approval_screens_are_read_whole() {
        for screen in [
            include_str!("../../../../testdata/codex/exec-approval-pane.txt"),
            include_str!("../../../../testdata/codex/exec-approval-pane-60.txt"),
            include_str!("../../../../testdata/codex/exec-approval-pane-40.txt"),
        ] {
            let p = prompt(screen, Harness::Codex);
            assert!(p.title.starts_with("Would you like to run the"), "{p:?}");
            assert!(
                p.detail
                    .iter()
                    .any(|d| d.contains("touch approval-probe.txt")),
                "{p:?}"
            );
            assert_eq!(p.choices.len(), 3);
        }
    }

    #[test]
    fn codex_approval_prompt_with_a_wrapped_label_and_shortcuts() {
        let p = prompt(CODEX_EXEC, Harness::Codex);
        assert_eq!(p.title, "Would you like to run the following command?");
        assert_eq!(
            p.choices[1].label,
            "Yes, and don't ask again for commands that start with `touch probe- file.txt` (p)"
        );
        assert_eq!(
            p.choices
                .iter()
                .map(|c| c.key.as_deref())
                .collect::<Vec<_>>(),
            [Some("y"), Some("p"), Some("Escape")]
        );
        assert_eq!(p.selected, 1);
        assert_eq!(
            p.detail,
            [
                "Environment: local",
                "Reason: May I create probe.txt? The workspace is read-only.",
                "$ touch probe.txt"
            ]
        );
    }

    #[test]
    fn a_moved_cursor_is_reported() {
        let moved = CLAUDE_BASH
            .replace(" ❯ 1. Yes", "   1. Yes")
            .replace("   2. Yes, and", " ❯ 2. Yes, and");
        assert_eq!(prompt(&moved, Harness::Claude).selected, 2);
    }

    #[test]
    fn a_prompt_quoted_in_the_conversation_does_not_count() {
        let quoted = "\
● Claude Code will then ask:

   Do you want to proceed?
   ❯ 1. Yes
     2. No

● Want me to go ahead?

────────────────────────────────────────
❯ \n────────────────────────────────────────
  ? for shortcuts
";
        assert_eq!(permission_prompt(quoted, Harness::Claude), PromptRead::None);
        let codex_quoted = "• Codex asks \"Would you like to run the following command?\" first.\n\
                            › 1. Yes, proceed (y)\n  2. No (esc)\n\n› Ask Codex to do anything\n\n  gpt-6 medium · 0% used\n";
        assert_eq!(
            permission_prompt(codex_quoted, Harness::Codex),
            PromptRead::None
        );
        let scrolled = format!("{CLAUDE_BASH}\nline\nline\nline\nline\nline\n");
        assert_eq!(
            permission_prompt(&scrolled, Harness::Claude),
            PromptRead::None
        );
        assert_eq!(permission_prompt("", Harness::Claude), PromptRead::None);
        assert_eq!(
            permission_prompt("Steps:\n❯ 1. build\n  2. test\n", Harness::Claude),
            PromptRead::None
        );
        // Codex: a live list under no title is an approval only with the approval's answer shape
        // (a shortcut, its footer); then it is not told whole, so not "no prompt" either (a
        // rename typed into it would be its answer).
        assert_eq!(
            permission_prompt("Steps:\n❯ 1. build\n  2. test\n", Harness::Codex),
            PromptRead::None
        );
        for shaped in [
            "Steps:\n❯ 1. build (y)\n  2. test (esc)\n",
            "Steps:\n❯ 1. build\n  2. test\n\n  Press enter to confirm or esc to cancel\n",
        ] {
            let r = permission_prompt(shaped, Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
    }

    fn two() -> Vec<Question> {
        normalize_questions(&json!({ "questions": [
            { "question": "Pick a color", "header": "Color",
              "options": [{ "label": "Red", "description": "The color red" }, { "label": "Blue", "description": "The color blue" }] },
            { "question": "Pick fruits", "header": "Fruits", "multiSelect": true,
              "options": [{ "label": "Apple", "description": "An apple" }, { "label": "Pear", "description": "A pear" }, { "label": "Plum", "description": "A plum" }] }
        ] }))
        .unwrap()
    }

    const RULE: &str = "────────────────────────────────────────────────────────────";

    // Claude Code 2.1's form, as captured (wording synthetic).
    fn color_form() -> String {
        format!(
            "❯ Ask me about colors and fruits\n{RULE}\n←  ☐ Color  ☐ Fruits  ✔ Submit  →\n\nPick a color\n\n\
             ❯ 1. Red\n     The color red\n  2. Blue\n     The color blue\n  3. Type something.\n\
             {RULE}\n  4. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel\n"
        )
    }

    fn fruits_form() -> String {
        format!(
            "{RULE}\n←  ☐ Color  ☐ Fruits  ✔ Submit  →\n\nPick fruits\n\n\
             ❯ 1. [ ] Apple\n         An apple\n  2. [ ] Pear\n         A pear\n  3. [ ] Plum\n         A plum\n\
             \x20 4. [ ] Type something\n     Submit\n{RULE}\n  5. Chat about this\n\n\
             Enter to select · Tab/Arrow keys to navigate · Esc to cancel\n"
        )
    }

    #[test]
    fn the_live_form_is_parsed_whole() {
        let f = parse_form(&color_form()).unwrap();
        assert_eq!(
            f.tabs,
            [("Color".to_string(), false), ("Fruits".to_string(), false)]
        );
        assert_eq!(f.question, "Pick a color");
        assert_eq!(
            f.rows
                .iter()
                .map(|r| (r.n, r.label.as_str(), r.description.as_str()))
                .collect::<Vec<_>>(),
            [
                (Some(1), "Red", "The color red"),
                (Some(2), "Blue", "The color blue"),
                (Some(3), "Type something.", ""),
                (Some(4), "Chat about this", "")
            ]
        );
        assert_eq!(f.cursor(), Some(0));
        let q = two();
        assert_eq!(f.shows(&q), Ok(0));
        assert!(f.untouched(&q[0]));
        assert!(screen_shows_question(&color_form(), &q));

        let m = parse_form(&fruits_form()).unwrap();
        assert_eq!(m.shows(&q), Ok(1));
        assert_eq!(m.rows[4].label, "Submit");
        assert_eq!(m.rows[0].checked, Some(false));
        assert!(m.untouched(&q[1]));
        // The first question's form is the one asked about.
        assert!(!screen_shows_question(&fruits_form(), &q));
    }

    #[test]
    fn a_touched_form_is_not_untouched() {
        let q = two();
        // A box ticked, the cursor back on 1.
        let ticked = fruits_form().replace("[ ] Pear", "[✔] Pear");
        let f = parse_form(&ticked).unwrap();
        assert!(!f.untouched(&q[1]));
        assert_eq!(f.ticked(), [1]);
        // Another tab answered (☒), this one pristine.
        let answered = color_form().replace("☐ Fruits", "☒ Fruits");
        assert!(!parse_form(&answered).unwrap().untouched(&q[0]));
        // Something typed.
        let typed = fruits_form().replace("[ ] Type something", "[✔] kiwi");
        assert!(!parse_form(&typed).unwrap().untouched(&q[1]));
        // The cursor moved.
        let moved = color_form()
            .replace("❯ 1. Red", "  1. Red")
            .replace("  2. Blue", "❯ 2. Blue");
        let f = parse_form(&moved).unwrap();
        assert_eq!(f.cursor(), Some(1));
        assert!(!f.untouched(&q[0]));
    }

    #[test]
    fn a_quoted_question_above_another_live_form_does_not_count() {
        let q = two();
        let other = normalize_questions(&json!({ "questions": [
            { "question": "Pick a color for the logo", "header": "Logo",
              "options": [{ "label": "Red", "description": "The color red" }, { "label": "Blue", "description": "The color blue" }] }
        ] }))
        .unwrap();
        // The question we want, quoted in the conversation; the live form asks another.
        let screen = format!(
            "● Earlier I asked: Pick a color\n❯ 1. Red\n  2. Blue\n\n{}",
            color_form()
                .replace("☐ Color  ☐ Fruits", "☐ Logo")
                .replace("Pick a color\n", "Pick a color for the logo\n")
        );
        assert!(!screen_shows_question(&screen, &q));
        assert!(screen_shows_question(&screen, &other));
        // Two questions sharing a prefix: only the active one's full text matches.
        let shared = normalize_questions(&json!({ "questions": [
            { "question": "Pick a color", "header": "Color",
              "options": [{ "label": "Red", "description": "The color red" }, { "label": "Blue", "description": "The color blue" }] },
            { "question": "Pick a color for the border", "header": "Border",
              "options": [{ "label": "Red", "description": "The color red" }, { "label": "Blue", "description": "The color blue" }] }
        ] }))
        .unwrap();
        let border = color_form()
            .replace("☐ Fruits", "☐ Border")
            .replace("Pick a color\n", "Pick a color for the border\n");
        let f = parse_form(&border).unwrap();
        assert_eq!(f.shows(&shared), Ok(1));
        assert!(!screen_shows_question(&border, &shared));
    }

    #[test]
    fn the_review_screen_needs_submit_focused() {
        assert!(review_submit_focused(
            "Review your answers\n\n● Pick a color\n  → Red\n\nReady to submit your answers?\n\n❯ 1. Submit answers\n  2. Cancel\n"
        ));
        assert!(!review_submit_focused(
            "Ready to submit your answers?\n\n  1. Submit answers\n❯ 2. Cancel\n"
        ));
        assert!(!review_submit_focused(
            "❯ 1. Submit answers\n\n❯ \n? for shortcuts\n"
        ));
    }

    #[test]
    fn duplicate_shortcuts_fall_back_to_the_cursor() {
        let dup = CODEX_EXEC.replace("(p)", "(y)");
        let p = prompt(&dup, Harness::Codex);
        assert_eq!(
            p.choices
                .iter()
                .map(|c| c.key.as_deref())
                .collect::<Vec<_>>(),
            [None, None, Some("Escape")]
        );
    }

    #[test]
    fn a_rule_inside_the_command_makes_the_frame_ambiguous() {
        let inside = CLAUDE_BASH.replace(
            " rm -rf build\n",
            " echo start\n────────────────────────────────────────\n echo end\n",
        );
        assert!(matches!(
            permission_prompt(&inside, Harness::Claude),
            PromptRead::Incomplete(_)
        ));
    }

    // Two heredocs that differ only in indentation: one deletes the file, one does not.
    #[test]
    fn command_indentation_is_part_of_the_prompts_identity() {
        let with = |body: &str| {
            CLAUDE_BASH.replace(
                " rm -rf build\n╌",
                &format!(" python3 - <<'EOF'\n if False:\n{body}\n EOF\n╌"),
            )
        };
        let guarded = prompt(&with("     os.remove('data.db')"), Harness::Claude);
        let deletes = prompt(&with(" os.remove('data.db')"), Harness::Claude);
        assert_ne!(guarded.id, deletes.id);
        // The same prompt read twice is the same prompt.
        let again = prompt(&with("     os.remove('data.db')"), Harness::Claude);
        assert_eq!(guarded.id, again.id);
    }

    // A command drawing a solid rule, then a dashed one: indented, as everything inside the
    // frame is, they are the command's own lines — the frame is still the one at column 0,
    // and what comes before them is part of the prompt's identity.
    #[test]
    fn rules_inside_the_command_never_hide_the_lines_above_them() {
        let with = |first: &str| {
            CLAUDE_BASH.replace(
                " rm -rf build\n",
                &format!(
                    " {first}\n ────────────────────────────────────────\n ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n echo end\n"
                ),
            )
        };
        let a = prompt(&with("rm -rf /w/app"), Harness::Claude);
        let b = prompt(&with("echo start"), Harness::Claude);
        assert_ne!(a.id, b.id);
        assert!(a.detail.contains(&"rm -rf /w/app".to_string()));
        assert!(a.detail.contains(&"Bash command".to_string()));
        // The same lines drawn from column 0 are not the frame's shape (a solid rule right
        // above a dashed pair, no header between): refused, not guessed.
        let flush = CLAUDE_BASH.replace(
            " rm -rf build\n",
            " rm -rf /w/app\n────────────────────────────────────────\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n echo end\n",
        );
        assert!(matches!(
            permission_prompt(&flush, Harness::Claude),
            PromptRead::Incomplete(_)
        ));
    }

    // The frame's rules are as wide as the pane: hashed, they made one prompt two ids at two
    // widths, and a resize between `--list` and `--choice --prompt` refused the answer.
    #[test]
    fn the_frame_width_is_not_part_of_the_prompts_identity() {
        let at = |cols: usize| -> String {
            CLAUDE_BASH
                .lines()
                .map(|l| match l.chars().next() {
                    Some(c @ ('─' | '╌')) if is_rule(l) => c.to_string().repeat(cols),
                    _ => l.to_string(),
                })
                .map(|l| l + "\n")
                .collect()
        };
        let narrow = prompt(&at(60), Harness::Claude);
        assert_eq!(narrow.id, prompt(&at(120), Harness::Claude).id);
        assert_eq!(narrow.id, prompt(CLAUDE_BASH, Harness::Claude).id);
        // A rule-like line of the command (padded) still counts.
        let ruled = CLAUDE_BASH.replace(" rm -rf build\n", " rm -rf build\n ────────────\n");
        assert_ne!(narrow.id, prompt(&ruled, Harness::Claude).id);
    }

    // A box character at one end only is the command's, and so is the indentation before it:
    // only a side frame (a box character at both ends) is dropped.
    #[test]
    fn a_lone_box_character_keeps_its_indentation_in_the_identity() {
        let with = |line: &str| CLAUDE_BASH.replace(" rm -rf build\n", &format!("{line}\n"));
        let deep = prompt(&with("     │ y"), Harness::Claude);
        let shallow = prompt(&with(" │ y"), Harness::Claude);
        assert_ne!(deep.id, shallow.id);
        assert_eq!(literal("│  a │"), "  a");
        assert_eq!(literal("   │ y"), "   │ y");
        // Boxed at both ends but indented: a line of the command, its indentation kept.
        assert_eq!(literal("  ╭────╮"), "  ╭────╮");
        let deep = prompt(&with("     │ y │"), Harness::Claude);
        let shallow = prompt(&with(" │ y │"), Harness::Claude);
        assert_ne!(deep.id, shallow.id);
    }

    // A command line boxed at both ends (`╭────╮`, `│x│`) once lost its indentation and read as
    // drawn from column 0: it forged the frame's top and the dashed pair, the real header and
    // the head of the command left the id, and `rm -rf` and `echo` got one id.
    #[test]
    fn a_boxed_command_line_never_reads_as_the_claude_frame() {
        let with = |first: &str| {
            CLAUDE_BASH.replace(
                " rm -rf build\n",
                &format!(
                    " {first}\n ╭────────────────╮\n Bash command\n ╭╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╮\n echo hi\n"
                ),
            )
        };
        let a = prompt(&with("rm -rf /w/app"), Harness::Claude);
        let b = prompt(&with("echo start"), Harness::Claude);
        assert_ne!(a.id, b.id);
        assert!(a.detail.contains(&"rm -rf /w/app".to_string()), "{a:?}");
        assert!(
            a.detail.contains(&"Remove the build output".to_string()),
            "{a:?}"
        );
    }

    // The same for Codex: a boxed command line once read as the last conversation item, and
    // the prompt's top moved under it.
    #[test]
    fn a_boxed_command_line_never_reads_as_the_codex_conversation() {
        let with = |first: &str, after: &str| {
            CODEX_EXEC.replace(
                "  $ touch probe.txt\n",
                &format!("  $ bash -lc '{first}\n  │x│\n  {after}\n  ls'\n"),
            )
        };
        for first in ["rm -rf /w/app", "echo hi"] {
            let r = permission_prompt(&with(first, "Allow listing"), Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
        let a = prompt(&with("rm -rf /w/app", "echo really?"), Harness::Codex);
        let b = prompt(&with("echo hi", "echo really?"), Harness::Codex);
        assert_eq!(a.title, "Would you like to run the following command?");
        assert_ne!(a.id, b.id);
        assert!(
            a.detail.iter().any(|d| d.contains("rm -rf /w/app")),
            "{a:?}"
        );
    }

    // Codex: the last conversation item out of reach above a tall command. The prompt's top
    // cannot be found, so it cannot be told whole: refused, never read from mid-command.
    #[test]
    fn a_codex_prompt_whose_top_is_out_of_reach_is_incomplete() {
        let tall = |n: usize| {
            let body: String = (0..n).map(|i| format!("  echo part {i}\n")).collect();
            CODEX_EXEC.replace(
                "  $ touch probe.txt\n",
                &format!("  $ bash -lc '\n{body}  echo really?\n  ls'\n"),
            )
        };
        let r = permission_prompt(&tall(MAX_CODEX_PROMPT_LINES + 10), Harness::Codex);
        assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        // Within reach, the same prompt is read whole.
        let p = prompt(&tall(10), Harness::Codex);
        assert_eq!(p.title, "Would you like to run the following command?");
    }

    /// [`CODEX_EXEC`] running a command that starts with `first` and has a line ending in "?".
    fn codex_asking(first: &str) -> String {
        CODEX_EXEC.replace(
            "  $ touch probe.txt\n",
            &format!("  $ bash -lc '{first}\n  echo really?\n  echo same\n  echo part 2\n  ls'\n"),
        )
    }

    // Codex, the capture holding no line at column 0 (a fresh or cleared pane, the prompt's head
    // clipped off): read from its first line, a command line ending in "?" became the title, and
    // two commands differing only in the clipped head got one id. Refused, like a top out of reach.
    #[test]
    fn a_codex_prompt_with_no_column_0_line_captured_is_incomplete() {
        let clipped = |first: &str| {
            let full = codex_asking(first);
            full[full.find("  echo really?").unwrap()..].to_string()
        };
        for first in ["rm -rf /w/app", "echo hi"] {
            let r = permission_prompt(&clipped(first), Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
    }

    // The top in sight (a column-0 line above), but no line under it reads as a title. A command
    // line ending in "?" was once taken for one ("echo really?": the command's head and the real
    // title cut off); with no such line it was no prompt at all, so a rename was typed into it
    // and the post-answer check took it for gone. Either way: not told whole.
    #[test]
    fn a_codex_prompt_under_its_top_but_without_a_title_is_incomplete() {
        let untitled = |first: &str| {
            codex_asking(first).replace("  Would you like to run the following command?\n", "")
        };
        for first in ["rm -rf /w/app", "echo hi"] {
            let asks = untitled(first);
            assert!(asks.contains("\n• Running") && asks.contains("\n  echo really?\n"));
            let r = permission_prompt(&asks, Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
            let plain = asks.replace("echo really?", "echo really");
            let r = permission_prompt(&plain, Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
    }

    // The reach, exactly: the last conversation item 60 lines above the first answer is in it,
    // 61 lines above is not.
    #[test]
    fn a_codex_prompt_is_read_from_at_most_60_lines_above_its_answers() {
        assert_eq!(MAX_CODEX_PROMPT_LINES, 60, "the docs say 60");
        let tall = |n: usize| {
            let body: String = (0..n).map(|i| format!("  echo part {i}\n")).collect();
            CODEX_EXEC.replace(
                "  $ touch probe.txt\n",
                &format!("  $ bash -lc '\n{body}  ls'\n"),
            )
        };
        let up = |screen: &str| {
            let lines: Vec<&str> = screen.lines().collect();
            let item = lines.iter().position(|l| l.starts_with("• ")).unwrap();
            let first = lines.iter().position(|l| l.starts_with("› 1. ")).unwrap();
            first - item
        };
        let n = (0..100).find(|&n| up(&tall(n)) == 60).unwrap();
        let p = prompt(&tall(n), Harness::Codex);
        assert_eq!(p.title, "Would you like to run the following command?");
        assert!(p.detail.iter().any(|d| d == "echo part 0"), "{p:?}");
        assert_eq!(up(&tall(n + 1)), 61);
        let r = permission_prompt(&tall(n + 1), Harness::Codex);
        assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
    }

    // With the last conversation item captured, even as the first line, the prompt is read whole.
    #[test]
    fn a_codex_prompt_under_a_column_0_line_is_read_whole() {
        let full = codex_asking("rm -rf /w/app");
        let from_item = &full[full.find("• Running").unwrap()..];
        let p = prompt(from_item, Harness::Codex);
        assert_eq!(p.title, "Would you like to run the following command?");
        assert!(
            p.detail.iter().any(|d| d.contains("rm -rf /w/app")),
            "{p:?}"
        );
        assert_eq!(p.id, prompt(&full, Harness::Codex).id);
        assert_ne!(p.id, prompt(&codex_asking("echo hi"), Harness::Codex).id);
    }

    /// `screen` without the approval's answer shape: no shortcut in an answer, no footer.
    fn unshaped(screen: &str) -> String {
        let s = [" (y)", " (p)", " (esc)"]
            .iter()
            .fold(screen.to_string(), |s, k| s.replace(k, ""));
        s.replace("  Press enter to confirm or esc to cancel\n", "")
            .replace("  Press enter to confirm or esc to\n  cancel\n", "")
    }

    // The title in sight, but no line at column 0 captured above it (a fresh or cleared pane):
    // what stood above the title is unknown, so the prompt is not told whole — read from its
    // first line, it would be. With or without the answer shape: the title makes it an approval.
    #[test]
    fn a_codex_title_in_sight_with_no_column_0_line_captured_is_incomplete() {
        let full = codex_asking("rm -rf /w/app");
        let from_title = &full[full.find("  Would you like").unwrap()..];
        for screen in [from_title.to_string(), unshaped(from_title)] {
            let r = permission_prompt(&screen, Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}");
        }
        assert_eq!(
            prompt(&full, Harness::Codex).title,
            "Would you like to run the following command?"
        );
    }

    const CODEX_40: &str = include_str!("../../../../testdata/codex/exec-approval-pane-40.txt");

    /// The recorded 40-column approval, its "don't ask again" answer naming a longer prefix: it
    /// wraps onto 4 lines under the answer.
    fn codex_40_long_prefix() -> String {
        let s = CODEX_40.replace(
            "     commands that start with `touch\n     approval-probe.txt` (p)\n",
            "     commands that start with `npm run\n     build --workspace packages/web\n     \
             --if-present --include-workspace-\n     root` (p)\n",
        );
        assert_ne!(s, CODEX_40);
        s
    }

    // Codex wraps a long answer onto as many lines as it takes. Capped at 3 like Claude Code's,
    // a 4th made the walk give up: a live approval read as no prompt, and after a key as
    // answered while it was still on screen.
    #[test]
    fn a_codex_answer_wrapped_onto_more_than_3_lines_is_read_whole() {
        let p = prompt(&codex_40_long_prefix(), Harness::Codex);
        assert_eq!(p.title, "Would you like to run the following");
        assert_eq!(
            p.choices[1].label,
            "Yes, and don't ask again for commands that start with `npm run build --workspace \
             packages/web --if-present --include-workspace- root` (p)"
        );
        assert_eq!(
            p.choices
                .iter()
                .map(|c| c.key.as_deref())
                .collect::<Vec<_>>(),
            [Some("y"), Some("p"), None]
        );
        assert_ne!(p.id, prompt(CODEX_40, Harness::Codex).id);
        // Claude Code's labels keep the cap: its prompts read as before.
        assert_eq!(
            permission_prompt(&codex_40_long_prefix(), Harness::Claude),
            PromptRead::None
        );
    }

    // Once a live list has the approval's answer shape, answers the walk cannot read are an
    // approval not told whole — never "no prompt". Without the shape, the same list is none.
    #[test]
    fn a_codex_approval_whose_answers_cannot_be_read_is_incomplete() {
        let screen = codex_40_long_prefix();
        let broken = [
            // The capture starts inside the answers: answer 1 is off it.
            screen[screen.find("  2. Yes, and").unwrap()..].to_string(),
            // A wrapped line that reads as a rule.
            screen.replace(
                "     build --workspace packages/web\n",
                "     ------------\n",
            ),
            // A wrapped line that reads as another answer.
            screen.replace("     build --workspace", "     12. build --workspace"),
            // A blank line inside the answers.
            screen.replace("     root` (p)\n", "\n     root` (p)\n"),
        ];
        for s in &broken {
            let r = permission_prompt(s, Harness::Codex);
            assert!(matches!(r, PromptRead::Incomplete(_)), "{r:?}\n{s}");
            let bare = unshaped(s);
            assert!(!bare.contains("(y)") && !bare.contains("esc to"), "{bare}");
            assert_eq!(permission_prompt(&bare, Harness::Codex), PromptRead::None);
        }
    }

    // Screens ending in a numbered list with the cursor on it that are no prompt: Codex's
    // popups (/model, /approvals, /review) and transcript pager, a numbered draft in its
    // composer, one in Claude Code's input box under a line ending in "?", pi's editor under a
    // tool's output quoting an approval's answers. Read as an incomplete approval, they made
    // `--list` report one and every answer after which one came back `outcome_uncertain`.
    // Synthetic layouts.
    const NOT_PROMPTS: [(&str, &str); 8] = [
        (
            "/model popup",
            "\
› Fix the flaky tests

• I pinned the seed in parser.rs; the suite passes.

  Select Model and Effort
  Access legacy models by running codex -m <model_name> or in your config.toml

› 1. gpt-5-codex (current)  Optimized for coding tasks with many tools.
  2. gpt-5                  Broad world knowledge with strong general reasoning.

  Press enter to confirm or esc to go back
",
        ),
        (
            "/approvals popup",
            "\
• I pinned the seed in parser.rs; the suite passes.

  Select Approval Mode

› 1. Read Only (current)  Codex can read files and answer questions.
  2. Auto                 Codex can read, edit and run commands in the workspace.
  3. Full Access          Codex can edit and run commands with network access.

  Press enter to confirm or esc to go back
",
        ),
        (
            "/review popup",
            "\
• I pinned the seed in parser.rs; the suite passes.

  Select a review preset

› 1. Review against a base branch  (PR Style)
  2. Review uncommitted changes
  3. Review a commit
  4. Custom review instructions

  Press enter to confirm or esc to go back
",
        ),
        (
            "Ctrl+T transcript pager",
            "\
/ T R A N S C R I P T / / / / / / / / / / / / / / / / / / / / / / / / / / / /
• I pinned the seed in parser.rs; the suite passes.

› 1. Run the full suite again
  2. Then open a PR

───────────────────────────────────────────────────────────────── 100% ─
  ↑/↓ to scroll   pgup/pgdn to page   home/end to jump
  q to quit   esc to edit prev
",
        ),
        (
            "Codex composer, numbered draft",
            "\
› Fix the flaky tests

• I pinned the seed in parser.rs; the suite passes.

› 1. Run the full suite again
  2. Then open a PR

  ? for shortcuts                                           92% context left
",
        ),
        (
            "Codex composer after an approval, numbered draft",
            "\
✔ You approved codex to run touch approval-probe.txt this time

• Working (2s • esc to interrupt)

› 1. Run the full suite again (y)
  2. Then open a PR (esc)

  ⏎ send   ⌃J newline   ⌃T transcript   ⌃C quit
",
        ),
        (
            "Claude Code input box, numbered draft under a question",
            "\
❯ Clean the build output
⏺ Should I also clear the cache?
⏺ Bash(rm -rf build)
  ⎿  (No content)
⏺ Removed build/.

────────────────────────────────────────────────────────────────────────────────
❯ 1. clear the cache too
  2. then rebuild
────────────────────────────────────────────────────────────────────────────────
  ? for shortcuts
",
        ),
        (
            "pi editor under a tool's output",
            concat!(
                " $ fleet answer worker-3 --list | sed -n 2,5p\n",
                "   $ rm -rf build\n",
                " ❯ 1. Yes, proceed (y)\n",
                "   2. No, and tell Codex what to do differently (esc)\n",
                "\n",
                "────────────────────────────────────────────────────────────────────────────────\n",
                "\n",
                "────────────────────────────────────────────────────────────────────────────────\n",
                "~/code/app (main)\n",
                "↑1.2k ↓300 R12k $0.012 3.1%/200k\n",
            ),
        ),
    ];

    #[test]
    fn popups_pagers_and_drafts_are_no_prompt() {
        use crate::core::title::shows_prompt;
        for (what, screen) in NOT_PROMPTS {
            for h in [Harness::Codex, Harness::Pi, Harness::Claude] {
                assert_eq!(
                    permission_prompt(screen, h),
                    PromptRead::None,
                    "{what}, {h:?}"
                );
                assert!(!shows_prompt(screen, h), "{what}, {h:?}");
            }
        }
        // A real approval with the answer shape and no title stays incomplete: the recorded
        // 40-column one without its title, and the /approvals popup were its footer the approval's.
        let untitled = CODEX_40.replace("  Would you like to run the following\n  command?\n", "");
        assert_ne!(untitled, CODEX_40);
        let approval_footer = NOT_PROMPTS[1].1.replace("esc to go back", "esc to cancel");
        for s in [&untitled, &approval_footer] {
            for h in [Harness::Codex, Harness::Pi] {
                let r = permission_prompt(s, h);
                assert!(matches!(r, PromptRead::Incomplete(_)), "{h:?}: {r:?}");
                assert!(shows_prompt(s, h));
            }
        }
    }

    // Claude Code 2.1's review screen, as captured (wording synthetic).
    const REVIEW: &str = "\
❯ Ask me about colors and fruits
────────────────────────────────────────────────────────────
←  ☒ Color  ☒ Fruits  ✔ Submit  →
Review your answers
 ● Pick a color
   → Blue
 ● Pick fruits
   → Apple, kiwi  fruit
Ready to submit your answers?
❯ 1. Submit answers
  2. Cancel
";

    #[test]
    fn the_review_screen_is_parsed_whole() {
        let r = parse_review(REVIEW).unwrap();
        assert_eq!(
            r.tabs,
            [("Color".to_string(), true), ("Fruits".to_string(), true)]
        );
        assert_eq!(
            r.items,
            [
                ("Pick a color".to_string(), "Blue".to_string()),
                ("Pick fruits".to_string(), "Apple, kiwi  fruit".to_string())
            ]
        );
        assert!(r.submit_focused);
        let cancel = REVIEW
            .replace("❯ 1. Submit", "  1. Submit")
            .replace("  2. Cancel", "❯ 2. Cancel");
        assert!(!parse_review(&cancel).unwrap().submit_focused);
        assert!(review_submit_focused(REVIEW));
        // Under the composer: not the live screen.
        assert_eq!(
            parse_review(&format!("{REVIEW}\n❯ \n? for shortcuts\n")),
            None
        );
    }

    #[test]
    fn a_prompt_left_in_the_scrollback_is_not_live() {
        // Cleared off the screen into the scrollback; the screen now shows something else.
        let screen = format!("{CLAUDE_BASH}{VISIBLE_MARK}\n● done\n\n\n\n");
        assert_eq!(
            permission_prompt(&screen, Harness::Claude),
            PromptRead::None
        );
        // On screen, its frame in the scrollback above: live, read whole.
        let (above, below) = CLAUDE_BASH.split_at(CLAUDE_BASH.find(" Do you want").unwrap());
        let split = format!("{above}{VISIBLE_MARK}\n{below}");
        assert_eq!(
            prompt(&split, Harness::Claude).detail,
            ["Bash command", "Remove the build output", "rm -rf build"]
        );
    }
}
