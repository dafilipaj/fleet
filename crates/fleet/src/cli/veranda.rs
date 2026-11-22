//! `fleet veranda`: Veranda links (opt-in) — for a while, a session talks with the guests in one
//! room of Veranda, a separate self-hosted group chat for people who are not on the tailnet.
//!
//! The bridge lives in the web server of the `veranda.host` machine (`web/lib/veranda.mjs`, it
//! talks to Veranda's bot API over a unix socket); these commands are its client: they ask this
//! machine's web server, which proxies to that host. Inside a session, the commands name it by
//! `$TMUX_PANE`: `reply` and `history` act on that session's own link only, and a linked session
//! may not `link` or `extend` (a guest could ask it to).

use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::slack::{request, request_within};
use crate::core::discovery;
use crate::error::{Error, Result};

fn ok_or_err(status: &str, v: &Value) -> Result<()> {
    if status.starts_with('2') {
        return Ok(());
    }
    let err = v["error"].as_str().unwrap_or("");
    if status == "404" && err == "not found" {
        return Err(Error::Other(
            "this machine's web server has no Veranda API — update it (fleet install / restart it)"
                .into(),
        ));
    }
    // 410: the link has ended — a linked session stops answering on exit 3.
    let code = if status == "410" { 3 } else { 1 };
    Err(Error::exit(
        code,
        if err.is_empty() {
            format!("web server answered {status}")
        } else {
            err.to_string()
        },
    ))
}

/// `18:00` today, `Wed 28 Oct 18:00` another day.
fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| {
            let t = t.with_timezone(&chrono::Local);
            let fmt = if t.date_naive() == chrono::Local::now().date_naive() {
                "%H:%M"
            } else {
                "%a %-d %b %H:%M"
            };
            t.format(fmt).to_string()
        })
        .unwrap_or_else(|| "?".into())
}

fn link_line(l: &Value) -> String {
    let who = l["sessionName"]
        .as_str()
        .or(l["session"].as_str())
        .unwrap_or("?");
    let until = l["until"].as_i64().map(clock).unwrap_or_else(|| "?".into());
    let note = l["note"].as_str().filter(|t| !t.is_empty());
    format!(
        "{}  room {}  ↔ {}  until {}  ({} in · {} out{}){}",
        l["id"].as_str().unwrap_or("?").bold(),
        l["room"].as_str().unwrap_or("?").bold(),
        who,
        until,
        l["delivered"].as_u64().unwrap_or(0),
        l["replies"].as_u64().unwrap_or(0),
        match l["pending"].as_u64() {
            Some(n) if n > 0 => format!(" · {n} waiting"),
            _ => String::new(),
        },
        note.map(|t| format!("\n            note: {t}"))
            .unwrap_or_default()
    )
}

/// This pane, when the command runs inside a tmux session.
fn own_pane() -> Option<String> {
    crate::core::done::own_pane_from_env(None)
}

/// `fleet veranda [status]`.
pub fn status(json: bool) -> Result<()> {
    let (status, v) = request("GET", "/api/veranda", None)?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if v["configured"].as_bool() == Some(false) {
        println!("Veranda links are not set up (no veranda section in the config)");
        return Ok(());
    }
    if v["enabled"].as_bool() != Some(true) {
        let why = v["error"]
            .as_str()
            .unwrap_or("no host runs them — set veranda.host in the config");
        println!("Veranda links are off: {why}");
        return Ok(());
    }
    let health = match v["error"].as_str() {
        Some(e) => format!("{} ({e})", "last poll failed".red()),
        None => "ok".green().to_string(),
    };
    println!(
        "Veranda bridge on {}: {health}{}",
        v["host"].as_str().unwrap_or("?"),
        v["url"]
            .as_str()
            .map(|u| format!("  {u}"))
            .unwrap_or_default()
    );
    let links = v["links"].as_array().cloned().unwrap_or_default();
    if links.is_empty() {
        println!("no active links");
    }
    for l in &links {
        println!("{}", link_line(l));
    }
    Ok(())
}

