//! Claude Code's `/model <id>`, read for `fleet model`: what its screen shows after the command
//! is typed, and what it writes into the session's transcript.
//!
//! **The screen.** `/model <id>` either switches at once, or — while the prompt cache is warm —
//! first asks to confirm, in place of the input box:
//!
//! ```text
//! ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
//!    Switch model?
//!    Your next response will be slower and use more tokens
//!
//!    This conversation is cached for the current model. Switching to Sonnet 5.5 means the full
//!    history gets re-read on your next message.
//!
//!    ❯ 1. Yes, switch to Sonnet 5.5
//!      2. No, go back
//! ```
//!
//! [`shown`] tells that confirmation — exactly that one, for that model — from the input box
//! and from anything else, by [`slash::shown`], the reader `fleet effort` uses for its own. An
//! unknown id is answered under the command, the box back (`⎿  Model '<id>' not found`), and so
//! is a `PreModelSwitch` hook's refusal (`Model switch to <label> was blocked by a
//! PreModelSwitch hook[: <reason>]`, as Claude Code 2.1.303's bundle words it): [`not_found`]
//! and [`refused`] count those lines.
//!
//! **The transcript.** Claude Code writes the command's output wrapped in
//! `<local-command-stdout>…</local-command-stdout>`: `Set model to <label> …` once it switched,
//! `Kept model as <label>` when the confirmation was declined, `Model '<id>' not found`, a
//! hook's refusal. 2.1.306 writes a switch as a `type: "user"` entry (its `message.content`),
//! the rest as `type: "system", subtype: "local_command"` entries (their `content`).
//! [`Replies`] reads the new lines for them, either shape ([`slash::Answers`]), and the label
//! the first two name — a `user` entry only as the output of the `/model <id>` entry before
//! it, since a prompt typed as exactly that output is written the same; [`same_model`]
//! tells a label that stands for the model asked for, [`names_other`] one that stands for
//! another.

use crate::core::harness;
use crate::core::slash::{self, Answers, Dialog};

pub use crate::core::slash::{Shown, Tail, at_or_after};

/// `/model`'s confirmation.
const DIALOG: Dialog = Dialog {
    title: "Switch model?",
    cached_for: "cached for the current model",
};

/// What `screen` (a capture of a Claude Code session, with its escape sequences when `styled`)
/// shows after `/model <id>` was typed. See [`Shown`]. The confirmation's label is matched to
/// `id` as a switch's is ([`same_model`]): Claude Code names the model an alias resolves to
/// (`sonnet`: "Yes, switch to Sonnet 5.5").
pub fn shown(screen: &str, id: &str, styled: bool) -> Shown {
    slash::shown(screen, styled, DIALOG, |label| same_model(label, id))
}

/// Does the label a confirmation names (`Sonnet 5.5`, `Opus 5.5 (1M context)`) stand for
/// model `id`? Its [`label_for`], alone or followed by ` (…)`; or the id itself, as Claude Code
/// names a model it has no label for.
fn labels(label: &str, id: &str) -> bool {
    if label.eq_ignore_ascii_case(id.trim()) {
        return true;
    }
    label_for(id).is_some_and(|l| {
        label == l
            || label
                .strip_prefix(l.as_str())
                .is_some_and(|rest| rest.starts_with(" (") && rest.ends_with(')'))
    })
}

/// The label Claude Code gives model `id`: the one fleet's Claude model list
/// ([`harness::claude_models`], what `fleet harness list` shows) pairs with it, else
/// [`label_of`]'s. `None` for an id it cannot tell (an alias such as `sonnet`, another
/// provider's).
pub fn label_for(id: &str) -> Option<String> {
    let id = id.trim();
    harness::claude_models(None)
        .into_iter()
        .find(|m| !m.id.is_empty() && m.id.eq_ignore_ascii_case(id))
        .map(|m| m.label)
        .or_else(|| label_of(id))
}

