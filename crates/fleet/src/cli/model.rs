//! `fleet model <target> <model-id>`: switch a Claude Code session to another model, as
//! `/model <id>` typed in its terminal does — the pick then also becomes Claude Code's default
//! for new sessions, as it does there — and answer the one question that can come back: its
//! confirmation while the prompt cache is warm. The checks, the lock, the wait and the
//! confirmation are [`switch`]'s, shared with `fleet effort`; what is `/model`'s own:
//!
//! - **Claude Code only**: Codex 0.163 opens a picker on `/model`, pi has none —
//!   `harness_unsupported`.
//! - The confirmation is "Switch model?" for that model ([`model::shown`]); work still under
//!   way where the box was ("Running PreModelSwitch hooks…") is waited for. "Model '…' not
//!   found" under the command is `model_not_found`, a `PreModelSwitch` hook's refusal
//!   `switch_refused`.
//! - **Success** is the transcript's `Set model to <label>` entry ([`model::Replies`]: one
//!   written as a prompt is the command's output only when it answers the entry naming
//!   `/model <id>`) written after the command began, its label positively the model asked for
//!   ([`model::same_model`]: the id itself, or the same model's label, aliases resolved through
//!   fleet's model list, `[1m]` and date suffixes the same model); its `timestamp` is
//!   `switched_at`. Any other label — another model's ([`model::names_other`]), or one fleet
//!   cannot match to the id (`Default (recommended)`, none) — is `outcome_uncertain`, and so is
//!   `Kept model as …` (the switch declined in the terminal: the model is unchanged).

use colored::Colorize;
use serde_json::json;

use crate::cli::close::Expect;
use crate::cli::commands::{clean_model, host_label};
use crate::cli::switch::{self, Command, Extra, Target};
use crate::core::hosts;
use crate::core::model::{self, Reply, Shown};
use crate::error::{Error, Result};

pub struct Opts {
    pub target: String,
    /// The model id, as given (checked by [`clean_model`]).
    pub model: String,
    pub json: bool,
    /// The process the caller means (`--expect-pid` + `--expect-start`, given together).
    pub expect: Option<Expect>,
}

/// `/model <id>`.
struct Model {
    id: String,
    replies: model::Replies,
}

impl Command for Model {
    type Done = ();
    const NAME: &'static str = "model";
    const SUCCESS: &'static str = "Set model to";

    fn value(&self) -> &str {
        &self.id
    }

    fn confirmation(&self) -> String {
        format!("Claude Code's switch confirmation for {}", self.id)
    }

    fn shown(&self, screen: &str, styled: bool) -> Shown {
        model::shown(screen, &self.id, styled)
    }

    fn drawn(&self, before: &str, now: &str) -> Option<Error> {
        let id = &self.id;
        if model::not_found(now, id) > model::not_found(before, id) {
            return Some(not_found_error(id));
        }
        (model::refused(now) > model::refused(before)).then(|| refused_error(id))
    }

    fn answer(&mut self, line: &str, typed: &str, confirmed: bool) -> Option<(String, Result<()>)> {
        let (reply, at) = self.replies.read(line)?;
        let id = &self.id;
        let r = match reply {
            Reply::Switched(Some(label)) if model::same_model(&label, id) => Ok(()),
            Reply::Switched(Some(label)) if model::names_other(&label, id) => Err(Error::tagged(
                "outcome_uncertain",
                format!(
                    "{typed}, but Claude Code set the model to {label}, not {id}{}: by fleet's Claude model list it switched to another model than the one asked for — or that list is out of date (see `fleet peek`)",
                    model::wanted(id)
                        .map(|l| format!(" ({l})"))
                        .unwrap_or_default()
                ),
            )),
            Reply::Switched(label) => Err(Error::tagged(
                "outcome_uncertain",
                format!(
                    "{typed}, and Claude Code set the model to {}, which fleet cannot match to {id}: whether it is the model asked for cannot be told (see `fleet peek`)",
                    label.as_deref().unwrap_or("a model it did not name")
                ),
            )),
            Reply::NotFound => Err(not_found_error(id)),
            Reply::Refused => Err(refused_error(id)),
            Reply::Kept(label) => Err(switch::kept_error(
                typed,
                "model",
                label.as_deref(),
                confirmed,
            )),
        };
        Some((at, r))
    }
}

pub fn run(o: Opts) -> Result<()> {
    let dry_run = hosts::dry_run();
    let mut extra = Extra::default();
    let done = match model_of(&o.model).and_then(|mut cmd| {
        let t = Target {
            target: &o.target,
            expect: o.expect.as_ref(),
        };
        switch::switch(&t, &mut cmd, dry_run, &mut extra).map(|d| (d, cmd.id))
    }) {
        Ok(d) => d,
        Err(e) => return switch::failed(e, o.json, extra),
    };
    let (done, id) = done;
    let s = &done.session;
    if o.json {
        let v = json!({
            "ok": true,
            "host": host_label(),
            "session_id": s.session_id,
            "pid": s.pid,
            "harness": s.harness.id(),
            "model": id,
            "confirmed": done.confirmed,
            "switched_at": done.switched_at,
            "dry_run": dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else if dry_run {
        println!(
            "{} ({}): would type /model {id}",
            s.headline().bold(),
            s.harness.id(),
        );
    } else {
        println!(
            "switched {} ({}) to {id}{}",
            s.headline().bold(),
            s.harness.id(),
            if done.confirmed {
                ", confirming Claude Code's prompt"
            } else {
                ""
            }
        );
    }
    Ok(())
}

/// The command for model id `raw`, checked as `spawn --model`'s ([`clean_model`]).
fn model_of(raw: &str) -> Result<Model> {
    let id = match clean_model(raw) {
        Ok(Some(m)) => m,
        Ok(None) => return Err(Error::tagged("usage", "no model id given")),
        Err(e) => return Err(Error::tagged("usage", e.to_string())),
    };
    Ok(Model {
        replies: model::Replies::to(&id),
        id,
    })
}

fn not_found_error(id: &str) -> Error {
    Error::tagged(
        "model_not_found",
        format!("Claude Code has no model \"{id}\" (Model '{id}' not found); nothing switched"),
    )
}

fn refused_error(id: &str) -> Error {
    Error::tagged(
        "switch_refused",
        format!("a PreModelSwitch hook refused the switch to {id}; nothing switched"),
    )
}
