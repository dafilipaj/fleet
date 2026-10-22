//! What `fleet model` and `fleet effort` share: Claude Code's slash commands that switch a
//! setting of a session — `/model <id>`, `/effort <level>` — read off its screen after the
//! command is typed, and off what it then writes into the session's transcript.
//!
//! **The screen.** Such a command either switches at once, or — while the prompt cache is
//! warm — first asks to confirm, in place of the input box, under a `▔` rule:
//!
//! ```text
//! ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔ ◐ medium · /effort ▔
//!    Change effort level?
//!    Your next response will be slower and use more tokens
//!
//!    This conversation is cached for the current effort level. Switching to xhigh means the
//!    full history gets re-read on your next message.
//!
//!    ❯ 1. Yes, switch to xhigh
//!      2. No, go back
//! ```
//!
//! [`shown`] tells that confirmation — exactly that one, for that value — from the input box
//! and from anything else. It reads the dialog with [`prompt::permission_prompt`], the reader
//! `fleet answer` uses (which reads it as an answerable prompt), and only adds what makes it
//! this one ([`Dialog`]): its title and choices, the warm-cache reason, nothing drawn under it.
//!
//! **The transcript.** Claude Code writes the command's output wrapped in
//! `<local-command-stdout>…</local-command-stdout>`, either as a `type: "user"` entry (its
//! `message.content`) right under the entry naming the command, or as a `type: "system"`,
//! `subtype: "local_command"` entry (its `content`). [`Answers`] reads a transcript's new lines
//! for the outputs of one command — a `user` entry only as the output of the entry naming that
//! very command before it, since a prompt typed as exactly that output is written the same —
//! and [`Tail`] gives those new lines.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::core::backend::strip_ansi;
use crate::core::harness::Harness;
use crate::core::input::{self, InputRead};
use crate::core::prompt::{self, PromptRead};

/// What the screen shows after the command and Enter, where the input box was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// The input box: the command not taken yet, or taken and answered above it.
    Box,
    /// Claude Code's warm-cache confirmation for that very switch; `key` is what its first
    /// ("yes") choice is picked with (its number, `1`).
    Confirm { key: String },
    /// Work still under way where the box was: its last line words ending in `…` (before any
    /// ` (…)`), as Claude Code's "Running PreModelSwitch hooks… (esc to cancel)" while the
    /// hooks run; or one of Claude Code's passing hint banners ([`banner`]) drawn there while
    /// the box is redrawn. What comes once it is done decides.
    Pending,
    /// Anything else: another dialog or menu, a confirmation for another value or in other
    /// words (a hook asking), a layout fleet does not know.
    Other,
}

/// What makes a confirmation the one a command asks: `/model`'s is "Switch model?", cached
/// "for the current model"; `/effort`'s "Change effort level?", cached "for the current effort
/// level".
#[derive(Debug, Clone, Copy)]
pub struct Dialog {
    /// Its title, the line under its `▔` top.
    pub title: &'static str,
    /// The words saying the prompt cache is why it asks (`cached for the current model`).
    pub cached_for: &'static str,
}

/// What `screen` (a capture of a Claude Code session, with its escape sequences when `styled`)
/// shows after a command whose confirmation is `dialog` was typed. See [`Shown`]. The
/// confirmation counts only when the value its first choice names (`Yes, switch to <label>`)
/// is the one asked for, by `asked`.
pub fn shown(screen: &str, styled: bool, dialog: Dialog, asked: impl Fn(&str) -> bool) -> Shown {
    if matches!(
        input::read(screen, Harness::Claude, styled),
        InputRead::Empty | InputRead::Typed(_) | InputRead::Unclear(_)
    ) {
        return Shown::Box;
    }
    match confirmation(screen, dialog) {
        Some(c) if c.warm && asked(&c.label) => Shown::Confirm { key: c.key },
        Some(_) => Shown::Other,
        None if pending(screen) => Shown::Pending,
        None => Shown::Other,
    }
}

/// Does `screen` end in a line of work under way — words ending in `…`, a ` (…)` after them
/// aside ("Running PreModelSwitch hooks… (esc to cancel)") — or in a [`banner`]?
fn pending(screen: &str) -> bool {
    let lines = lines(screen);
    let Some(last) = lines.iter().rev().map(|l| l.trim()).find(|l| !l.is_empty()) else {
        return false;
    };
    if banner(last) {
        return true;
    }
    let words = last.split(" (").next().unwrap_or(last).trim_end();
    words
        .strip_suffix('…')
        .is_some_and(|w| !w.trim().is_empty())
}