/// Does `label` — the model a `Set model to <label>` names — stand for model `id`, as far as
/// fleet can tell? Only on a positive match: the id itself (a ` (…)` after it aside), as
/// Claude Code names a model it has no label for; or one model's label on both sides — `id`'s
/// ([`wanted`]: from fleet's model list, by the Claude model id rule, or an alias such as
/// `sonnet` resolved through that list) and `label` read as a Claude model's (`Sonnet 5.5`, a
/// ` (1M context)` after it aside) or as a Claude model id. A `[1m]` or date suffix on either
/// side is the same model. Anything fleet cannot resolve (`Default (recommended)`, a label in
/// other words, an id of another shape) is not a match.
pub fn same_model(label: &str, id: &str) -> bool {
    if labels(label, id) || bare(label).eq_ignore_ascii_case(id.trim()) {
        return true;
    }
    match (wanted(id), model_label(label)) {
        (Some(want), Some(got)) => got.eq_ignore_ascii_case(&want),
        _ => false,
    }
}

/// Does `label` stand for another model than `id`: both resolve ([`same_model`]'s), to
/// different models? `false` when either cannot be resolved — nothing to tell it by.
pub fn names_other(label: &str, id: &str) -> bool {
    !same_model(label, id) && wanted(id).is_some() && model_label(label).is_some()
}

/// The label of the model `id` asks for: [`label_for`]'s, else — an alias, a Claude model
/// family's name alone (`sonnet`, `opus[1m]`) — the label of the one model of that family on
/// fleet's Claude model list ([`harness::claude_models`]). `None` for anything else.
pub fn wanted(id: &str) -> Option<String> {
    if let Some(l) = label_for(id) {
        return Some(l);
    }
    let alias = id.trim().to_ascii_lowercase();
    let alias = alias.strip_suffix("[1m]").unwrap_or(&alias);
    if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let prefix = format!("claude-{alias}-");
    let mut family = harness::claude_models(None)
        .into_iter()
        .filter(|m| m.id.to_ascii_lowercase().starts_with(&prefix));
    match (family.next(), family.next()) {
        (Some(m), None) => Some(m.label),
        _ => None,
    }
}

/// `label` without a ` (…)` after it.
fn bare(label: &str) -> &str {
    let l = label.trim();
    match l.rfind(" (") {
        Some(i) if l.ends_with(')') => &l[..i],
        _ => l,
    }
}

/// `label` as a Claude model's label: `Sonnet 5.5` (a ` (1M context)` after it aside), or the
/// [`label_of`] the Claude model id it is. `None` for words of any other shape.
fn model_label(label: &str) -> Option<String> {
    let l = bare(label);
    if let Some(of) = label_of(l) {
        return Some(of);
    }
    let (family, version) = l.split_once(' ')?;
    let family = family
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
        && family.chars().all(|c| c.is_ascii_alphabetic());
    let nums: Vec<&str> = version.split('.').collect();
    let version = nums.len() <= 2
        && nums
            .iter()
            .all(|n| !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()));
    (family && version).then(|| l.to_string())
}

/// The label Claude Code gives a Claude model id: `claude-sonnet-5-5` → `Sonnet 5.5`,
/// `claude-opus-4-1-20250805` → `Opus 4.1`, `claude-sonnet-4-20250514` → `Sonnet 4`; a
/// trailing `[1m]` aside. `None` for an id not of that shape (an alias, another provider's).
pub fn label_of(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_lowercase();
    let id = id.strip_suffix("[1m]").unwrap_or(&id);
    let mut parts = id.strip_prefix("claude-")?.split('-');
    let family = parts
        .next()
        .filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphabetic()))?;
    let mut nums: Vec<&str> = parts.collect();
    let digits = |p: &&str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
    if nums.len() > 1 && nums.last().is_some_and(|d| d.len() == 8 && digits(d)) {
        nums.pop();
    }
    if nums.is_empty() || nums.len() > 2 || !nums.iter().all(|n| digits(n) && n.len() <= 2) {
        return None;
    }
    let mut cs = family.chars();
    let first = cs.next()?.to_ascii_uppercase();
    Some(format!("{first}{} {}", cs.as_str(), nums.join(".")))
}

/// How many lines of `screen` say Claude Code has no model `id`: `⎿  Model '<id>' not found`,
/// under the `/model <id>` it answers.
pub fn not_found(screen: &str, id: &str) -> usize {
    let said = format!("Model '{}' not found", id.trim());
    slash::lines(screen)
        .iter()
        .filter(|l| l.trim().trim_start_matches('⎿').trim() == said)
        .count()
}

/// How many lines of `screen` say a `PreModelSwitch` hook blocked a switch (`Model switch to
/// <label> was blocked by a PreModelSwitch hook[: <reason>]`).
pub fn refused(screen: &str) -> usize {
    slash::lines(screen).iter().filter(|l| blocked(l)).count()
}