/// `fleet veranda link [<session>] --room <name> [--for <duration>] [--title …] [--note …]`.
pub fn link(
    session: Option<&str>,
    room: &str,
    duration: Option<&str>,
    title: Option<&str>,
    note: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut body = json!({ "room": room });
    if let Some(d) = duration {
        body["minutes"] = json!(d);
    }
    if let Some(t) = title {
        body["title"] = json!(t);
    }
    if let Some(n) = note {
        body["note"] = json!(n);
    }
    let pane = own_pane();
    match session {
        Some(q) => {
            let s = discovery::resolve(q).map_err(Error::Other)?;
            let id = s
                .session_id
                .clone()
                .ok_or_else(|| Error::Other(format!("{} has no session id yet", s.headline())))?;
            body["session"] = json!(id);
        }
        None => {
            let p = pane.clone().ok_or_else(|| {
                Error::Other(
                    "not inside a tmux session — name the session to link: fleet veranda link <session> --room …".into(),
                )
            })?;
            body["pane"] = json!(p);
        }
    }
    if let Some(p) = pane {
        body["callerPane"] = json!(p);
    }
    body["host"] = json!(crate::core::config::get().self_name());
    let (status, v) = request("POST", "/api/veranda/link", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("linked: {}", link_line(&v));
    if let Some(u) = v["url"].as_str() {
        println!("  url:        {u}");
    }
    println!(
        "  passphrase: {}   (shown once — pass it on to the guests)",
        v["passphrase"].as_str().unwrap_or("?").bold()
    );
    Ok(())
}

/// Which link a command names: `--room`, else the session it runs in (by its pane).
fn target(room: Option<&str>) -> Result<Value> {
    let mut body = json!({});
    let pane = own_pane();
    match room {
        Some(r) => body["room"] = json!(r),
        None => {
            body["pane"] = json!(pane.clone().ok_or_else(|| {
                Error::Other("not inside a linked session — name the room: --room <name>".into())
            })?);
        }
    }
    if let Some(p) = pane {
        body["callerPane"] = json!(p);
    }
    Ok(body)
}

/// `fleet veranda unlink [--room <name>]`: close the room and end the link.
pub fn unlink(room: Option<&str>, json: bool) -> Result<()> {
    let (status, v) = request("POST", "/api/veranda/unlink", Some(&target(room)?))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "unlinked room {} (closed)",
            v["room"].as_str().unwrap_or("?").bold()
        );
    }
    Ok(())
}

/// `fleet veranda extend --for <duration> [--room <name>]`: the link lasts that much longer.
pub fn extend(duration: &str, room: Option<&str>, json: bool) -> Result<()> {
    let mut body = target(room)?;
    body["minutes"] = json!(duration);
    let (status, v) = request("POST", "/api/veranda/extend", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("extended: {}", link_line(&v));
    }
    Ok(())
}

/// The body of a call a linked session makes about its own link.
fn own_body() -> Result<Value> {
    let pane = own_pane().ok_or_else(|| {
        Error::Other("run it inside the linked session (no $TMUX_PANE here)".into())
    })?;
    Ok(json!({ "pane": pane }))
}

