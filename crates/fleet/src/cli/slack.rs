//! `fleet slack`: Slack links — for a while, a session talks with the people in one Slack thread.
//!
//! The bridge lives in the web server of the `slack.host` machine (`web/lib/slack.mjs`, it holds
//! the Slack tokens); these commands are its client: they ask this machine's web server, which
//! proxies to that host. Inside a linked session, `reply` names the session by `$TMUX_PANE`, so
//! the bridge can refuse a reply on a link that belongs to another session.

use std::time::Duration;

use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::brief::local_web_base;
use crate::core::{discovery, hosts};
use crate::error::{Error, Result};

/// `curl` to this machine's web server: `(status, body)`. No HTTP client in the crate (see
/// `brief::regenerate_command`).
fn request(method: &str, path: &str, body: Option<&Value>) -> Result<(String, Value)> {
    request_within(method, path, body, 40)
}

/// `request` with its own time limit (a file comes back whole).
fn request_within(
    method: &str,
    path: &str,
    body: Option<&Value>,
    secs: u64,
) -> Result<(String, Value)> {
    let base = local_web_base();
    let url = format!("{base}{path}");
    let mut c = std::process::Command::new("curl");
    c.args([
        "-sS",
        "--max-time",
        &secs.to_string(),
        "-X",
        method,
        "-w",
        "\n%{http_code}",
    ]);
    if body.is_some() {
        c.args([
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    c.arg(&url);
    if hosts::dry_run() {
        println!("{}", hosts::display_command(&c));
        return Ok(("200".into(), Value::Null));
    }
    let input = body.map(|b| b.to_string());
    let got = hosts::capture_input(
        c,
        input.as_deref().map(str::as_bytes),
        Duration::from_secs(secs + 5),
    )
    .map_err(|e| Error::Other(format!("{e} — curl is needed for `fleet slack`")))?;
    let unreachable = || {
        Error::Other(format!(
            "cannot reach the web server at {base} — Slack links run in it (start it: fleet web serve)"
        ))
    };
    if got.code != Some(0) {
        return Err(unreachable());
    }
    let (text, status) = got
        .stdout
        .trim_end()
        .rsplit_once('\n')
        .map(|(b, s)| (b.to_string(), s.trim().to_string()))
        .unwrap_or_else(|| (String::new(), got.stdout.trim().to_string()));
    if status == "000" {
        return Err(unreachable());
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, v))
}

fn ok_or_err(status: &str, v: &Value) -> Result<()> {
    if status.starts_with('2') {
        return Ok(());
    }
    let err = v["error"].as_str().unwrap_or("");
    if status == "404" && err == "not found" {
        return Err(Error::Other(
            "this machine's web server has no Slack API — update it (fleet install / restart it)"
                .into(),
        ));
    }
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

/// `18:00` today, `Wed 28 Oct 18:00` another day (a link lasts up to a week, or has no end).
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
    let until = l["until"]
        .as_i64()
        .map(clock)
        .unwrap_or_else(|| "unlinked".into());
    let task = l["task"].as_str().filter(|t| !t.is_empty());
    let place = match l["dm"]["name"].as_str() {
        Some(name) => format!("DM with {name}"),
        None => l["permalink"].as_str().unwrap_or("").to_string(),
    };
    format!(
        "{}  {}  until {}  {}  {}{}",
        l["id"].as_str().unwrap_or("?").bold(),
        who,
        until,
        l["mode"].as_str().unwrap_or("mention"),
        place,
        task.map(|t| format!("\n            task: {t}"))
            .unwrap_or_default()
    )
}

/// `fleet slack [list]`.
pub fn list(json: bool) -> Result<()> {
    let (status, v) = request("GET", "/api/slack", None)?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if v["enabled"].as_bool() != Some(true) {
        let why = v["error"]
            .as_str()
            .unwrap_or("no host runs them — set slack.host in the config");
        println!("Slack links are off: {why}");
        return Ok(());
    }
    let socket = if v["socket"]["connected"].as_bool() == Some(true) {
        "connected".green().to_string()
    } else {
        format!(
            "{} ({})",
            "not connected".red(),
            v["socket"]["error"].as_str().unwrap_or("starting")
        )
    };
    println!(
        "Slack bridge on {}: {socket}",
        v["host"].as_str().unwrap_or("?")
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

/// `fleet slack link <url | @person> --for <duration> [--session <target>] [--task <text>]` — a
/// person is resolved by the bridge (the app's DM with them).
pub fn link(
    url: &str,
    duration: &str,
    session: Option<&str>,
    task: Option<&str>,
    proactive: bool,
    json: bool,
) -> Result<()> {
    let mode = if proactive { "proactive" } else { "mention" };
    let mut body = json!({ "url": url, "minutes": duration, "mode": mode });
    if let Some(t) = task {
        body["task"] = json!(t);
    }
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
            let pane = crate::core::done::own_pane_from_env(None).ok_or_else(|| {
                Error::Other(
                    "not inside a tmux session — name the session to link: --session <target>"
                        .into(),
                )
            })?;
            body["pane"] = json!(pane);
        }
    }
    body["host"] = json!(crate::core::config::get().self_name());
    let (status, v) = request("POST", "/api/slack/link", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let verb = if v["renewed"].as_bool() == Some(true) {
            "renewed"
        } else {
            "linked"
        };
        println!("{verb}: {}", link_line(&v));
    }
    Ok(())
}

/// `fleet slack unlink <link>`.
pub fn unlink(id: &str, json: bool) -> Result<()> {
    let (status, v) = request(
        "POST",
        &format!(
            "/api/slack/links/{}/unlink",
            crate::cli::brief::path_segment(id)
        ),
        Some(&json!({})),
    )?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("unlinked {}", id.bold());
    }
    Ok(())
}

/// `fleet slack reply <link> <text> [--thread <ts>]`: post in the linked thread.
pub fn reply(id: &str, text: &str, thread: Option<&str>, json: bool) -> Result<()> {
    let mut body = json!({ "link": id, "text": text });
    if let Some(t) = thread {
        body["thread"] = json!(t);
    }
    if let Some(pane) = crate::core::done::own_pane_from_env(None) {
        body["pane"] = json!(pane);
    }
    let (status, v) = request("POST", "/api/slack/reply", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("posted in Slack ({id})");
    }
    Ok(())
}

/// The body of a read: the link, and this session's pane (the bridge refuses another session's link).
fn read_body(id: &str) -> Value {
    let mut body = json!({ "link": id });
    if let Some(pane) = crate::core::done::own_pane_from_env(None) {
        body["pane"] = json!(pane);
    }
    body
}

/// `1792777175.569809` → `Wed 28 Oct 18:00` (or `18:00` today).
fn ts_clock(ts: &str) -> String {
    ts.parse::<f64>()
        .map(|s| clock((s * 1000.0) as i64))
        .unwrap_or_else(|_| "?".into())
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / 1024.0 / 1024.0),
        n if n >= 1024 => format!("{} KB", n / 1024),
        n => format!("{n} B"),
    }
}