fn blocked(text: &str) -> bool {
    text.contains("blocked by a PreModelSwitch hook")
}

/// What Claude Code answered a `/model` with, in its transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `Set model to <label> …`: switched, to the model `label` names (`None` when it cannot
    /// be read off the words).
    Switched(Option<String>),
    /// `Kept model as <label>`: the confirmation was declined; the model is unchanged.
    Kept(Option<String>),
    /// `Model '<id>' not found`.
    NotFound,
    /// A `PreModelSwitch` hook blocked it.
    Refused,
}

/// The replies to one `/model <id>` that a transcript's new lines hold, read in the order they
/// were written: [`slash::Answers`]' outputs, read as [`said`] reads them. A `user` entry
/// counts only as the output of that very command: a prompt typed as exactly
/// `<local-command-stdout>Set model to …</local-command-stdout>` is written as such an entry
/// too, so one counts only when it answers an entry naming `/model <id>` read before it —
/// 2.1.306 writes a meta caveat, the command, then its output, under one `promptId`, each the
/// parent of the next. A `system` entry, which no prompt is written as, counts as it is.
pub struct Replies(Answers);

impl Replies {
    /// For the command `/model <id>`.
    pub fn to(id: &str) -> Self {
        Replies(Answers::to("model", id))
    }

    /// The reply transcript line `line`, the next one written, records (see [`Replies`]).
    pub fn read(&mut self, line: &str) -> Option<(Reply, String)> {
        let a = self.0.read(line)?;
        Some((said(&a.text)?, a.at))
    }
}

/// The `/model` reply transcript line `line` records, with its `timestamp` (as written), read
/// alone: the command's output ([`slash::output`]) — `Set model to …` (2.1.306's, a `user`
/// entry after a meta `<local-command-caveat>` entry and the one naming the command), `Kept
/// model as …`, `Model '…' not found`, a hook's refusal (`system` entries; read the same should
/// any other reply come so). `None` for any other line — the entry naming the command itself,
/// a prompt that only quotes such output, a tool's result — included.
#[cfg(test)]
fn reply(line: &str) -> Option<(Reply, String)> {
    let (out, at, _) = slash::output(line)?;
    Some((said(&out)?, at))
}

/// What the output of a `/model` says.
fn said(out: &str) -> Option<Reply> {
    Some(if let Some(rest) = out.strip_prefix("Set model to ") {
        Reply::Switched(named(rest))
    } else if let Some(rest) = out.strip_prefix("Kept model as ") {
        Reply::Kept(named(rest))
    } else if out.starts_with("Model '") && out.contains("' not found") {
        Reply::NotFound
    } else if blocked(out) {
        Reply::Refused
    } else {
        return None;
    })
}

