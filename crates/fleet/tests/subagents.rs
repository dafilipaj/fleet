//! Fresh-process list timing with synthetic state. The core's 210-meta I/O
//! regression checks exactly which files each poll opens; this measures CLI cost.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use serde_json::json;

#[test]
#[ignore = "timing benchmark: run --release --test subagents -- --ignored --nocapture"]
fn list_cost_with_210_synthetic_metas() {
    let env = common::Env::new();
    let bin = env.path("bin");
    fs::create_dir(&bin).unwrap();
    // Discovery sees one synthetic process. No real ps, tmux server or agent runs.
    let ps = bin.join("ps");
    fs::write(&ps, "#!/bin/sh\ncase \"$*\" in\n'-o tty= -p 42') printf 'ttys001\\n';;\n'eww -p 42') exit 0;;\n'eww -o pid=,tty=,lstart=,command= -p 42') printf '42 ttys001 Thu Jan 1 00:00:00 2026 synthetic\\n';;\n*) exit 1;;\nesac\n").unwrap();
    fs::set_permissions(&ps, fs::Permissions::from_mode(0o755)).unwrap();
    let tmux = bin.join("tmux");
    fs::write(&tmux, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let sessions = env.path("home/.claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("42.json"),
        json!({"pid":42, "sessionId":"session",
        "cwd":"/work/project", "version":"2.1.301", "startedAt":1767225600000_i64,
        "kind":"interactive", "entrypoint":"cli", "status":"idle"})
        .to_string(),
    )
    .unwrap();
    let project = env.path("home/.claude/projects/-work-project");
    let agents = project.join("session/subagents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        project.join("session.jsonl"),
        include_str!("../../../testdata/subagents/typed.jsonl"),
    )
    .unwrap();
    // Mostly historical finishes, plus ten current finishes and ten running tasks.
    // Historical logs intentionally do not exist: opening them would be wasted I/O.
    for id in 0..210 {
        let meta = agents.join(format!("agent-{id}.meta.json"));
        fs::write(
            &meta,
            json!({"finished":id < 200, "description":"Synthetic task"}).to_string(),
        )
        .unwrap();
        let at = std::time::UNIX_EPOCH
            + Duration::from_secs(1_767_225_600 + if id < 190 { 30 } else { 120 });
        fs::File::options()
            .write(true)
            .open(&meta)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(at))
            .unwrap();
        if (190..200).contains(&id) {
            fs::write(
                agents.join(format!("agent-{id}.jsonl")),
                "{\"timestamp\":\"2026-01-01T00:01:10Z\"}\n",
            )
            .unwrap();
        }
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |expected: serde_json::Value| {
        let before = Instant::now();
        let out = env
            .cmd()
            .env("PATH", &path)
            .env("FLEET_TMUX", &tmux)
            .args(["list", "--json"])
            .output()
            .unwrap();
        let elapsed = before.elapsed();
        assert!(out.status.success(), "{}", common::stderr(&out));
        let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            rows.as_array().unwrap().len(),
            1,
            "synthetic registry must be discovered"
        );
        assert_eq!(rows[0]["subagents"], expected);
        elapsed
    };
    let parked = project.join("sidecars-parked");
    let mut empty = Vec::new();
    let mut full = Vec::new();
    for pair in 0..65 {
        // Alternate order and omit five warm-up pairs; each list is a new process.
        for populated in if pair % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            if !populated {
                fs::rename(&agents, &parked).unwrap();
            }
            let elapsed = run(if populated {
                json!({"running":10,"finished":10})
            } else {
                json!({"running":0,"finished":0})
            });
            if !populated {
                fs::rename(&parked, &agents).unwrap();
            }
            if pair >= 5 {
                if populated {
                    full.push(elapsed);
                } else {
                    empty.push(elapsed);
                }
            }
        }
    }
    empty.sort();
    full.sort();
    let baseline = empty[empty.len() / 2].as_secs_f64() * 1000.0;
    let populated = full[full.len() / 2].as_secs_f64() * 1000.0;
    eprintln!(
        "fresh fleet list, 60 alternating pairs: empty {baseline:.3} ms, 210 metas {populated:.3} ms, added {:.3} ms",
        populated - baseline
    );
}