/// One message of a read, as the session reads it: who (name, id, owner / you / app), when, its
/// ts, its thread, the text, its files.
fn message_text(m: &Value, in_thread: bool) -> String {
    let ts = m["ts"].as_str().unwrap_or("?");
    let mut who = m["name"].as_str().unwrap_or("someone").to_string();
    if let Some(u) = m["user"].as_str() {
        who.push_str(&format!(" ({u}"));
        if m["owner"].as_bool() == Some(true) {
            who.push_str(", the owner of this session");
        }
        who.push(')');
    }
    if m["you"].as_bool() == Some(true) {
        who.push_str(" [you]");
    } else if m["bot"].as_bool() == Some(true) {
        who.push_str(" [app]");
    }
    let mut head = format!("• {who} at {} · ts {ts}", ts_clock(ts));
    let replies = m["replies"].as_u64().unwrap_or(0);
    if !in_thread && replies > 0 {
        head.push_str(&format!(
            " · {replies} repl{} (fleet slack thread … {ts})",
            if replies == 1 { "y" } else { "ies" }
        ));
    }
    if !in_thread && let Some(t) = m["threadTs"].as_str().filter(|t| *t != ts) {
        head.push_str(&format!(" · in thread {t}"));
    }
    if let Some(st) = m["subtype"].as_str().filter(|s| *s != "file_share") {
        head.push_str(&format!(" · {st}"));
    }
    let mut out = format!("{head}\n{}", m["text"].as_str().unwrap_or(""));
    for f in m["files"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n  [file {} · {}{}{}]",
            f["id"].as_str().unwrap_or("?"),
            f["name"].as_str().unwrap_or("?"),
            f["mimetype"]
                .as_str()
                .map(|t| format!(" · {t}"))
                .unwrap_or_default(),
            f["size"]
                .as_u64()
                .map(|n| format!(" · {}", human_size(n)))
                .unwrap_or_default()
        ));
    }
    out
}