/// One of the hints Claude Code shows for a moment on the line above its input box, about the
/// terminal it runs in ("tmux detected · …", "tmux focus-events off · add 'set -g
/// focus-events on' to ~/.tmux.conf …"). Caught alone at the bottom of the screen, while the
/// box under it is redrawn, it is no dialog.
fn banner(line: &str) -> bool {
    ["tmux detected", "tmux focus-events off"]
        .iter()
        .any(|b| line.trim().starts_with(b))
}

/// The screen's lines as plain text, trailing blanks cut.
pub(crate) fn lines(screen: &str) -> Vec<String> {
    screen
        .lines()
        .map(|l| strip_ansi(l).trim_end().to_string())
        .collect()
}

/// A command's confirmation, read off the bottom of the screen.
struct Confirmation {
    /// Its first choice's number: the key that picks it.
    key: String,
    /// What its first choice switches to (`Yes, switch to <label>`).
    label: String,
    /// It asks because the prompt cache is warm ([`Dialog::cached_for`]), not for a hook.
    warm: bool,
}

/// The confirmation `dialog` when it is what the screen ends with, as `fleet answer` reads it
/// ([`prompt::permission_prompt`]): titled so, exactly two choices — `1. Yes, switch to
/// <label>` and `2. No, go back` — and nothing drawn under them. `None` for anything else,
/// another command's confirmation included.
fn confirmation(screen: &str, dialog: Dialog) -> Option<Confirmation> {
    let plain = lines(screen).join("\n");
    let PromptRead::Prompt(p) = prompt::permission_prompt(&plain, Harness::Claude) else {
        return None;
    };
    if p.title != dialog.title {
        return None;
    }
    let [yes, no] = &p.choices[..] else {
        return None;
    };
    let label = yes
        .label
        .strip_prefix("Yes, switch to ")?
        .trim()
        .to_string();
    if no.label != "No, go back" || label.is_empty() {
        return None;
    }
    // The reader allows a short footer under the answers (a permission prompt's "Esc to
    // cancel"); this dialog has none, so one there means another dialog.
    let last = plain.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let last = last.trim_start_matches(['❯', '›', '>']).trim_start();
    if last != format!("{}. {}", no.n, no.label) {
        return None;
    }
    let text = p.detail.join(" ");
    Some(Confirmation {
        key: yes.key.clone()?,
        label,
        warm: text.contains(dialog.cached_for) && !hooked(&text),
    })
}

/// Do a confirmation's words say a hook asks, not the warm cache: "hook" in any case, or the
/// name of the hook event that asks. Claude Code 2.1.308 draws `/model`'s and `/effort`'s
/// confirmations from one dialog, which a `PreModelSwitch` hook can retitle "A PreModelSwitch
/// hook asked you to confirm"; the event is named here on its own too, so other words that
/// name it are not read as the warm cache's.
fn hooked(text: &str) -> bool {
    text.to_lowercase().contains("hook") || text.contains("PreModelSwitch")
}

/// The output of a local command transcript line `line` records, with its `timestamp` (as
/// written) and whether it is a `user` entry: the text wrapped in `<local-command-stdout>` (or
/// `-stderr`), escape sequences dropped and trimmed, in either of the two entries Claude Code
/// writes it as —
///
/// - `type: "system"`, `subtype: "local_command"`, the output its `content`;
/// - `type: "user"`, not `isMeta` and not a sidechain's, the output its `message.content`, a
///   string that is that output and nothing else. A prompt typed as exactly that is written
///   so too: [`Answers`] takes one only as its command's answer.
///
/// `None` for any other line — the entry naming the command itself
/// (`<command-name>/…</command-name>…`), a prompt that only quotes such output, a tool's
/// result — included.
pub(crate) fn output(line: &str) -> Option<(String, String, bool)> {
    if !line.contains("local-command-std") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let (content, user) = match v["type"].as_str()? {
        "system" if v["subtype"] == "local_command" => (v["content"].as_str()?, false),
        "user" if v["isMeta"] != true && v["isSidechain"] != true => {
            let c = v["message"]["content"].as_str()?.trim();
            let whole = ["stdout", "stderr"].iter().any(|s| {
                c.starts_with(&format!("<local-command-{s}>"))
                    && c.ends_with(&format!("</local-command-{s}>"))
            });
            if !whole {
                return None;
            }
            (c, true)
        }
        _ => return None,
    };
    let at = v["timestamp"].as_str()?.to_string();
    let content = strip_ansi(content);
    let out = ["stdout", "stderr"].iter().find_map(|s| {
        let open = format!("<local-command-{s}>");
        let close = format!("</local-command-{s}>");
        let start = content.find(&open)? + open.len();
        let end = content[start..].find(&close)? + start;
        Some(content[start..end].trim().to_string())
    })?;
    Some((out, at, user))
}

