//! Claude Code's `/effort <level>`, read for `fleet effort`: what its screen shows after the
//! command is typed, and what it writes into the session's transcript. The shapes are
//! `/model`'s ([`slash`]), read by the same readers.
//!
//! **The screen.** `/effort <level>` either switches at once, or — while the prompt cache is
//! warm — first asks to confirm, in place of the input box (Claude Code 2.1.308; the hint at
//! the end of the `▔` top is the one drawn above the box, `◐ <level> · /effort`):
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
//! [`shown`] tells that confirmation — for that very level — from the input box and from
//! anything else ([`slash::shown`]).
//!
//! **The transcript.** Every answer is a `type: "user"` entry wrapping the output in
//! `<local-command-stdout>`, right under the `type: "user"` entry naming the command
//! (`<command-name>/effort</command-name>`, `<command-args><level></command-args>`): its
//! `parentUuid` that entry's `uuid`, the same `promptId`. 2.1.308 writes:
//!
//! - `Set effort level to <level> (saved as your default for new sessions): <what it does>` —
//!   switched, and the default for new sessions on this model;
//! - `Set effort level to max (this session only): …` — switched, nothing saved;
//! - `Effort level set to auto` — the model's saved level cleared: it runs at its default
//!   level, and so do new sessions on it;
//! - `Kept effort level as <level>` — the confirmation declined, the level unchanged.
//!
//! [`Replies`] reads the new lines for them, tied to the command entry before them as
//! `/model`'s are ([`slash::Answers`]).

use crate::core::slash::{self, Answers, Dialog, Shown};

/// `/effort`'s confirmation.
const DIALOG: Dialog = Dialog {
    title: "Change effort level?",
    cached_for: "cached for the current effort level",
};

/// The level Claude Code clears a model's saved level with, running it at its default.
pub const AUTO: &str = "auto";

/// What `screen` (a capture of a Claude Code session, with its escape sequences when `styled`)
/// shows after `/effort <level>` was typed. See [`Shown`]: its confirmation counts only when
/// its first choice is `Yes, switch to <level>`, that very word.
pub fn shown(screen: &str, level: &str, styled: bool) -> Shown {
    slash::shown(screen, styled, DIALOG, |label| label == level.trim())
}

/// What Claude Code answered a `/effort` with, in its transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `Set effort level to <level> (…)`, or `Effort level set to auto`: switched to `level`.
    /// `saved`: Claude Code's default for new sessions on this model changed with it — `(saved
    /// as your default for new sessions)`, or `auto`, which clears the saved level; not `(this
    /// session only)`, as `max` is.
    Set { level: String, saved: bool },
    /// `Kept effort level as <level>`: the confirmation was declined; the level is unchanged.
    Kept(Option<String>),
    /// An answer under the command in words fleet does not know (a level Claude Code does not
    /// take, a newer wording): what it said.
    Other(String),
}

/// The replies to one `/effort <level>` that a transcript's new lines hold, read in the order
/// they were written: [`slash::Answers`]' outputs, read as [`said`] reads them — an output in
/// other words only when it is tied to the command (a `system` entry could be any command's).
pub struct Replies(Answers);

impl Replies {
    /// For the command `/effort <level>`.
    pub fn to(level: &str) -> Self {
        Replies(Answers::to("effort", level))
    }

    /// The reply transcript line `line`, the next one written, records (see [`Replies`]).
    pub fn read(&mut self, line: &str) -> Option<(Reply, String)> {
        let a = self.0.read(line)?;
        match said(&a.text) {
            Reply::Other(_) if !a.tied => None,
            r => Some((r, a.at)),
        }
    }
}

/// What the output of an `/effort` says.
fn said(out: &str) -> Reply {
    if let Some(rest) = out.strip_prefix("Set effort level to ") {
        let level = word(rest);
        if !level.is_empty() {
            let saved = rest.contains("(saved as your default");
            return Reply::Set { level, saved };
        }
    } else if let Some(rest) = out.strip_prefix("Effort level set to ") {
        let level = word(rest);
        if !level.is_empty() {
            let saved = level == AUTO;
            return Reply::Set { level, saved };
        }
    } else if let Some(rest) = out.strip_prefix("Kept effort level as ") {
        let level = word(rest);
        return Reply::Kept((!level.is_empty()).then_some(level));
    }
    Reply::Other(out.to_string())
}