/// A page of messages, oldest first, and how to get the next one.
fn print_page(v: &Value, next: &str) {
    let in_thread = v["order"].as_str() == Some("oldest first");
    let mut msgs = v["messages"].as_array().cloned().unwrap_or_default();
    if !in_thread {
        msgs.reverse(); // Slack pages a channel newest first; read it top-down
    }
    if msgs.is_empty() {
        println!("no messages");
    }
    for m in &msgs {
        println!("{}\n", message_text(m, in_thread));
    }
    // Both page back in time: a channel to older messages, a thread to earlier replies (Slack
    // starts a thread at its latest replies and repeats its first message on every page).
    match v["nextCursor"].as_str().filter(|c| !c.is_empty()) {
        Some(c) => println!("older: {next} --cursor {c}"),
        None => println!("(no more)"),
    }
}

/// `fleet slack history <link>`: the linked conversation, a page at a time (a thread link: its thread).
pub fn history(
    id: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
    oldest: Option<&str>,
    latest: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut body = read_body(id);
    if let Some(n) = limit {
        body["limit"] = json!(n);
    }
    for (k, v) in [("cursor", cursor), ("oldest", oldest), ("latest", latest)] {
        if let Some(v) = v {
            body[k] = json!(v);
        }
    }
    let (status, v) = request("POST", "/api/slack/history", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print_page(&v, &format!("fleet slack history {id}"));
    }
    Ok(())
}

/// `fleet slack thread <link> <ts>`: one thread of the linked conversation.
pub fn thread(
    id: &str,
    ts: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut body = read_body(id);
    body["ts"] = json!(ts);
    if let Some(n) = limit {
        body["limit"] = json!(n);
    }
    if let Some(c) = cursor {
        body["cursor"] = json!(c);
    }
    let (status, v) = request("POST", "/api/slack/thread", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print_page(&v, &format!("fleet slack thread {id} {ts}"));
    }
    Ok(())
}

/// A file name safe to write: no path, no control characters.
fn safe_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let n = n.trim_start_matches('.').trim();
    if n.is_empty() {
        "file".into()
    } else {
        n.into()
    }
}

/// Standard base64 (what the bridge sends) → bytes.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `fleet slack file <link> <file-id> [-o <path>]`: a file shared in the linked conversation, saved
/// here (default: a temp dir) — the path is printed.
pub fn file(id: &str, file_id: &str, out: Option<&str>, json: bool) -> Result<()> {
    let mut body = read_body(id);
    body["file"] = json!(file_id);
    let (status, v) = request_within("POST", "/api/slack/file", Some(&body), 120)?;
    ok_or_err(&status, &v)?;
    if hosts::dry_run() {
        return Ok(());
    }
    let bytes = v["data"]
        .as_str()
        .and_then(base64_decode)
        .ok_or_else(|| Error::Other("the web server sent no file".into()))?;
    let name = safe_name(v["name"].as_str().unwrap_or(file_id));
    let path = match out {
        Some(o) if std::path::Path::new(o).is_dir() => std::path::Path::new(o).join(&name),
        Some(o) => std::path::PathBuf::from(o),
        None => {
            let dir = std::env::temp_dir().join("fleet-slack");
            std::fs::create_dir_all(&dir)?;
            dir.join(format!("{file_id}-{name}"))
        }
    };
    std::fs::write(&path, &bytes)?;
    if json {
        let mut meta = v.clone();
        meta.as_object_mut().map(|o| o.remove("data"));
        meta["path"] = json!(path.display().to_string());
        println!("{}", serde_json::to_string_pretty(&meta)?);
    } else {
        println!(
            "{} ({}, {})",
            path.display(),
            v["mimetype"].as_str().unwrap_or("file"),
            human_size(bytes.len() as u64)
        );
    }
    Ok(())
}

