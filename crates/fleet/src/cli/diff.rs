//! `fleet diff <target>`: a session's Changes per checkout (see [`crate::core::diff`]).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::core::diff;
use crate::core::discovery;
use crate::core::harness::Harness;
use crate::error::{Error, Result};

pub struct Opts {
    pub target: String,
    pub json: bool,
    pub since: Option<String>,
}

pub fn run(o: Opts, remote: bool) -> Result<()> {
    let result = if remote {
        Err((
            "usage",
            "diff runs on this machine only (pass --local)".to_string(),
        ))
    } else {
        collect(&o)
    };
    match result {
        Ok(v) => {
            if o.json {
                println!("{v}");
            } else {
                print_text(&v);
            }
            Ok(())
        }
        Err((kind, message)) => {
            if o.json {
                println!("{}", json!({ "ok": false, "kind": kind, "error": message }));
            }
            Err(Error::exit(1, message))
        }
    }
}

fn collect(o: &Opts) -> std::result::Result<Value, (&'static str, String)> {
    let session = discovery::resolve_target(&o.target).map_err(|e| (e.kind(), e.to_string()))?;
    let transcripts = match session.harness {
        Harness::Claude => super::messages::locate(&session)
            .ok()
            .flatten()
            .map(|t| with_subagents(&t))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut v = diff::run(&diff::Input {
        session_id: session.session_id.clone().filter(|s| !s.is_empty()),
        harness: session.harness,
        cwd: session.cwd.clone(),
        transcripts,
        since: o.since.clone(),
    });
    let mut out = json!({
        "ok": true,
        "session_id": session.session_id,
        "harness": session.harness,
    });
    if let (Some(dst), Some(src)) = (out.as_object_mut(), v.as_object_mut()) {
        dst.append(src);
    }
    Ok(out)
}

/// The transcript, then every subagent transcript beside it (`<id>/subagents/agent-*.jsonl`),
/// running or finished, nested ones included: they all live in that one folder.
fn with_subagents(t: &Path) -> Vec<PathBuf> {
    let mut out = vec![t.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(t.with_extension("").join("subagents")) {
        let mut subs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "jsonl")
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("agent-"))
            })
            .collect();
        subs.sort();
        out.extend(subs);
    }
    out
}

fn print_text(v: &Value) {
    if v["unchanged"] == true {
        println!("unchanged");
        return;
    }
    let cands = v["candidates"].as_array().cloned().unwrap_or_default();
    for c in &cands {
        let st = &c["stats"];
        println!(
            "{} ({}) +{} -{} in {} files",
            c["root"].as_str().unwrap_or_default(),
            c["branch"].as_str().unwrap_or("detached"),
            st["additions"],
            st["deletions"],
            st["files"]
        );
        for f in c["files"].as_array().into_iter().flatten() {
            println!(
                "  {:<9} +{:<5} -{:<5} {}",
                f["kind"].as_str().unwrap_or_default(),
                f["additions"],
                f["deletions"],
                f["path"].as_str().unwrap_or_default()
            );
        }
    }
    for s in v["skipped"].as_array().into_iter().flatten() {
        println!(
            "{} skipped: {}",
            s["root"].as_str().unwrap_or_default(),
            s["detail"].as_str().unwrap_or_default()
        );
    }
    if cands.is_empty() && v["skipped"].as_array().is_none_or(Vec::is_empty) {
        println!("no checkouts");
    }
}
