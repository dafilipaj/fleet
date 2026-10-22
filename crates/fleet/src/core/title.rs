//! One title per session — the model of truth for what a session is called.
//!
//! A session used to carry three competing names: the Claude session name
//! (`/rename`), the tmux session name, and the generated title. Now:
//!
//! - **The Claude session name is the source of truth.** It is what `/rename`
//!   sets and what Claude persists in its session registry, so it survives a
//!   fleet restart and is the same on every view.
//! - **The display title is derived from it, once, here**
//!   ([`display_title`]): the Claude name when someone chose one, else the
//!   generated title, else a heuristic slug of the first prompt, else the
//!   short id. `list --json` carries it as `display_title`; every UI draws only
//!   that.
//! - **The tmux session name is derived too**: a slug of the title
//!   ([`tmux_name_for`]), brought in line on every rename that goes through
//!   [`apply_rename`] — a manual rename, `fleet name --apply` (auto-naming) and
//!   the dashboard's rename buffer — but only while the tmux session is that
//!   one Claude session's own (one window, one pane; see [`plan_tmux`]).
//!   A raw `tmux rename-session` is not watched: it sticks until the next
//!   title change overwrites it. `fleet tmux rename` of a single-Claude session
//!   renames the Claude session instead, so the edge runs through the title.
//! - iTerm tab titles are left alone: Claude already puts its session name in
//!   the terminal title (OSC), and a tab title someone set by hand is their
//!   filing system, not ours to overwrite.
//!
//! Renaming types `/rename <title>` into the live Claude TUI, so it is held for
//! a session waiting on a prompt, where typed keys are its answer (see [`Hold`]
//! for why a busy session is fine).

use serde::Serialize;

use crate::core::backend::{self, TmuxSync};
use crate::core::discovery::{Backend, Session};
use crate::core::naming;
use crate::error::{Error, Result};

/// The longest title any path may send. Generated names are capped at
/// [`naming::MAX_NAME`]; this is the looser cap for one a human typed, and it
/// exists because the title goes out as `/rename <title>` into a live TUI.
pub const MAX_TITLE: usize = 64;

/// The longest tmux session name derived from a title.
pub const MAX_TMUX_SLUG: usize = 48;

// --- the display title -------------------------------------------------------

/// The Claude session name, when someone (a human, `fleet name --apply`, the
/// web UI) chose it — `None` while it is still Claude's `<cwd>-9d` fallback.
pub fn chosen_name(s: &Session) -> Option<&str> {
    if s.is_derived_name() {
        return None;
    }
    s.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
}

/// What every view calls this session — the one title.
///
/// Precedence: the chosen Claude name, the generated title, a slug of the
/// first prompt, Claude's derived name, the short session id, the pid.
pub fn display_title(s: &Session) -> String {
    if let Some(n) = chosen_name(s) {
        return n.to_string();
    }
    if let Some(g) = s
        .gen_title
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        return g.to_string();
    }
    if let Some(h) = naming::heuristic_from(None, s.title.as_deref()) {
        return h;
    }
    s.label()
}

/// Fill `display_title` on every row (what `list --json` ships).
pub fn stamp_display_titles(rows: &mut [Session]) {
    for r in rows {
        r.display_title = Some(display_title(r));
    }
}

// --- validating a title ------------------------------------------------------

/// Trim, reject empty / multi-line, cap at [`MAX_TITLE`] characters.
pub fn clean_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Other("empty name".into()));
    }
    if title.contains(['\n', '\r']) {
        return Err(Error::Other("name must be a single line".into()));
    }
    Ok(title.chars().take(MAX_TITLE).collect::<String>())
}

// --- the derived tmux name ---------------------------------------------------

/// Fold the Latin letters people actually type in titles (`č`, `ß`, `é`…) to
/// ASCII, so `Popravak čišćenja` slugs to `popravak-ciscenja` rather than
/// losing every accented letter.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ą' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ą' => "a",
        'ç' | 'ć' | 'č' | 'Ç' | 'Ć' | 'Č' => "c",
        'ď' | 'đ' | 'Ď' | 'Đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ę' | 'Ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
        'ł' | 'Ł' | 'ľ' | 'Ľ' => "l",
        'ñ' | 'ń' | 'ň' | 'Ñ' | 'Ń' | 'Ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ő' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ő' => "o",
        'ř' | 'Ř' => "r",
        'ś' | 'š' | 'Ś' | 'Š' => "s",
        'ß' => "ss",
        'ť' | 'Ť' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ů' | 'ű' | 'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ů' | 'Ű' => "u",
        'ý' | 'ÿ' | 'Ý' => "y",
        'ź' | 'ż' | 'ž' | 'Ź' | 'Ż' | 'Ž' => "z",
        _ => return None,
    })
}