/// `fleet spawn --slack <url> --slack-for <duration>`: the link a new session starts with.
pub struct SpawnLink {
    pub url: String,
    pub duration: String,
    pub task: Option<String>,
    pub proactive: bool,
}

/// Before a linked spawn opens anything: this machine runs the bridge (only its own sessions
/// can be linked).
pub fn check_linkable() -> Result<()> {
    let (status, v) = request("GET", "/api/slack", None)?;
    ok_or_err(&status, &v)?;
    let me = crate::core::config::get().self_name();
    match v["host"].as_str() {
        _ if v["enabled"].as_bool() != Some(true) => Err(Error::Other(format!(
            "Slack links are off: {}",
            v["error"]
                .as_str()
                .unwrap_or("set slack.host in the config")
        ))),
        Some(h) if h != me => Err(Error::Other(format!(
            "only sessions on {h} can be linked to Slack — spawn there: fleet -H {h} spawn …"
        ))),
        _ => Ok(()),
    }
}

/// Link the session just started in tmux session `tmux`, retrying until it has registered with a
/// session id; `first` (its prompt) goes in with the link's intro.
pub fn link_spawned(tmux: &str, link: &SpawnLink, first: &str) -> Result<()> {
    let mode = if link.proactive {
        "proactive"
    } else {
        "mention"
    };
    let mut body = json!({
        "url": link.url,
        "minutes": link.duration,
        "mode": mode,
        "tmuxSession": tmux,
        "first": first,
        "host": crate::core::config::get().self_name(),
    });
    if let Some(t) = &link.task {
        body["task"] = json!(t);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        let (status, v) = request("POST", "/api/slack/link", Some(&body))?;
        // 404: not listed yet, 409: no session id yet.
        let pending =
            (status == "404" || status == "409") && v["error"].as_str() != Some("not found");
        if pending && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        ok_or_err(&status, &v).map_err(|e| {
            Error::Other(format!(
                "{tmux} started, but linking it to Slack failed: {e} — its prompt was not sent"
            ))
        })?;
        println!("linked: {}", link_line(&v));
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_what_node_sends() {
        assert_eq!(base64_decode("UE5HREFUQQ==").unwrap(), b"PNGDATA");
        assert_eq!(base64_decode("AAEC/w==").unwrap(), vec![0, 1, 2, 255]);
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
        assert!(base64_decode("no*pe").is_none());
    }

    #[test]
    fn file_names_cannot_leave_the_directory() {
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name("plan.png"), "plan.png");
        assert_eq!(safe_name("..."), "file");
    }

    #[test]
    fn a_message_reads_with_who_when_replies_and_files() {
        let m = json!({ "ts": "1792777100.000001", "user": "U1", "name": "Alex", "owner": false, "you": false, "bot": false,
            "text": "plan", "replies": 2, "threadTs": "1792777100.000001",
            "files": [{ "id": "F01", "name": "plan.png", "mimetype": "image/png", "size": 2048 }] });
        let t = message_text(&m, false);
        assert!(t.starts_with("• Alex (U1) at "), "{t}");
        assert!(
            t.contains("· 2 replies (fleet slack thread … 1792777100.000001)"),
            "{t}"
        );
        assert!(
            t.ends_with("plan\n  [file F01 · plan.png · image/png · 2 KB]"),
            "{t}"
        );
        assert!(!message_text(&m, true).contains("replies"));
    }
}