/// The model label at the start of `rest`, what follows `Set model to ` or `Kept model as `:
/// Claude Code writes it as a code span (`` `Sonnet 5.5` ``, drawn in bold), then ` and saved
/// as your default for new sessions`, ` for this session only` or nothing.
fn named(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let label = match rest.strip_prefix('`') {
        Some(span) => span.split('`').next().unwrap_or_default(),
        None => [
            " and saved as your default",
            " for this session only",
            " · ",
        ]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .map_or(rest, |end| &rest[..end]),
    };
    let label = label.trim();
    (!label.is_empty()).then(|| label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::backend::strip_ansi;
    use crate::core::harness::Harness;
    use crate::core::input::{self, InputRead};
    use crate::core::prompt::{self, PromptRead};
    use crate::core::slash::lines;

    /// A live capture (testdata/screens/live, see `core::input`'s tests): Claude Code 2.1.303
    /// in a 110x34 tmux pane, `capture-pane -p -e`.
    fn live(name: &str) -> String {
        screen(&format!("live/{name}"))
    }

    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../testdata/screens/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    const SONNET: &str = "claude-sonnet-5-5";

    #[test]
    fn the_warm_cache_confirmation_for_that_model_is_confirmed_with_its_first_choice() {
        let confirm = live("claude-model-confirm");
        let yes = Shown::Confirm { key: "1".into() };
        assert_eq!(shown(&confirm, SONNET, true), yes);
        assert_eq!(shown(&confirm, "claude-sonnet-5-5-20260101", true), yes);
        assert_eq!(shown(&confirm, "claude-sonnet-5-5[1m]", true), yes);
        // As iTerm would give it: the words alone.
        let plain: Vec<String> = confirm.lines().map(strip_ansi).collect();
        assert_eq!(shown(&plain.join("\n"), SONNET, false), yes);
        // An alias: Claude Code names the model it resolves to, as fleet's model list does.
        assert_eq!(shown(&confirm, "sonnet", true), yes);
        assert_eq!(shown(&confirm, "SONNET", true), yes);
        // Another model's confirmation is not this one's.
        for id in [
            "claude-opus-5-5",
            "claude-sonnet-5-1",
            "opus",
            "haiku",
            "claude-haiku-5-5",
            "sonnet-latest",
        ] {
            assert_eq!(shown(&confirm, id, true), Shown::Other, "{id}");
        }
    }

    // "Running PreModelSwitch hooks… (esc to cancel)", dim, where the box was while the hooks
    // run (Claude Code 2.1.308's bundle): pending, not another screen. A line of other words
    // last, or a confirmation of another model, is not.
    #[test]
    fn hooks_still_running_are_pending() {
        let running = "> Say hello.\n\n* Hello.\n\n\x1b[2mRunning PreModelSwitch hooks… (esc to cancel)\x1b[0m\n\n";
        assert_eq!(shown(running, SONNET, true), Shown::Pending);
        assert_eq!(shown(&strip_ansi(running), SONNET, false), Shown::Pending);
        assert_eq!(shown("Applying…", SONNET, true), Shown::Pending);
        for other in [
            "Running PreModelSwitch hooks",
            "Model switch cancelled",
            "…",
            "Running… hooks done",
            "",
        ] {
            assert_eq!(shown(other, SONNET, true), Shown::Other, "{other}");
        }
        let confirm = live("claude-model-confirm");
        assert_eq!(shown(&confirm, "claude-haiku-5-5", true), Shown::Other);
    }

    // One reader: `fleet answer --list` reads the same dialog as an answerable prompt, and a
    // send (or `model`, before typing) finds a prompt in place of the box.
    #[test]
    fn answer_and_the_input_box_read_the_same_confirmation() {
        let confirm = live("claude-model-confirm");
        let PromptRead::Prompt(p) =
            prompt::permission_prompt(&lines(&confirm).join("\n"), Harness::Claude)
        else {
            panic!("not read as a prompt");
        };
        assert_eq!(p.title, "Switch model?");
        let keys: Vec<_> = p.choices.iter().map(|c| c.key.as_deref()).collect();
        assert_eq!(keys, [Some("1"), Some("2")]);
        assert_eq!(
            input::read(&confirm, Harness::Claude, true),
            InputRead::Prompt
        );
    }

    #[test]
    fn a_confirmation_in_other_words_is_not_the_one_confirmed() {
        let confirm = live("claude-model-confirm");
        let edits: [(&str, &str); 8] = [
            // A PreModelSwitch hook asking, not the warm cache: in whichever words name it.
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
                "PreModelSwitch Hook: confirm. Your next response will be slower",
            ),
            (
                "cached for the current model",
                "cached for the current effort",
            ),
            // A third choice, or choices of other words.
            (
                "2. \x1b[39mNo, go back",
                "2. \x1b[39mNo, go back\n     3. Yes, and don't ask again",
            ),
            ("No, go back", "No, keep Opus 5.5"),
            ("Yes, switch to", "Sure, switch to"),
            // Something drawn under it.
            (
                "No, go back",
                "No, go back\n\n   Esc to cancel · Tab to amend",
            ),
        ];
        for (from, to) in edits {
            assert!(confirm.contains(from), "{from}");
            let edited = confirm.replacen(from, to, 1);
            assert_eq!(shown(&edited, SONNET, true), Shown::Other, "{to}");
        }
        // Choices not numbered 1, 2 from the top are no answer list the reader takes.
        let renumbered = confirm.replacen("1. ", "3. ", 1);
        assert_eq!(shown(&renumbered, SONNET, true), Shown::Other);
        // `/effort`'s dialog, the same layout, is not this one.
        let effort = confirm.replacen("Switch model?", "Change effort level?", 1);
        assert_eq!(shown(&effort, SONNET, true), Shown::Other);
    }

    #[test]
    fn the_input_box_whatever_it_holds_is_the_box() {
        for name in [
            // `/model claude-sonnet-5-5` typed, not yet taken.
            "claude-model-typed",
            // Answered above the box: not found, and a confirmation declined.
            "claude-model-not-found",
            "claude-model-cancelled",
            "claude-empty",
            "claude-busy",
        ] {
            assert_eq!(shown(&live(name), SONNET, true), Shown::Box, "{name}");
        }
    }

    #[test]
    fn anything_else_in_place_of_the_box_is_other() {
        for name in ["claude-permission", "claude-transcript"] {
            assert_eq!(shown(&screen(name), SONNET, true), Shown::Other, "{name}");
        }
        assert_eq!(shown("", SONNET, true), Shown::Other);
    }

    #[test]
    fn model_not_found_is_counted_for_that_id_alone() {
        let nf = live("claude-model-not-found");
        assert_eq!(not_found(&nf, "claude-nonexistent-9-9"), 1);
        assert_eq!(not_found(&nf, SONNET), 0);
        assert_eq!(
            not_found(&live("claude-model-cancelled"), "claude-nonexistent-9-9"),
            0
        );
        assert_eq!(refused(&nf), 0);
        let blocked = nf.replace(
            "Model 'claude-nonexistent-9-9' not found",
            "Model switch to Sonnet 5.5 was blocked by a PreModelSwitch hook: not on Fridays",
        );
        assert_eq!(refused(&blocked), 1);
        assert_eq!(not_found(&blocked, "claude-nonexistent-9-9"), 0);
    }

    #[test]
    fn a_claude_model_id_has_the_label_claude_code_gives_it() {
        let cases = [
            ("claude-sonnet-5-5", Some("Sonnet 5.5")),
            ("claude-opus-5-5[1m]", Some("Opus 5.5")),
            ("Claude-Fable-5-1", Some("Fable 5.1")),
            ("claude-opus-4-1-20250805", Some("Opus 4.1")),
            ("claude-sonnet-4-20250514", Some("Sonnet 4")),
            ("claude-haiku-5", Some("Haiku 5")),
            ("sonnet", None),
            ("opus[1m]", None),
            ("claude-3-5-sonnet-20241022", None),
            ("claude-sonnet", None),
            ("claude-sonnet-5-5-5", None),
            ("gpt-6", None),
        ];
        for (id, want) in cases {
            assert_eq!(label_of(id).as_deref(), want, "{id}");
        }
        assert!(labels("Opus 5.5 (1M context)", "claude-opus-5-5"));
        assert!(labels("my-model", "my-model"));
        assert!(!labels("Opus 5.5x", "claude-opus-5-5"));
        assert!(!labels("Opus 5.5 (1M context", "claude-opus-5-5"));
    }

    // fleet's Claude model list (`fleet harness list`) and the rule `label_of` reads ids by
    // agree on every model the list names.
    #[test]
    fn the_model_list_and_the_label_rule_agree() {
        let listed = harness::claude_models(None);
        assert!(listed.len() > 1);
        for m in listed.iter().filter(|m| !m.id.is_empty()) {
            assert_eq!(
                label_of(&m.id).as_deref(),
                Some(m.label.as_str()),
                "{}",
                m.id
            );
            assert_eq!(
                label_for(&m.id).as_deref(),
                Some(m.label.as_str()),
                "{}",
                m.id
            );
        }
    }

    // `Set model to <label>`: the model asked for only on a positive match — the id itself,
    // or one model's label on both sides, aliases resolved through fleet's model list and a
    // `[1m]` or date suffix the same model. Another model only when both resolve.
    #[test]
    fn a_switch_is_to_the_model_asked_for_only_on_a_positive_match() {
        for (label, id) in [
            ("Sonnet 5.5", SONNET),
            ("Sonnet 5.5 (1M context)", SONNET),
            ("Sonnet 5.5", "claude-sonnet-5-5-20260101"),
            ("Sonnet 5.5", "claude-sonnet-5-5[1m]"),
            ("Opus 5.5", "claude-opus-5-5[1m]"),
            ("Opus 5.5 (1M context)", "claude-opus-5-5[1m]"),
            ("claude-sonnet-5-5", SONNET),
            ("claude-sonnet-5-5-20260101", SONNET),
            ("claude-sonnet-5-5[1m]", SONNET),
            // Aliases, through fleet's model list.
            ("Sonnet 5.5", "sonnet"),
            ("Opus 5.5 (1M context)", "opus[1m]"),
            ("Fable 5.1", "FABLE"),
            // The id itself: a model Claude Code has no label for.
            ("my-model", "my-model"),
            ("sonnet (claude-sonnet-5-5)", "sonnet"),
        ] {
            assert!(same_model(label, id), "{label} for {id}");
            assert!(!names_other(label, id), "{label} for {id}");
        }
        // Not resolved: neither this model nor, as far as fleet can tell, another.
        for (label, id) in [
            ("Default (recommended)", SONNET),
            ("Default (recommended)", "sonnet"),
            ("Sonnet 5.5 Fast", SONNET),
            ("sonnet 5.5", SONNET),
            ("my-model", SONNET),
            ("Haiku 5.5", "gpt-6"),
            ("Haiku 5.5", "claude"),
            ("Sonnet 5.5", "sonnet-latest"),
            ("", SONNET),
        ] {
            assert!(!same_model(label, id), "{label} for {id}");
            assert!(!names_other(label, id), "{label} for {id}");
        }
        // Another model.
        for (label, id) in [
            ("Haiku 5.5", SONNET),
            ("Sonnet 5.1", SONNET),
            ("Opus 5.5 (1M context)", SONNET),
            ("Fable 5.1", "claude-opus-5-5"),
            ("Sonnet 4", "claude-sonnet-4-5-20250929"),
            ("claude-haiku-5-5", SONNET),
            ("Haiku 5.5", "sonnet"),
            ("Sonnet 5.5", "opus[1m]"),
        ] {
            assert!(!same_model(label, id), "{label} for {id}");
            assert!(names_other(label, id), "{label} for {id}");
        }
    }

    #[test]
    fn the_label_a_reply_names_is_read_off_its_words() {
        for (rest, want) in [
            (
                "Sonnet 5.5 and saved as your default for new sessions",
                Some("Sonnet 5.5"),
            ),
            (
                "`Sonnet 5.5` and saved as your default for new sessions",
                Some("Sonnet 5.5"),
            ),
            (
                "Opus 5.5 (1M context) for this session only · high effort",
                Some("Opus 5.5 (1M context)"),
            ),
            ("Opus 5.5 (1M context)", Some("Opus 5.5 (1M context)")),
            ("`Opus 5.5`", Some("Opus 5.5")),
            ("", None),
            ("``", None),
        ] {
            assert_eq!(named(rest).as_deref(), want, "{rest}");
        }
    }

    /// testdata/claude/model-transcript.jsonl: a synthetic Claude Code transcript (made-up
    /// values, the record shapes of Claude Code 2.1.306) — a prompt and its reply, then
    /// `/model` typed four times: not found and declined (`system` entries), switched (`user`
    /// entries: a meta caveat, the command, its output), and a hook's refusal in the words of
    /// Claude Code's bundle.
    fn transcript() -> Vec<String> {
        let path = format!(
            "{}/../../testdata/claude/model-transcript.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_transcript_says_what_model_answered() {
        let replies: Vec<(Reply, String)> = transcript().iter().filter_map(|l| reply(l)).collect();
        assert_eq!(
            replies,
            [
                (Reply::NotFound, "2026-01-02T03:05:01.100Z".to_string()),
                (
                    Reply::Kept(Some("Opus 5.5".into())),
                    "2026-01-02T03:06:02.200Z".to_string()
                ),
                (
                    Reply::Switched(Some("Sonnet 5.5".into())),
                    "2026-01-02T03:07:03.300Z".to_string()
                ),
                (Reply::Refused, "2026-01-02T03:08:04.400Z".to_string()),
            ]
        );
        // A prompt, a reply, the command's own entry (either shape), the caveat: none.
        let t = transcript();
        for line in t.iter().filter(|l| !l.contains("local-command-stdout")) {
            assert!(reply(line).is_none(), "{line}");
        }
        // The switch as a `system` entry, its label in bold, is read the same.
        let system = r#"{"type":"system","subtype":"local_command","content":"<local-command-stdout>Set model to \u001b[1mSonnet 5.5\u001b[22m and saved as your default for new sessions</local-command-stdout>","timestamp":"2026-01-02T03:07:03.300Z"}"#;
        assert_eq!(
            reply(system),
            Some((
                Reply::Switched(Some("Sonnet 5.5".into())),
                "2026-01-02T03:07:03.300Z".into()
            ))
        );
        // Not the command's output: a meta or sidechain entry, a prompt quoting it, a tool's
        // result, an assistant's, a user entry with it only at its top level.
        let switched = t.iter().find(|l| l.contains("Set model to")).unwrap();
        let v: serde_json::Value = serde_json::from_str(switched).unwrap();
        let edited = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v = v.clone();
            f(&mut v);
            v.to_string()
        };
        let output = v["message"]["content"].as_str().unwrap().to_string();
        for line in [
            edited(&|v| v["isMeta"] = true.into()),
            edited(&|v| v["isSidechain"] = true.into()),
            edited(&|v| v["message"]["content"] = format!("what does {output} mean?").into()),
            edited(&|v| v["message"]["content"] = format!("{output} why?").into()),
            edited(&|v| {
                v["message"]["content"] =
                    serde_json::json!([{ "type": "tool_result", "content": output }])
            }),
            edited(&|v| v["type"] = "assistant".into()),
            r#"{"type":"user","subtype":"local_command","content":"<local-command-stdout>Set model to X</local-command-stdout>","timestamp":"2026-01-02T03:04:05.000Z"}"#.to_string(),
        ] {
            assert!(reply(&line).is_none(), "{line}");
        }
        assert!(reply("not json local-command-stdout").is_none());
    }

    // A prompt typed as exactly the switch's output is written as the switch's own entry is: it
    // counts only as the answer to the entry naming `/model <id>` before it.
    #[test]
    fn a_prompt_typed_as_the_switch_is_no_switch() {
        let t = transcript();
        let read = |id: &str, lines: &[String]| -> Vec<Reply> {
            let mut r = Replies::to(id);
            lines
                .iter()
                .filter_map(|l| r.read(l))
                .map(|(r, _)| r)
                .collect()
        };
        // The whole transcript: every reply to its command, the switch included.
        assert_eq!(
            read(SONNET, &t),
            [
                Reply::NotFound,
                Reply::Kept(Some("Opus 5.5".into())),
                Reply::Switched(Some("Sonnet 5.5".into())),
                Reply::Refused,
            ]
        );
        let switched = t.iter().position(|l| l.contains("Set model to")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&t[switched]).unwrap();
        let as_prompt = |parent: &str, prompt: &str| {
            let mut v = v.clone();
            v["parentUuid"] = parent.into();
            v["promptId"] = prompt.into();
            v.to_string()
        };
        // Typed after the reply, no command before it: no switch, whatever the id.
        let forged = as_prompt(
            "aaaaaaaa-0000-4000-8000-000000000002",
            "bbbbbbbb-0000-4000-8000-000000000009",
        );
        let lines = [t[0].clone(), t[1].clone(), forged.clone()];
        assert!(read(SONNET, &lines).is_empty());
        assert!(
            reply(&forged).is_some(),
            "the entry alone reads as a switch"
        );
        // Under the caveat alone, the command's entry missing.
        let caveat = &t[switched - 2];
        assert!(caveat.contains("local-command-caveat"));
        let mut lines = t[..switched - 1].to_vec();
        lines.push(t[switched].clone());
        assert_eq!(
            read(SONNET, &lines).last(),
            Some(&Reply::Kept(Some("Opus 5.5".into())))
        );
        // The command's entry there, but for another model: not its answer.
        let haiku = "claude-haiku-5-5";
        assert!(
            !read(haiku, &t[..=switched]).contains(&Reply::Switched(Some("Sonnet 5.5".into())))
        );
        // Its own command before it, but the prompt not under it: not its answer either.
        let mut lines = t[..switched].to_vec();
        lines.push(as_prompt(
            "aaaaaaaa-0000-4000-8000-000000000006",
            "bbbbbbbb-0000-4000-8000-000000000009",
        ));
        assert_eq!(
            read(SONNET, &lines).last(),
            Some(&Reply::Kept(Some("Opus 5.5".into())))
        );
        // Either tie to it is enough: its parent, or its prompt id.
        for (parent, prompt) in [
            (
                "aaaaaaaa-0000-4000-8000-000000000008",
                "bbbbbbbb-0000-4000-8000-000000000009",
            ),
            (
                "aaaaaaaa-0000-4000-8000-000000000006",
                "bbbbbbbb-0000-4000-8000-000000000002",
            ),
        ] {
            let mut lines = t[..switched].to_vec();
            lines.push(as_prompt(parent, prompt));
            assert_eq!(
                read(SONNET, &lines).last(),
                Some(&Reply::Switched(Some("Sonnet 5.5".into()))),
                "{parent} {prompt}"
            );
        }
    }
}