/// The outputs of one command (`/<name> <args>`) that a transcript's new lines hold, read in
/// the order they were written: [`output`]'s, except that a `user` entry counts only as the
/// output of that very command. A prompt typed as exactly `<local-command-stdout>…
/// </local-command-stdout>` is written as such an entry too, so one counts only when it answers
/// an entry naming `/<name> <args>` (`<command-name>/<name></command-name>`,
/// `<command-args><args></command-args>`, either shape, not meta, not a sidechain's) read
/// before it: its `parentUuid` that entry's `uuid`, or its `promptId` that entry's — Claude
/// Code writes the command, then its output, under one `promptId`, each the parent of the
/// next. A `system` entry, which no prompt is written as, is read as [`output`] reads it.
pub struct Answers {
    name: String,
    args: String,
    /// The `uuid` and `promptId` of each entry naming the command read so far.
    commands: Vec<(Option<String>, Option<String>)>,
}

/// One output [`Answers`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The output's text, inside `<local-command-stdout>`.
    pub text: String,
    /// The entry's `timestamp`, as written.
    pub at: String,
    /// It is tied to the entry naming the command (a `user` entry under it), not merely a
    /// `system` entry with some command's output.
    pub tied: bool,
}

impl Answers {
    /// For the command `/<name> <args>` (`name` without its `/`).
    pub fn to(name: &str, args: &str) -> Self {
        Answers {
            name: name.trim_start_matches('/').to_string(),
            args: args.trim().to_string(),
            commands: Vec::new(),
        }
    }

    /// The output transcript line `line`, the next one written, records (see [`Answers`]).
    pub fn read(&mut self, line: &str) -> Option<Answer> {
        if line.contains(&format!("<command-name>/{}</command-name>", self.name)) {
            if let Some(ids) = self.command_entry(line) {
                self.commands.push(ids);
            }
            return None;
        }
        let (text, at, user) = output(line)?;
        if !user {
            return Some(Answer {
                text,
                at,
                tied: false,
            });
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let (parent, prompt) = (v["parentUuid"].as_str(), v["promptId"].as_str());
        self.commands
            .iter()
            .any(|(uuid, pid)| {
                (parent.is_some() && uuid.as_deref() == parent)
                    || (prompt.is_some() && pid.as_deref() == prompt)
            })
            .then_some(Answer {
                text,
                at,
                tied: true,
            })
    }

    /// The `uuid` and `promptId` of transcript line `line` when it is an entry naming the
    /// command: `<command-name>/<name></command-name>`, its `<command-args>` the args — a
    /// `type: "user"` entry's `message.content` (not meta, not a sidechain's) or a `type:
    /// "system"`, `subtype: "local_command"` entry's `content`.
    fn command_entry(&self, line: &str) -> Option<(Option<String>, Option<String>)> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let content = match v["type"].as_str()? {
            "system" if v["subtype"] == "local_command" => v["content"].as_str()?,
            "user" if v["isMeta"] != true && v["isSidechain"] != true => {
                v["message"]["content"].as_str()?
            }
            _ => return None,
        };
        let content = content.trim();
        if !content.starts_with(&format!("<command-name>/{}</command-name>", self.name)) {
            return None;
        }
        let open = "<command-args>";
        let start = content.find(open)? + open.len();
        let end = content[start..].find("</command-args>")? + start;
        if content[start..end].trim() != self.args {
            return None;
        }
        let field = |k: &str| v[k].as_str().map(str::to_string);
        Some((field("uuid"), field("promptId")))
    }
}

