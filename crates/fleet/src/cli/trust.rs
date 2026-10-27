//! Text/JSON rendering of the Session host trust command.
use serde_json::Value;

use crate::{
    core::trust,
    error::{Error, Result},
};

pub fn early_failure_fields(v: &mut Value) {
    v["detail"] = Value::Null;
    v["pane"] = Value::Null;
    v["typed"] = false.into();
}

pub fn run(o: trust::Opts, json: bool, remote: bool) -> Result<()> {
    let result = if remote {
        Err(trust::Failure::new("usage"))
    } else {
        trust::run(&o)
    };
    let (mut v, failure) = match result {
        Ok(v) => (v, None),
        Err(f) => (f.json(), Some(f.message())),
    };
    v["host"] = super::commands::host_label().into();
    if json {
        println!("{v}");
    } else if failure.is_none() {
        print_text(&v, o.list);
    }
    match failure {
        Some(message) => Err(Error::exit(1, message)),
        None => Ok(()),
    }
}

fn print_text(v: &Value, list: bool) {
    if list {
        let p = &v["prompt"];
        for line in p["lines"].as_array().into_iter().flatten() {
            println!("{}", line.as_str().unwrap_or_default());
        }
        for (i, c) in p["choices"].as_array().into_iter().flatten().enumerate() {
            let cursor = if p["selected"] == i { "❯" } else { " " };
            println!("{cursor} {}", c["label"].as_str().unwrap_or_default());
        }
        match (v["detail"].as_str(), p["id"].as_str()) {
            (None, Some(id)) => println!("answerable: --prompt {id}"),
            (detail, _) => println!("not answerable: {}", detail.unwrap_or("no_prompt")),
        }
        return;
    }
    let keys = v["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["key"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let answer = v["answer"].as_str().unwrap_or_default();
    if v["dry_run"] == true {
        println!("would {answer} with: {keys}");
        return;
    }
    let closed = match (v["closed"].as_bool(), v["close_detail"].as_str()) {
        (Some(true), _) => "; tmux session closed".to_string(),
        (Some(false), why) => format!("; tmux session left open ({})", why.unwrap_or("")),
        (None, _) => String::new(),
    };
    println!("answered: {answer} ({keys}){closed}");
}
