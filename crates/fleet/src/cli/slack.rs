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
    let base = local_web_base();
    let url = format!("{base}{path}");
    let mut c = std::process::Command::new("curl");
    c.args([
        "-sS",
        "--max-time",
        "40",
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
        Duration::from_secs(45),
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

fn clock(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

fn link_line(l: &Value) -> String {
    let who = l["sessionName"]
        .as_str()
        .or(l["session"].as_str())
        .unwrap_or("?");
    let until = l["until"].as_i64().map(clock).unwrap_or_default();
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
            let pane = std::env::var("TMUX_PANE").map_err(|_| {
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
    if let Ok(pane) = std::env::var("TMUX_PANE") {
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
