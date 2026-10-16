//! `fleet messages <target>`: a session's conversation from its transcript — prompts, replies,
//! tool calls and questions — with cursors for paging and cheap polling (see
//! [`crate::core::transcript`]).

use std::path::PathBuf;
use std::time::Instant;

use colored::Colorize;
use serde_json::{Value, json};

use crate::core::discovery::{self, Session, claude_home};
use crate::core::harness::{self, Harness};
use crate::core::transcript::{self, Cursor, Entry, Kind, Page, ReadError};
use crate::error::{Error, Result};

/// Entries a page holds when `--limit` is not given (no cursor, or `--before`).
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 500;

pub struct Opts {
    pub target: String,
    pub json: bool,
    pub after: Option<String>,
    pub before: Option<String>,
    pub limit: Option<String>,
}

/// A failure, with the `kind` `--json` reports.
#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub message: String,
}

fn fail<T>(kind: &'static str, message: impl Into<String>) -> std::result::Result<T, Failure> {
    Err(Failure {
        kind,
        message: message.into(),
    })
}

/// What a successful call found.
pub struct Found {
    pub session: Session,
    pub transcript: Option<PathBuf>,
    pub page: Page,
}

pub fn run(o: Opts) -> Result<()> {
    let started = Instant::now();
    match collect(&o) {
        Ok(found) => {
            crate::core::hosts::debug(&format!(
                "messages: read {} transcript bytes in {:?}",
                found.page.bytes_read,
                started.elapsed()
            ));
            if o.json {
                println!("{}", serde_json::to_string(&to_json(&found))?);
            } else {
                print_text(&found);
            }
            Ok(())
        }
        Err(f) => {
            if o.json {
                println!(
                    "{}",
                    json!({ "ok": false, "kind": f.kind, "error": f.message })
                );
            }
            Err(Error::exit(1, f.message))
        }
    }
}

fn parse_limit(raw: Option<&str>) -> std::result::Result<Option<usize>, Failure> {
    let Some(raw) = raw else { return Ok(None) };
    match raw.trim().parse::<usize>() {
        Ok(n) if (1..=MAX_LIMIT).contains(&n) => Ok(Some(n)),
        _ => fail(
            "usage",
            format!("--limit must be a number from 1 to {MAX_LIMIT}, not {raw:?}"),
        ),
    }
}

fn parse_cursor(raw: &str) -> std::result::Result<Cursor, Failure> {
    Cursor::parse(raw).map_err(|message| Failure {
        kind: "cursor_invalid",
        message,
    })
}

/// Where a session's transcript is, `Ok(None)` when it has not written one yet.
///
/// Claude: `<claude home>/projects/<cwd>/<id>.jsonl`, or whichever project dir holds it once
/// the session moved (worktree, `cd`). Codex / pi: the file discovery paired with the process,
/// else the one named by the session id wherever it lives.
pub fn locate(s: &Session) -> std::result::Result<Option<PathBuf>, Failure> {
    let id = s.session_id.as_deref().filter(|id| !id.is_empty());
    match s.harness {
        Harness::Claude => {
            let Some(id) = id else {
                return fail(
                    "transcript_not_found",
                    format!(
                        "{} has no session id to find its transcript by",
                        s.headline()
                    ),
                );
            };
            Ok(crate::core::context::locate_transcript(
                &claude_home(),
                s.cwd.as_deref(),
                id,
            ))
        }
        Harness::Codex | Harness::Pi => {
            if let Some(t) = s.transcript.as_deref().map(PathBuf::from)
                && t.is_file()
            {
                return Ok(Some(t));
            }
            let root = if s.harness == Harness::Codex {
                harness::codex_home()
            } else {
                harness::pi_home()
            };
            Ok(id.and_then(|id| harness::find_transcript(s.harness, &root, id)))
        }
    }
}

pub fn collect(o: &Opts) -> std::result::Result<Found, Failure> {
    if o.after.is_some() && o.before.is_some() {
        return fail("usage", "--after and --before cannot be used together");
    }
    let limit = parse_limit(o.limit.as_deref())?;
    let after = o.after.as_deref().map(parse_cursor).transpose()?;
    let before = o.before.as_deref().map(parse_cursor).transpose()?;
    let session = discovery::resolve_target(&o.target).map_err(|e| Failure {
        kind: e.kind(),
        message: e.to_string(),
    })?;
    let transcript = locate(&session)?;
    let Some(path) = transcript.clone() else {
        // Nothing written yet (a fresh session): an empty conversation, unless the caller
        // holds a cursor into a transcript that is gone now.
        if after.iter().chain(&before).any(|c| !c.is_empty_start()) {
            return fail(
                "cursor_invalid",
                "the session's transcript is gone — reload without a cursor",
            );
        }
        return Ok(Found {
            session,
            transcript: None,
            page: Page {
                // No entries: no cursor, unless polling with one (it stays valid).
                cursor: after.as_ref().map(|_| Cursor::empty().encode()),
                ..Page::default()
            },
        });
    };
    let h = session.harness;
    let page = match (after, before) {
        (Some(c), _) => transcript::after(&path, h, &c, limit),
        (_, Some(c)) => transcript::before(&path, h, &c, limit.unwrap_or(DEFAULT_LIMIT)),
        _ => transcript::latest(&path, h, limit.unwrap_or(DEFAULT_LIMIT)).map_err(ReadError::Io),
    }
    .map_err(|e| match e {
        ReadError::CursorInvalid(m) => Failure {
            kind: "cursor_invalid",
            message: m,
        },
        ReadError::Io(e) => Failure {
            kind: "io",
            message: format!("cannot read {}: {e}", path.display()),
        },
    })?;
    Ok(Found {
        session,
        transcript,
        page,
    })
}

pub fn to_json(f: &Found) -> Value {
    json!({
        "ok": true,
        "session_id": f.session.session_id,
        "harness": f.session.harness,
        "transcript": f.transcript.as_ref().map(|p| p.display().to_string()),
        "entries": f.page.entries,
        "updated": f.page.updated,
        "has_more_before": f.page.has_more_before,
        "has_more_after": f.page.has_more_after,
        "cursor": f.page.cursor,
    })
}

fn first_line(s: &str) -> (&str, bool) {
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("").trim();
    (first, lines.next().is_some())
}

fn print_text(f: &Found) {
    if f.transcript.is_none() {
        println!("{}", "(no transcript yet)".dimmed());
        return;
    }
    if f.page.has_more_before {
        println!("{}", "… earlier messages: --before <cursor>".dimmed());
    }
    let agent = f.session.harness.id();
    for e in &f.page.entries {
        println!("{}", text_line(e, agent));
    }
}

fn text_line(e: &Entry, agent: &str) -> String {
    let time =
        e.ts.as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "     ".into());
    let (who, body) = match e.kind {
        Kind::User => ("you".to_string(), e.text.as_str()),
        Kind::Assistant => (agent.to_string(), e.text.as_str()),
        Kind::Command => ("cmd".into(), e.text.as_str()),
        Kind::System => ("note".into(), e.text.as_str()),
        Kind::ToolCall => ("tool".into(), e.text.as_str()),
        Kind::Question => ("asks".into(), e.text.as_str()),
    };
    let (line, more) = first_line(body);
    let mut out = format!("{} {:<6} {}", time.dimmed(), who.bold(), line);
    if more {
        out.push_str(&" …".dimmed().to_string());
    }
    if let Some(s) = e.status {
        out.push_str(&format!(" [{s}]").dimmed().to_string());
    }
    if e.queued {
        out.push_str(&" [queued]".dimmed().to_string());
    }
    out
}
