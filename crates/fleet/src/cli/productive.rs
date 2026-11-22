//! `fleet productive`: Productive links — a session follows one Productive task, as the bridge
//! host's Productive account.
//!
//! The bridge lives in the web server of the `productive.host` machine
//! (`web/lib/productive.mjs`): it polls that account's inbox and types comments and changes on
//! the task into the linked session. These commands are its client: they ask this machine's web
//! server, which proxies to that host. The session answers with `fleet productive reply`, which
//! posts a comment as the account (the token never leaves the server).

use std::io::Read;

use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::slack::{ok_or_err, request};
use crate::core::discovery;
use crate::error::{Error, Result};

fn link_line(l: &Value) -> String {
    let who = l["sessionName"]
        .as_str()
        .or(l["session"].as_str())
        .unwrap_or("?");
    let mut extra = Vec::new();
    if l["asleep"].as_bool() == Some(true) {
        extra.push("asleep".to_string());
    }
    if let Some(k) = l["keep"].as_i64() {
        extra.push(format!("sleeps after {}", minutes(k)));
    }
    if let Some(n) = l["pending"].as_i64().filter(|n| *n > 0) {
        extra.push(format!("{n} waiting"));
    }
    if let Some(by) = l["startedBy"]["name"].as_str() {
        extra.push(format!("mentioned by {by}"));
    }
    let task = match l["number"].as_str() {
        Some(n) => format!("#{n}"),
        None => l["taskId"].as_str().unwrap_or("?").to_string(),
    };
    format!(
        "{}  {}  {} {}{}",
        l["id"].as_str().unwrap_or("?").bold(),
        who,
        task,
        l["url"].as_str().unwrap_or("?"),
        if extra.is_empty() {
            String::new()
        } else {
            format!("  ({})", extra.join(", "))
        }
    )
}

fn minutes(m: i64) -> String {
    match m {
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// The session a command is about: `--session <target>`, else the one it runs in ($TMUX_PANE).
fn target(body: &mut Value, session: Option<&str>) -> Result<()> {
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
                    "not inside a tmux session — name the session: --session <target>".into(),
                )
            })?;
            body["pane"] = json!(pane);
        }
    }
    body["host"] = json!(crate::core::config::get().self_name());
    Ok(())
}

/// `fleet productive [list]`.
pub fn list(json: bool) -> Result<()> {
    let (status, v) = request("GET", "/api/productive", None)?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if v["enabled"].as_bool() != Some(true) {
        let why = v["error"]
            .as_str()
            .unwrap_or("no host runs them — set productive.host in the config");
        println!("Productive links are off: {why}");
        return Ok(());
    }
    let poll = match v["error"].as_str() {
        Some(e) => format!("{} ({e})", "failing".red()),
        None => "polling".green().to_string(),
    };
    println!(
        "Productive bridge on {} as {} (person {}): {poll}",
        v["host"].as_str().unwrap_or("?"),
        v["me"]["name"].as_str().unwrap_or("?"),
        v["me"]["id"].as_str().unwrap_or("?")
    );
    match v["spawn"]["from"].as_array() {
        Some(from) if !from.is_empty() => {
            let names: Vec<&str> = from.iter().filter_map(Value::as_str).collect();
            println!("mentions start sessions from: {}", names.join(", "));
        }
        _ => println!("mentions start no sessions (productive.spawn)"),
    }
    let links = v["links"].as_array().cloned().unwrap_or_default();
    if links.is_empty() {
        println!("no active links");
    }
    for l in &links {
        println!("{}", link_line(l));
    }
    Ok(())
}

/// `fleet productive link <task> [--session <target>] [--task <text>] [--keep <duration>]`.
pub fn link(
    url: &str,
    session: Option<&str>,
    task: Option<&str>,
    keep: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut body = json!({ "url": url });
    if let Some(t) = task {
        body["task"] = json!(t);
    }
    if let Some(k) = keep {
        body["keep"] = json!(k);
    }
    target(&mut body, session)?;
    let (status, v) = request("POST", "/api/productive/link", Some(&body))?;
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

/// `fleet productive reply <text | -> [--file <path>] [--link <pl-…>] [--session <target>]`.
pub fn reply(
    text: Option<&str>,
    file: Option<&str>,
    link: Option<&str>,
    session: Option<&str>,
    json: bool,
) -> Result<()> {
    let body_text = match (text, file) {
        (_, Some(f)) => {
            std::fs::read_to_string(f).map_err(|e| Error::Other(format!("cannot read {f}: {e}")))?
        }
        (Some("-"), None) | (None, None) => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| Error::Other(format!("cannot read stdin: {e}")))?;
            s
        }
        (Some(t), None) => t.to_string(),
    };
    if body_text.trim().is_empty() {
        return Err(Error::Other("nothing to post: the comment is empty".into()));
    }
    let mut body = json!({ "body": body_text });
    match link {
        Some(l) => {
            body["link"] = json!(l);
            body["host"] = json!(crate::core::config::get().self_name());
        }
        None => target(&mut body, session)?,
    }
    let (status, v) = request("POST", "/api/productive/comment", Some(&body))?;
    ok_or_err(&status, &v)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("commented on {}", v["url"].as_str().unwrap_or("the task"));
    }
    Ok(())
}

/// `fleet productive unlink <link>`.
pub fn unlink(id: &str, json: bool) -> Result<()> {
    let (status, v) = request(
        "POST",
        &format!(
            "/api/productive/links/{}/unlink",
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