/// The level word at the start of `rest`: up to a blank, `:` or `(`, a code span's backticks
/// aside.
fn word(rest: &str) -> String {
    rest.trim_start()
        .trim_start_matches('`')
        .split(|c: char| c.is_whitespace() || c == ':' || c == '(' || c == '`')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Is `level` a word `/effort` can be given: lowercase letters only, 1 to 16 of them. Which
/// levels a model has is for Claude Code to say (and `fleet harness list` to list).
pub fn valid_level(level: &str) -> bool {
    (1..=16).contains(&level.len()) && level.chars().all(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::backend::strip_ansi;
    use crate::core::harness::Harness;
    use crate::core::input::{self, InputRead};
    use crate::core::prompt::{self, PromptRead};

    /// A live capture (testdata/screens/live): Claude Code 2.1.308 in a 200x50 tmux pane,
    /// `capture-pane -p -e`, the cwd, hook notices and plan name taken out.
    fn live(name: &str) -> String {
        let path = format!(
            "{}/../../testdata/screens/live/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn the_warm_cache_confirmation_for_that_level_is_confirmed_with_its_first_choice() {
        let confirm = live("claude-effort-confirm");
        let yes = Shown::Confirm { key: "1".into() };
        assert_eq!(shown(&confirm, "xhigh", true), yes);
        // As iTerm would give it: the words alone.
        let plain: Vec<String> = confirm.lines().map(strip_ansi).collect();
        assert_eq!(shown(&plain.join("\n"), "xhigh", false), yes);
        // Another level's confirmation is not this one's.
        for level in ["high", "max", "Xhigh", "xhig", "auto", ""] {
            assert_eq!(shown(&confirm, level, true), Shown::Other, "{level}");
        }
    }

    // One reader: `fleet answer --list` reads the dialog, the hint on its `▔` top and all, as
    // an answerable prompt, and a send finds a prompt in place of the box.
    #[test]
    fn answer_and_the_input_box_read_the_same_confirmation() {
        let confirm = live("claude-effort-confirm");
        let PromptRead::Prompt(p) =
            prompt::permission_prompt(&slash::lines(&confirm).join("\n"), Harness::Claude)
        else {
            panic!("not read as a prompt");
        };
        assert_eq!(p.title, "Change effort level?");
        let keys: Vec<_> = p.choices.iter().map(|c| c.key.as_deref()).collect();
        assert_eq!(keys, [Some("1"), Some("2")]);
        assert_eq!(
            input::read(&confirm, Harness::Claude, true),
            InputRead::Prompt
        );
    }

    #[test]
    fn a_confirmation_in_other_words_is_not_the_one_confirmed() {
        let confirm = live("claude-effort-confirm");
        let edits: [(&str, &str); 8] = [
            (
                "cached for the current effort level",
                "cached for the current model",
            ),
            // A hook asking, not the warm cache: in whichever words name it.
            (
                "Your next response will be slower",
                "A hook asked you to confirm. Your next response will be slower",
            ),
            (
                "Your next response will be slower",
                "A PreModelSwitch hook asked you to confirm. Your next response will be slower",
            ),
            (
                "Your next response will be slower",
                "Confirmation requested by PreModelSwitch. Your next response will be slower",
            ),
            (
                "Your next response will be slower",
                "Hook says: confirm. Your next response will be slower",
            ),
            ("No, go back", "No, keep medium"),
            ("Yes, switch to", "Sure, switch to"),
            ("No, go back", "No, go back\n\n   Esc to cancel"),
        ];
        for (from, to) in edits {
            assert!(confirm.contains(from), "{from}");
            let edited = confirm.replacen(from, to, 1);
            assert_eq!(shown(&edited, "xhigh", true), Shown::Other, "{to}");
        }
        // `/model`'s dialog, the same layout, is not this one; nor is this one `/model`'s.
        let model = confirm.replacen("Change effort level?", "Switch model?", 1);
        assert_eq!(shown(&model, "xhigh", true), Shown::Other);
        assert_eq!(
            crate::core::model::shown(&confirm, "xhigh", true),
            Shown::Other
        );
    }

    #[test]
    fn the_input_box_whatever_was_answered_above_it_is_the_box() {
        // Switched at once (with a tmux hint banner over the box), declined, max, auto, and
        // after a confirmed switch.
        for name in [
            "claude-effort-cold",
            "claude-effort-declined",
            "claude-effort-max",
            "claude-effort-auto",
            "claude-effort-confirmed",
        ] {
            let screen = live(name);
            assert_eq!(shown(&screen, "xhigh", true), Shown::Box, "{name}");
            assert_eq!(
                input::read(&screen, Harness::Claude, true),
                InputRead::Empty,
                "{name}"
            );
            assert_eq!(input::claude_working(&screen), None, "{name}");
        }
    }

    /// The `/effort` entries of a live Claude Code 2.1.308 transcript, every id and time made
    /// up: medium cold, xhigh declined, xhigh confirmed, max, auto.
    fn transcript() -> Vec<String> {
        let path = format!(
            "{}/../../testdata/claude/effort-transcript.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn read(level: &str, lines: &[String]) -> Vec<Reply> {
        let mut r = Replies::to(level);
        lines
            .iter()
            .filter_map(|l| r.read(l))
            .map(|(r, _)| r)
            .collect()
    }

    fn set(level: &str, saved: bool) -> Reply {
        Reply::Set {
            level: level.into(),
            saved,
        }
    }

    #[test]
    fn the_transcript_says_what_effort_answered() {
        let t = transcript();
        assert_eq!(read("medium", &t), [set("medium", true)]);
        assert_eq!(
            read("xhigh", &t),
            [Reply::Kept(Some("medium".into())), set("xhigh", true)]
        );
        assert_eq!(read("max", &t), [set("max", false)]);
        assert_eq!(read("auto", &t), [set("auto", true)]);
        assert!(read("high", &t).is_empty());
        // Its time is the output entry's.
        let mut r = Replies::to("max");
        let at: Vec<String> = t.iter().filter_map(|l| r.read(l)).map(|(_, a)| a).collect();
        assert_eq!(at, ["2026-01-02T03:14:13.383Z"]);
    }

    #[test]
    fn the_words_of_an_answer() {
        for (out, want) in [
            (
                "Set effort level to high (saved as your default for new sessions): Thorough",
                set("high", true),
            ),
            (
                "Set effort level to max (this session only): Most",
                set("max", false),
            ),
            ("Set effort level to `low`", set("low", false)),
            ("Set effort level to xhigh: deeper", set("xhigh", false)),
            ("Effort level set to auto", set("auto", true)),
            ("Effort level set to high", set("high", false)),
            (
                "Kept effort level as medium",
                Reply::Kept(Some("medium".into())),
            ),
            ("Kept effort level as ", Reply::Kept(None)),
            (
                "Invalid effort level: turbo",
                Reply::Other("Invalid effort level: turbo".into()),
            ),
            (
                "Set effort level to ",
                Reply::Other("Set effort level to ".into()),
            ),
        ] {
            assert_eq!(said(out), want, "{out}");
        }
    }

    // A prompt typed as exactly the switch's output, or an answer under another level's
    // command, or one tied to nothing, is no answer to this command. Words fleet does not know
    // count only tied to the command.
    #[test]
    fn only_an_answer_under_its_own_command_counts() {
        let t = transcript();
        let max = t
            .iter()
            .position(|l| l.contains("Set effort level to max"))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&t[max]).unwrap();
        let moved = |parent: &str, prompt: &str| {
            let mut v = v.clone();
            v["parentUuid"] = parent.into();
            v["promptId"] = prompt.into();
            v.to_string()
        };
        // Not under its command: neither its parent nor its prompt id.
        let forged = moved(
            "aaaaaaaa-0000-4000-8000-000000000099",
            "bbbbbbbb-0000-4000-8000-000000000099",
        );
        let mut lines = t[..max].to_vec();
        lines.push(forged);
        assert!(read("max", &lines).is_empty());
        // Either tie is enough.
        let command = &t[max - 1];
        let c: serde_json::Value = serde_json::from_str(command).unwrap();
        let (uuid, prompt) = (c["uuid"].as_str().unwrap(), c["promptId"].as_str().unwrap());
        for (parent, pid) in [
            (uuid, "bbbbbbbb-0000-4000-8000-000000000099"),
            ("x", prompt),
        ] {
            let mut lines = t[..max].to_vec();
            lines.push(moved(parent, pid));
            assert_eq!(read("max", &lines), [set("max", false)], "{parent} {pid}");
        }
        // Under another level's command.
        assert!(read("high", &t[..=max]).is_empty());
        // Unknown words under its own command: what Claude Code said.
        let mut odd = v.clone();
        odd["message"]["content"] =
            "<local-command-stdout>Invalid effort level: max</local-command-stdout>".into();
        let odd = odd.to_string();
        let mut lines = t[..max].to_vec();
        lines.push(odd);
        assert_eq!(
            read("max", &lines),
            [Reply::Other("Invalid effort level: max".into())]
        );
        // The same words as a `system` entry: tied to no command, not counted.
        let system = r#"{"type":"system","subtype":"local_command","content":"<local-command-stdout>Invalid effort level: max</local-command-stdout>","timestamp":"2026-01-02T03:04:05.000Z"}"#;
        assert!(Replies::to("max").read(system).is_none());
        let known = system.replace(
            "Invalid effort level: max",
            "Set effort level to max (this session only)",
        );
        assert_eq!(
            Replies::to("max").read(&known).map(|(r, _)| r),
            Some(set("max", false))
        );
    }

    #[test]
    fn a_level_is_lowercase_letters() {
        for ok in ["low", "medium", "xhigh", "max", "auto", "abcdefghijklmnop"] {
            assert!(valid_level(ok), "{ok}");
        }
        for bad in [
            "",
            "High",
            "x-high",
            "max ",
            "high;",
            "abcdefghijklmnopq",
            "5",
        ] {
            assert!(!valid_level(bad), "{bad}");
        }
    }
}