/// Is transcript time `at` (RFC 3339) no earlier than `since`, to the millisecond (a
/// transcript's precision)? An unreadable time is not.
pub fn at_or_after(at: &str, since: chrono::DateTime<chrono::Utc>) -> bool {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(at.trim()) else {
        return false;
    };
    t.timestamp_millis() >= since.timestamp_millis()
}

/// What a session's transcript gains from now on: the complete lines appended after the point
/// it was at when made. The file is found again on every read (a session's first entries can
/// create it); one that shrank is read again from its start.
pub struct Tail {
    find: Box<dyn Fn() -> Option<PathBuf>>,
    path: Option<PathBuf>,
    pos: u64,
}

impl Tail {
    /// Its end now, `find` saying where the transcript is.
    pub fn from_end(find: impl Fn() -> Option<PathBuf> + 'static) -> Self {
        let path = find();
        let pos = path.as_deref().map_or(0, len);
        Tail {
            find: Box::new(find),
            path,
            pos,
        }
    }

    /// The complete lines written since the last read.
    pub fn read(&mut self) -> Vec<String> {
        if let Some(p) = (self.find)()
            && self.path.as_deref() != Some(p.as_path())
        {
            self.path = Some(p);
            self.pos = 0;
        }
        let Some(path) = &self.path else {
            return Vec::new();
        };
        if len(path) < self.pos {
            self.pos = 0;
        }
        let Ok(mut f) = std::fs::File::open(path) else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(self.pos)).is_err() || f.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        // A line still being written waits for its newline.
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        self.pos += end as u64 + 1;
        String::from_utf8_lossy(&buf[..end])
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANY: Dialog = Dialog {
        title: "Switch model?",
        cached_for: "cached for the current model",
    };

    // Claude Code's hint banners, caught alone where the box was while it is redrawn: pending,
    // not another screen. The same words anywhere but last, or other words, are not.
    #[test]
    fn a_hint_banner_alone_where_the_box_was_is_pending() {
        for screen in [
            "> Say hello.\n\n* Hello.\n\n                    tmux detected · some advice\n\n",
            "\x1b[2m       tmux focus-events off · add 'set -g focus-events on' to ~/.tmux.conf and reattach for focus tracking\x1b[0m\n",
        ] {
            assert_eq!(
                shown(screen, true, ANY, |_| true),
                Shown::Pending,
                "{screen}"
            );
        }
        for screen in [
            "tmux detected · some advice\nSomething else entirely",
            "tmux is detected",
            "the tmux focus-events off hint",
        ] {
            assert_eq!(shown(screen, true, ANY, |_| true), Shown::Other, "{screen}");
        }
    }

    #[test]
    fn only_a_reply_written_after_the_command_began_counts() {
        let began = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:07:03.300Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(at_or_after("2026-01-02T03:07:03.300Z", began));
        assert!(at_or_after("2026-01-02T04:07:03.300+01:00", began));
        assert!(!at_or_after("2026-01-02T03:07:03.299Z", began));
        assert!(!at_or_after("soon", began));
        // Sub-millisecond: the transcript cannot be later than the millisecond it began in.
        let mid = began + chrono::Duration::microseconds(600);
        assert!(at_or_after("2026-01-02T03:07:03.300Z", mid));
    }

    #[test]
    fn the_tail_reads_only_whole_lines_appended_since() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.jsonl");
        std::fs::write(&p, "old\n").unwrap();
        let at = p.clone();
        let mut t = Tail::from_end(move || at.is_file().then(|| at.clone()));
        assert!(t.read().is_empty());
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"one\ntw").unwrap();
        assert_eq!(t.read(), ["one"]);
        f.write_all(b"o\n").unwrap();
        assert_eq!(t.read(), ["two"]);
        // Rewritten shorter: read again from its start.
        std::fs::write(&p, "new\n").unwrap();
        assert_eq!(t.read(), ["new"]);
        // Not there at first: read whole once it appears.
        let q = d.path().join("later.jsonl");
        let at = q.clone();
        let mut t = Tail::from_end(move || at.is_file().then(|| at.clone()));
        assert!(t.read().is_empty());
        std::fs::write(&q, "first\n").unwrap();
        assert_eq!(t.read(), ["first"]);
    }
}
