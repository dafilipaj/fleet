//! `fleet effort <target> <level>`: switch a Claude Code session's effort level, as
//! `/effort <level>` typed in its terminal does, and answer the one question that can come
//! back: its confirmation while the prompt cache is warm. The checks, the lock, the wait and
//! the confirmation are [`switch`]'s, shared with `fleet model`; what is `/effort`'s own:
//!
//! - **The level** is checked for shape only ([`effort::valid_level`]: lowercase letters, at
//!   most 16) and typed as given; which levels a model has is `fleet harness list`'s
//!   (`efforts`), and Claude Code says what it makes of the word.
//! - **Claude Code only**: Codex 0.163 changes its effort in the second step of its `/model`
//!   picker, pi has none — `harness_unsupported`.
//! - The confirmation is "Change effort level?" for that very level ([`effort::shown`]).
//! - **Success** is the transcript's `Set effort level to <level> …` (or, for `auto`, `Effort
//!   level set to auto`) under the entry naming `/effort <level>` ([`effort::Replies`]), written
//!   after the command began, naming the level asked for; its `timestamp` is `switched_at`,
//!   and `saved` says whether Claude Code's default for new sessions on this model changed with
//!   it (`(saved as your default for new sessions)`, or `auto`, which clears the saved level —
//!   not `max`, for this session only). Another level, `Kept effort level as …` (declined in
//!   the terminal) and an answer in words fleet does not know (quoted) are
//!   `outcome_uncertain`.

use colored::Colorize;
use serde_json::json;

use crate::cli::close::Expect;
use crate::cli::commands::host_label;
use crate::cli::switch::{self, Command, Extra, Target};
use crate::core::effort::{self, Reply};
use crate::core::hosts;
use crate::core::slash::Shown;
use crate::error::{Error, Result};

pub struct Opts {
    pub target: String,
    /// The level, as given (checked by [`effort::valid_level`]).
    pub level: String,
    pub json: bool,
    /// The process the caller means (`--expect-pid` + `--expect-start`, given together).
    pub expect: Option<Expect>,
}

/// `/effort <level>`.
struct Effort {
    level: String,
    replies: effort::Replies,
}

/// The longest stretch of an answer fleet does not know that its message quotes.
const QUOTED: usize = 200;

impl Command for Effort {
    /// `saved`.
    type Done = bool;
    const NAME: &'static str = "effort";
    const SUCCESS: &'static str = "Set effort level to";

    fn value(&self) -> &str {
        &self.level
    }

    fn confirmation(&self) -> String {
        format!("Claude Code's confirmation of `/effort {}`", self.level)
    }

    fn shown(&self, screen: &str, styled: bool) -> Shown {
        effort::shown(screen, &self.level, styled)
    }

    fn answer(
        &mut self,
        line: &str,
        typed: &str,
        confirmed: bool,
    ) -> Option<(String, Result<bool>)> {
        let (reply, at) = self.replies.read(line)?;
        let want = &self.level;
        let r = match reply {
            Reply::Set { level, saved } if &level == want => Ok(saved),
            Reply::Set { level, .. } => Err(Error::tagged(
                "outcome_uncertain",
                format!(
                    "{typed}, but Claude Code set the effort level to {level}, not {want} (see `fleet peek`)"
                ),
            )),
            Reply::Kept(level) => Err(switch::kept_error(
                typed,
                "effort level",
                level.as_deref(),
                confirmed,
            )),
            Reply::Other(said) => {
                let mut quoted: String = said.chars().take(QUOTED).collect();
                if quoted.len() < said.len() {
                    quoted.push('…');
                }
                Err(Error::tagged(
                    "outcome_uncertain",
                    format!(
                        "{typed}, and Claude Code answered \"{quoted}\", which fleet does not read as a switch: whether the effort level changed cannot be told (see `fleet peek`)"
                    ),
                ))
            }
        };
        Some((at, r))
    }
}

pub fn run(o: Opts) -> Result<()> {
    let dry_run = hosts::dry_run();
    let mut extra = Extra::default();
    let level = o.level.trim();
    if !effort::valid_level(level) {
        let e = Error::tagged(
            "usage",
            format!(
                "invalid effort level \"{level}\" — lowercase letters only, at most 16 (`fleet harness list --json` lists each model's `efforts`)"
            ),
        );
        return switch::failed(e, o.json, extra);
    }
    let mut cmd = Effort {
        level: level.to_string(),
        replies: effort::Replies::to(level),
    };
    let t = Target {
        target: &o.target,
        expect: o.expect.as_ref(),
    };
    let done = match switch::switch(&t, &mut cmd, dry_run, &mut extra) {
        Ok(d) => d,
        Err(e) => return switch::failed(e, o.json, extra),
    };
    let s = &done.session;
    if o.json {
        let v = json!({
            "ok": true,
            "host": host_label(),
            "session_id": s.session_id,
            "pid": s.pid,
            "harness": s.harness.id(),
            "effort": level,
            "confirmed": done.confirmed,
            "saved": done.done,
            "switched_at": done.switched_at,
            "dry_run": dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else if dry_run {
        println!(
            "{} ({}): would type /effort {level}",
            s.headline().bold(),
            s.harness.id(),
        );
    } else {
        println!(
            "switched {} ({}) to {level} effort{}{}",
            s.headline().bold(),
            s.harness.id(),
            match (done.done == Some(true), level == effort::AUTO) {
                (true, true) => {
                    ", its saved level cleared: it runs at its model's default, as new sessions on it do"
                }
                (true, false) => ", saved as Claude Code's default for new sessions on its model",
                (false, _) => ", for this session only",
            },
            if done.confirmed {
                ", confirming Claude Code's prompt"
            } else {
                ""
            }
        );
    }
    Ok(())
}