/// `fleet veranda reply <markdown>`: post in this session's room.
pub fn reply(text: &str, json: bool) -> Result<()> {
    let mut body = own_body()?;
    body["text"] = json!(text);
    let (status, v) = request("POST", "/api/veranda/reply", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!(
            "posted in Veranda (room {})",
            v["room"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// `fleet veranda history [--limit N] [--after <id>]`: this session's room since it was linked.
pub fn history(limit: Option<u32>, after: Option<u64>, json: bool) -> Result<()> {
    let mut body = own_body()?;
    if let Some(n) = limit {
        body["limit"] = json!(n);
    }
    if let Some(a) = after {
        body["after"] = json!(a);
    }
    let (status, v) = request("POST", "/api/veranda/history", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    if msgs.is_empty() {
        println!("no messages");
    }
    for m in &msgs {
        println!("{}\n", message_text(m));
    }
    match v["next"].as_u64() {
        Some(n) => println!("more: fleet veranda history --after {n}"),
        None => println!("(no more)"),
    }
    Ok(())
}

fn message_text(m: &Value) -> String {
    let who = match m["kind"].as_str() {
        Some("agent") => "[you]".to_string(),
        Some("system") => "[system]".to_string(),
        _ => {
            let name = m["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("guest");
            match m["username"]
                .as_str()
                .filter(|u| !u.is_empty() && *u != name)
            {
                Some(u) => format!("{name} (@{u})"),
                None => name.to_string(),
            }
        }
    };
    let when = m["createdAt"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| clock(t.timestamp_millis()))
        .unwrap_or_else(|| "?".into());
    format!(
        "• {who} at {when} · id {}\n{}",
        m["id"],
        m["text"].as_str().unwrap_or("")
    )
}

/// Why a Veranda-linked session may not ask with AskUserQuestion.
const GUARD_REASON: &str = "This session is linked to the Veranda room <room>, where an AskUserQuestion prompt cannot be shown or answered — the session would block on it. Ask in the room instead: `fleet veranda reply \"…\"` (offer the options as a short list), then end your turn and wait for the answer.";

/// The PreToolUse hook's answer for one tool call, given the Veranda bridge's links: a deny for
/// AskUserQuestion in a linked session, else nothing.
pub(crate) fn guard_decision(
    input: &Value,
    links: impl FnOnce() -> Option<Value>,
    now_ms: i64,
) -> Option<Value> {
    if input["tool_name"].as_str() != Some("AskUserQuestion") {
        return None;
    }
    let session = input["session_id"].as_str().filter(|s| !s.is_empty())?;
    let v = links()?;
    let link = v["links"].as_array()?.iter().find(|l| {
        l["session"].as_str() == Some(session) && l["until"].as_i64().is_none_or(|u| u > now_ms)
    })?;
    let reason = GUARD_REASON.replace("<room>", link["room"].as_str().unwrap_or("?"));
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
}

/// The Veranda bridge's state for the guard — only when the config has a `veranda` section, so a
/// fleet without it makes no extra request.
pub(crate) fn guard_links() -> Option<Value> {
    if !crate::core::config::get().veranda.is_object() {
        return None;
    }
    let (status, v) = request_within("GET", "/api/veranda", None, 4).ok()?;
    status.starts_with('2').then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_denies_ask_user_question_only_in_a_veranda_linked_session() {
        let links = || {
            Some(json!({ "links": [
                { "id": "vl-aaaaaa", "room": "demo", "session": "s-linked", "until": 2000 },
                { "id": "vl-cccccc", "room": "old", "session": "s-expired", "until": 500 },
            ] }))
        };
        let ask = |sid: &str| json!({ "tool_name": "AskUserQuestion", "session_id": sid });
        let out = guard_decision(&ask("s-linked"), links, 1000).unwrap();
        let o = &out["hookSpecificOutput"];
        assert_eq!(o["permissionDecision"], "deny");
        let why = o["permissionDecisionReason"].as_str().unwrap();
        assert!(why.contains("Veranda room demo"), "{why}");
        assert!(why.contains("fleet veranda reply"), "{why}");
        assert!(guard_decision(&ask("s-expired"), links, 1000).is_none());
        assert!(guard_decision(&ask("s-other"), links, 1000).is_none());
        let bash = json!({ "tool_name": "Bash", "session_id": "s-linked" });
        assert!(guard_decision(&bash, links, 1000).is_none());
        assert!(guard_decision(&ask("s-linked"), || None, 1000).is_none());
    }

    #[test]
    fn a_history_message_reads_with_who_when_and_id() {
        let m = json!({ "id": 7, "kind": "guest", "name": "Ana", "username": "ana", "text": "hi", "createdAt": "2026-01-01T10:00:00Z" });
        let t = message_text(&m);
        assert!(t.starts_with("• Ana (@ana) at "), "{t}");
        assert!(t.ends_with("· id 7\nhi"), "{t}");
        let a = json!({ "id": 8, "kind": "agent", "text": "ok" });
        assert!(message_text(&a).starts_with("• [you] at ?"));
    }
}