/// The tmux session name a title derives: a lowercase kebab slug, capped at
/// [`MAX_TMUX_SLUG`] on a word boundary. Empty when nothing usable is left
/// (an all-emoji title) — the caller then leaves tmux alone.
pub fn tmux_name_for(title: &str) -> String {
    let mut ascii = String::with_capacity(title.len());
    for c in title.chars() {
        match fold(c) {
            Some(f) => ascii.push_str(f),
            None => ascii.push(c),
        }
    }
    naming::slugify(&ascii, MAX_TMUX_SLUG)
}

/// Is `current` already the name `desired` derives to — itself, or itself with
/// the collision suffix a previous sync gave it (`fix-login-2`)?
pub fn is_derived_from(current: &str, desired: &str) -> bool {
    if current == desired {
        return true;
    }
    current
        .strip_prefix(desired)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What to do with a tmux session name after its Claude session got `title`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxPlan {
    Rename { from: String, to: String },
    Skip(String),
}

/// Decide the tmux side of a rename. Pure — every input comes from a live
/// tmux query the caller made — so each guard is testable without a server.
///
/// `existing` is every tmux session name on the server; `current` is excluded
/// from the collision check, or a session already called `fix-login-2` would
/// be "renamed" to `fix-login-3` on every sync.
pub fn plan_tmux(
    title: &str,
    current: &str,
    own: Option<&str>,
    windows: usize,
    panes: usize,
    existing: &[String],
) -> TmuxPlan {
    let desired = tmux_name_for(title);
    if desired.is_empty() {
        return TmuxPlan::Skip("nothing usable left of that title for tmux".into());
    }
    if is_derived_from(current, &desired) {
        return TmuxPlan::Skip(format!("tmux session already {current}"));
    }
    if let Some(why) = backend::tmux_guard(current, own, windows, panes) {
        return TmuxPlan::Skip(why);
    }
    let others: Vec<String> = existing.iter().filter(|e| *e != current).cloned().collect();
    TmuxPlan::Rename {
        from: current.to_string(),
        to: backend::unique_tmux_name(&desired, &others),
    }
}

// --- adopting a hand-picked tmux name ---------------------------------------

/// A tmux session name nobody chose: the web spawner's `fw-hhmmss`, tmux's
/// numeric default, or Claude's `<cwd>-9d` style.
pub fn is_generic_tmux_name(name: &str, cwd: Option<&str>) -> bool {
    if name.is_empty() {
        return false;
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits(name) {
        return true;
    }
    if let Some(rest) = name.strip_prefix("fw-")
        && rest.len() == 6
        && digits(rest)
    {
        return true;
    }
    let base = cwd
        .unwrap_or("")
        .rsplit('/')
        .find(|p| !p.is_empty())
        .unwrap_or("");
    if base.is_empty() {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let base = base.to_ascii_lowercase();
    lower
        .strip_prefix(&base)
        .and_then(|r| r.strip_prefix('-'))
        .is_some_and(|h| h.len() == 2 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The tmux session name to adopt as the title of a still-unnamed session.
///
/// Auto-naming used to leave hand-picked tmux names alone by syncing only
/// generic ones. With the tmux name derived from the title that protection
/// lives here instead: a session with Claude's derived name, sitting alone in a
/// tmux session somebody named (`fleet new fix-login`), takes that name as its
/// title — no model call — so the sync that follows is a no-op.
pub fn adoptable_tmux_name(s: &Session) -> Option<String> {
    if crate::core::discovery::is_fixture() || s.backend != Backend::Tmux {
        return None;
    }
    if !s.is_derived_name() {
        return None;
    }
    let current = s.tmux_session.as_deref().filter(|t| !t.is_empty())?;
    if is_generic_tmux_name(current, s.cwd.as_deref()) || current == "fleet" {
        return None;
    }
    let handle = s.handle.as_deref().filter(|h| !h.is_empty())?;
    let (windows, panes) = backend::tmux_shape(handle).ok()?;
    (windows == 1 && panes == 1).then(|| current.to_string())
}

/// The one Claude session a tmux session hosts, when it hosts exactly one and
/// nothing else (one window, one pane) — the case where the tmux name is the
/// title's to derive, so a rename of it should go through the title.
pub fn sole_claude_in(tmux_session: &str) -> Option<Session> {
    if crate::core::discovery::is_fixture() {
        return None;
    }
    let mut inside: Vec<Session> = crate::core::discovery::discover()
        .into_iter()
        .filter(|s| s.backend == Backend::Tmux && s.tmux_session.as_deref() == Some(tmux_session))
        .collect();
    if inside.len() != 1 {
        return None;
    }
    let s = inside.remove(0);
    let handle = s.handle.as_deref().filter(|h| !h.is_empty())?;
    let (windows, panes) = backend::tmux_shape(handle).ok()?;
    (windows == 1 && panes == 1).then_some(s)
}

// --- renaming ----------------------------------------------------------------

/// How one rename should be applied.
#[derive(Clone, Copy, Default)]
pub struct RenameOpts {
    /// Bring the session's tmux session name along.
    pub sync_tmux: bool,
    /// Send `/rename` even to a busy or waiting session. The hold exists because
    /// a live turn would read `/rename foo` as its answer; a session that is
    /// *always* busy would otherwise be unrenameable by any path, so the escape
    /// hatch is explicit rather than absent.
    pub force: bool,
    /// Hold also when the input box cannot be found or read ([`Hold::NoInputBox`],
    /// [`Hold::Unreadable`]), not only when it holds text or a prompt — as `send
    /// --require-empty`. By default a box fleet cannot see holds nothing: `/rename` is typed
    /// as before the box was read. `force` skips the read, and so this.
    pub require_empty: bool,
}

/// Why a rename was held.
///
/// Only a session **waiting on the user** is held: it is sitting on a permission
/// prompt or a question, where typed keys are its *answer* — `/rename foo` plus
/// Enter could pick an option. A **busy** session is not held: Claude Code runs
/// `/rename` as a local command the moment it is submitted, mid-turn, without
/// interrupting the turn (verified live: the numbers kept streaming, "Session
/// renamed to …" appeared inline, the turn finished normally; the model gets a
/// system reminder about the new name). An older Claude that queues input while
/// busy would run it after the turn instead — also harmless.
///
/// Nor is text already in the session's **input box** (a prompt Escape put back, a
/// draft): `/rename foo` typed after it would go out with it as one prompt — the
/// stopped prompt run again — so that holds too, read off the screen under the lock.
///
/// With `require_empty` a box fleet cannot find or read holds as well, under the reason codes
/// `send --json` gives it (`no_input_box`, `unreadable`). Text in the box and a prompt in its
/// place keep the codes `rename` always had (`input`, `waiting`), which `send` calls `text`
/// and `prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Hold {
    /// On a permission prompt / question: `/rename foo` would be its answer.
    #[serde(rename = "waiting")]
    Waiting,
    /// Mid-turn in a harness that would queue the text as a message (Codex, pi).
    #[serde(rename = "busy")]
    Busy,
    /// Text in the input box: `/rename foo` would be glued onto it and submit both.
    #[serde(rename = "input")]
    Input,
    /// `require_empty`, and no input box fleet knows is on screen (a menu, a panel such as
    /// `/usage`, the transcript view): it cannot tell the box is empty.
    #[serde(rename = "no_input_box")]
    NoInputBox,
    /// `require_empty`, and the box cannot be read: it shows text the screen's missing
    /// attributes cannot tell from a placeholder (iTerm), or Claude Code's shows nothing and
    /// the terminal's cursor, which tells blanks typed there, is hidden or not given.
    #[serde(rename = "unreadable")]
    Unreadable,
}

/// Would typing `/rename` into this session be unsafe right now?
pub fn hold(s: &Session) -> Option<Hold> {
    if s.is_waiting() {
        return Some(Hold::Waiting);
    }
    (s.harness != crate::core::harness::Harness::Claude && s.status == "busy").then_some(Hold::Busy)
}

impl Hold {
    /// What a read of the input box holds the rename for, if anything: text in the box, or a
    /// prompt in its place, always; a box not found or not readable only with `require_empty`
    /// (the same [`InputRead::holds`](crate::core::input::InputRead::holds) `send` asks).
    pub fn from_read(read: &crate::core::input::InputRead, require_empty: bool) -> Option<Hold> {
        use crate::core::input::InputRead;
        if !read.holds(require_empty) {
            return None;
        }
        match read {
            InputRead::Empty => None,
            InputRead::Typed(_) => Some(Hold::Input),
            InputRead::Prompt => Some(Hold::Waiting),
            InputRead::Unclear(_) => Some(Hold::Unreadable),
            InputRead::NoBox => Some(Hold::NoInputBox),
        }
    }

    pub fn message(self, s: &Session) -> String {
        match self {
            Hold::Waiting => format!(
                "{} is waiting on you — nothing sent (--force overrides)",
                s.headline()
            ),
            Hold::Busy => format!(
                "{} is busy — nothing sent, rename it once the turn is over (--force overrides)",
                s.headline()
            ),
            Hold::Input => format!(
                "{} has text in its input box — the rename would go out with it as one prompt; nothing sent, rename it once the box is clear (--force overrides)",
                s.headline()
            ),
            Hold::NoInputBox => format!(
                "fleet cannot find {}'s input box on screen (a menu, the transcript view, or a layout it does not know), so it cannot tell it is empty — nothing sent (--force overrides)",
                s.headline()
            ),
            Hold::Unreadable => format!(
                "fleet cannot tell whether {}'s input box is empty: this terminal gives no text attributes to tell a placeholder or suggestion from typed text, or the caret that shows blanks typed there cannot be read (the terminal's cursor not given, or hidden with no caret painted in the box) — nothing sent (--force overrides)",
                s.headline()
            ),
        }
    }
}

/// Does the screen end in a prompt — a permission prompt (whole or not) or a question form —
/// whose answer typed text would be?
pub fn shows_prompt(screen: &str, harness: crate::core::harness::Harness) -> bool {
    use crate::core::prompt::{self, PromptRead};
    !matches!(prompt::permission_prompt(screen, harness), PromptRead::None)
        || prompt::is_question_form(screen)
}

/// What [`apply_rename`] did.
#[derive(Debug)]
pub enum RenameOutcome {
    /// `/rename` was typed into the session. `None` when tmux sync was off.
    Sent(Option<TmuxSync>),
    /// Nothing was sent, and why.
    Held(Hold, String),
}

impl RenameOutcome {
    /// The one-line note about tmux, if there is one.
    pub fn tmux_note(&self) -> Option<String> {
        match self {
            RenameOutcome::Sent(Some(t)) => Some(t.to_string()),
            _ => None,
        }
    }
}

/// Drive Claude's own `/rename` for one session, then (optionally) bring its
/// tmux session name along.
///
/// The single choke point for every rename: `fleet rename`, `name --apply`,
/// `fleet tmux rename` of a single-Claude session, the dashboard's rename buffer
/// and the web UI (through `fleet rename --json`) all come through here, so the
/// "never type into a live turn" rule can't be forgotten in one of them.
///
/// It types under the session's lock (see [`crate::core::lock`]), like every other command
/// that types into a session: `/rename …` and its Enter never land between the keys of an
/// answer. And under the lock, unless forced, its input box is read off the screen
/// ([`crate::core::input`], as `send` reads it): a prompt that came up since the session was
/// looked up holds the rename as a waiting session would, and text in the box holds it as
/// [`Hold::Input`]. A box fleet cannot find or read holds nothing, as for `send` by default —
/// unless `require_empty`, which holds it as [`Hold::NoInputBox`] / [`Hold::Unreadable`], as
/// `send --require-empty` does.
pub fn apply_rename(s: &Session, title: &str, o: RenameOpts) -> Result<RenameOutcome> {
    if !o.force
        && let Some(h) = hold(s)
    {
        return Ok(RenameOutcome::Held(h, h.message(s)));
    }
    let lock = crate::core::lock::for_typing(s).map_err(|e| {
        let kind = match e {
            crate::core::lock::LockError::Busy => "busy",
            crate::core::lock::LockError::Io(_) => "io",
        };
        Error::tagged(kind, format!("{e}; nothing sent"))
    })?;
    if lock.is_some() {
        crate::core::lock::still_named(s, "nothing sent")?;
    }
    if !o.force
        && lock.is_some()
        && let Some(h) = Hold::from_read(&crate::core::input::of(s)?, o.require_empty)
    {
        return Ok(RenameOutcome::Held(h, h.message(s)));
    }
    backend::send(s, &s.harness.rename_command(title))?;
    drop(lock);
    if !o.sync_tmux {
        return Ok(RenameOutcome::Sent(None));
    }
    Ok(RenameOutcome::Sent(Some(
        match backend::rename_tmux_session(s, title) {
            Ok(t) => t,
            // The Claude rename already landed; a tmux failure is a footnote,
            // not a reason to report the whole thing as failed.
            Err(e) => TmuxSync::Skipped(format!("tmux not renamed: {e}")),
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, source: &str) -> Session {
        Session {
            pid: 7,
            name: Some(name.into()),
            name_source: Some(source.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_chosen_claude_name_is_the_title() {
        let mut s = named("fix-login", "user");
        s.gen_title = Some("login-flow-repair".into());
        s.title = Some("please fix the login".into());
        assert_eq!(display_title(&s), "fix-login");
    }

    #[test]
    fn a_derived_claude_name_yields_to_the_generated_title() {
        let mut s = named("app-9d", "derived");
        s.gen_title = Some("  cache-warmup  ".into());
        assert_eq!(display_title(&s), "cache-warmup");
        // No generated title: the first prompt, slugged.
        s.gen_title = None;
        s.title = Some("# Fix the flaky login test\nmore".into());
        assert_eq!(display_title(&s), "fix-the-flaky-login-test");
        // Nothing at all: Claude's derived name is still better than an id.
        s.title = None;
        assert_eq!(display_title(&s), "app-9d");
    }

    #[test]
    fn with_no_name_the_short_id_or_pid_stands_in() {
        let mut s = Session {
            pid: 42,
            ..Default::default()
        };
        assert_eq!(display_title(&s), "42");
        s.session_id = Some("0123456789abcdef".into());
        assert_eq!(display_title(&s), "01234567");
    }

    #[test]
    fn a_name_without_a_source_counts_as_chosen() {
        let s = Session {
            name: Some("docs-refresh".into()),
            gen_title: Some("other".into()),
            ..Default::default()
        };
        assert_eq!(display_title(&s), "docs-refresh");
    }

    #[test]
    fn titles_are_trimmed_validated_and_capped() {
        assert_eq!(clean_title("  cache-warmup  ").unwrap(), "cache-warmup");
        assert!(clean_title("   ").is_err());
        assert!(clean_title("two\nlines").is_err());
        assert_eq!(
            clean_title(&"a".repeat(300)).unwrap().chars().count(),
            MAX_TITLE
        );
        assert_eq!(
            clean_title(&"é".repeat(300)).unwrap().chars().count(),
            MAX_TITLE
        );
    }

    #[test]
    fn the_tmux_name_is_a_slug_of_the_title() {
        assert_eq!(tmux_name_for("Fix Login Flow"), "fix-login-flow");
        assert_eq!(tmux_name_for("api: v2.1 rollout"), "api-v2-1-rollout");
        assert_eq!(tmux_name_for("Popravak čišćenja"), "popravak-ciscenja");
        assert_eq!(tmux_name_for("Straße"), "strasse");
        assert_eq!(tmux_name_for("--leading"), "leading");
        assert_eq!(tmux_name_for("🚀🚀"), "");
        let long = tmux_name_for(&"word ".repeat(40));
        assert!(long.len() <= MAX_TMUX_SLUG, "{long}");
        assert!(!long.ends_with('-'), "{long}");
    }

    #[test]
    fn collision_suffixes_count_as_already_derived() {
        assert!(is_derived_from("fix-login", "fix-login"));
        assert!(is_derived_from("fix-login-2", "fix-login"));
        assert!(is_derived_from("fix-login-13", "fix-login"));
        assert!(!is_derived_from("fix-login-flow", "fix-login"));
        assert!(!is_derived_from("fix-login-", "fix-login"));
        assert!(!is_derived_from("fix", "fix-login"));
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_single_job_session_follows_the_title() {
        let plan = plan_tmux(
            "Fix Login",
            "fw-120000",
            None,
            1,
            1,
            &names(&["fw-120000", "x"]),
        );
        assert_eq!(
            plan,
            TmuxPlan::Rename {
                from: "fw-120000".into(),
                to: "fix-login".into()
            }
        );
    }

    #[test]
    fn a_taken_name_gets_the_next_free_suffix() {
        let existing = names(&["app-3", "fix-login", "fix-login-2"]);
        assert_eq!(
            plan_tmux("fix login", "app-3", None, 1, 1, &existing),
            TmuxPlan::Rename {
                from: "app-3".into(),
                to: "fix-login-3".into()
            }
        );
    }

    #[test]
    fn a_session_already_carrying_its_derived_name_is_left_alone() {
        // Exactly the name, or the name with a collision suffix from last time —
        // re-syncing must not walk `fix-login-2` to `fix-login-3`.
        let existing = names(&["fix-login", "fix-login-2"]);
        assert!(matches!(
            plan_tmux("fix-login", "fix-login-2", None, 1, 1, &existing),
            TmuxPlan::Skip(_)
        ));
        assert!(matches!(
            plan_tmux("Fix Login", "fix-login", None, 1, 1, &existing),
            TmuxPlan::Skip(_)
        ));
    }

    #[test]
    fn shared_or_protected_tmux_sessions_are_never_renamed() {
        let e = names(&["work"]);
        assert!(
            matches!(plan_tmux("x y", "work", None, 2, 2, &e), TmuxPlan::Skip(w) if w.contains("windows"))
        );
        assert!(matches!(
            plan_tmux("x y", "work", None, 1, 3, &e),
            TmuxPlan::Skip(_)
        ));
        assert!(matches!(
            plan_tmux("x y", "fleet", None, 1, 1, &e),
            TmuxPlan::Skip(_)
        ));
        assert!(matches!(
            plan_tmux("x y", "work", Some("work"), 1, 1, &e),
            TmuxPlan::Skip(_)
        ));
        assert!(matches!(
            plan_tmux("🚀", "work", None, 1, 1, &e),
            TmuxPlan::Skip(_)
        ));
    }

    #[test]
    fn generic_tmux_names_are_recognised() {
        assert!(is_generic_tmux_name("fw-120301", None));
        assert!(is_generic_tmux_name("3", None));
        assert!(is_generic_tmux_name("app-9d", Some("~/Code/app")));
        assert!(is_generic_tmux_name("App-9D", Some("~/Code/app/")));
        assert!(!is_generic_tmux_name("fix-login", Some("~/Code/app")));
        assert!(!is_generic_tmux_name("app-9z", Some("~/Code/app")));
        assert!(!is_generic_tmux_name("fw-12", None));
        assert!(!is_generic_tmux_name("", None));
    }

    // A Codex approval the screen cannot tell whole (no column-0 line over it, or no title) is
    // still a prompt the rename's text would answer: held, never taken for "no prompt".
    #[test]
    fn a_codex_approval_not_read_whole_holds_a_rename() {
        use crate::core::harness::Harness;
        let answers =
            "› 1. Yes, proceed (y)\n  2. No (esc)\n\n  Press enter to confirm or esc to cancel\n";
        let whole = format!(
            "• Running ls\n\n  Would you like to run the following command?\n\n  $ ls\n\n{answers}"
        );
        let topless =
            format!("  Would you like to run the following command?\n\n  $ ls\n\n{answers}");
        let untitled = format!("• Running ls\n\n  $ ls\n\n{answers}");
        for screen in [&whole, &topless, &untitled] {
            assert!(shows_prompt(screen, Harness::Codex), "{screen}");
        }
        assert!(!shows_prompt(
            "• Done\n\n› Ask Codex to do anything\n",
            Harness::Codex
        ));
    }

    #[test]
    fn only_a_waiting_session_is_held_unless_forced() {
        let mut s = named("auth-spike", "user");
        s.status = "waiting".into();
        assert_eq!(hold(&s), Some(Hold::Waiting));
        match apply_rename(&s, "x", RenameOpts::default()).unwrap() {
            RenameOutcome::Held(Hold::Waiting, why) => {
                assert!(
                    why.contains("waiting on you") && why.contains("--force"),
                    "{why}"
                )
            }
            other => panic!("a waiting session must not be typed into: {other:?}"),
        }
        // Busy is fine: Claude runs `/rename` mid-turn without disturbing it.
        for status in ["busy", "idle", "unknown"] {
            s.status = status.into();
            assert_eq!(hold(&s), None, "{status}");
        }
        // Forced, the hold is gone: this fixture has no handle, so the send
        // fails — what matters is that it got as far as trying.
        s.status = "waiting".into();
        let forced = apply_rename(
            &s,
            "x",
            RenameOpts {
                sync_tmux: false,
                force: true,
                require_empty: false,
            },
        );
        assert!(!matches!(forced, Ok(RenameOutcome::Held(..))));
    }

    #[test]
    fn non_tmux_sessions_never_adopt_a_tmux_name() {
        let mut s = named("app-9d", "derived");
        s.backend = Backend::Iterm;
        s.tmux_session = Some("fix-login".into());
        assert_eq!(adoptable_tmux_name(&s), None);
        // A chosen name is never replaced by the tmux name.
        let mut s = named("chosen", "user");
        s.backend = Backend::Tmux;
        s.tmux_session = Some("fix-login".into());
        assert_eq!(adoptable_tmux_name(&s), None);
    }

    /// A capture from testdata/screens, read as `fleet send` would read the session's box.
    fn read_of(name: &str, styled: bool) -> crate::core::input::InputRead {
        let path = format!(
            "{}/../../testdata/screens/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let screen = if styled {
            raw
        } else {
            // What iTerm gives: the text alone.
            raw.lines()
                .map(backend::strip_ansi)
                .collect::<Vec<_>>()
                .join("\n")
        };
        crate::core::input::read(&screen, crate::core::harness::Harness::Claude, styled)
    }

    fn from_screen(name: &str, styled: bool, require_empty: bool) -> Option<Hold> {
        Hold::from_read(&read_of(name, styled), require_empty)
    }

    #[test]
    fn an_input_box_fleet_cannot_find_holds_a_rename_only_when_an_empty_box_is_required() {
        // The transcript view stands for a panel such as `/usage`: no box on screen.
        assert_eq!(from_screen("claude-transcript", true, false), None);
        assert_eq!(
            from_screen("claude-transcript", true, true),
            Some(Hold::NoInputBox)
        );
    }

    #[test]
    fn an_input_box_fleet_cannot_read_holds_a_rename_only_when_an_empty_box_is_required() {
        // Without text attributes a placeholder cannot be told from typed text.
        assert_eq!(from_screen("claude-placeholder", false, false), None);
        assert_eq!(
            from_screen("claude-placeholder", false, true),
            Some(Hold::Unreadable)
        );
    }

    #[test]
    fn text_or_a_prompt_holds_a_rename_with_or_without_the_flag() {
        for require_empty in [false, true] {
            assert_eq!(
                from_screen("live/claude-restored", true, require_empty),
                Some(Hold::Input)
            );
            assert_eq!(
                from_screen("claude-permission", true, require_empty),
                Some(Hold::Waiting)
            );
        }
    }

    #[test]
    fn an_empty_box_holds_nothing_even_when_an_empty_box_is_required() {
        for require_empty in [false, true] {
            assert_eq!(from_screen("claude-placeholder", true, require_empty), None);
        }
    }

    #[test]
    fn a_hold_is_reported_under_the_reason_send_gives_it() {
        let reason = |h: Hold| serde_json::to_value(h).unwrap();
        // The new holds are `send`'s own reason codes (`InputRead::reason`).
        assert_eq!(reason(Hold::NoInputBox), "no_input_box");
        assert_eq!(reason(Hold::Unreadable), "unreadable");
        let sent = [
            (Hold::NoInputBox, read_of("claude-transcript", true)),
            (Hold::Unreadable, read_of("claude-placeholder", false)),
        ];
        for (h, read) in sent {
            assert_eq!(reason(h), read.reason().unwrap());
        }
        // The codes `rename` always had are unchanged.
        assert_eq!(reason(Hold::Waiting), "waiting");
        assert_eq!(reason(Hold::Busy), "busy");
        assert_eq!(reason(Hold::Input), "input");
    }

    #[test]
    fn the_new_holds_say_what_was_not_sent_and_never_what_the_box_holds() {
        let s = named("fix-login", "user");
        for h in [Hold::NoInputBox, Hold::Unreadable] {
            let m = h.message(&s);
            assert!(m.contains("nothing sent") && m.contains("--force"), "{m}");
        }
        let read = read_of("claude-placeholder", false);
        let text = read.text().unwrap().to_string();
        let m = Hold::Unreadable.message(&s);
        assert!(!m.contains(&text), "{m}");
    }
}
