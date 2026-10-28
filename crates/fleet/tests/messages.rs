//! `fleet messages`: transcripts read through the CLI, against canned sessions
//! (`FLEET_FIXTURE`) and synthetic transcripts under a temp `HOME`.
#![allow(deprecated)]

mod common;

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

struct T {
    env: common::Env,
    home: PathBuf,
}

impl T {
    fn new(sessions: Value) -> Self {
        let env = common::Env::new();
        let home = env.path("home");
        std::fs::create_dir_all(&home).unwrap();
        common::fixture(env.dir.path(), &sessions.to_string());
        T { env, home }
    }

    fn write(&self, rel: &str, lines: &[Value]) -> PathBuf {
        let p = self.home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn run(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self
            .env
            .cmd()
            .env("HOME", &self.home)
            .env("FLEET_FIXTURE", self.env.path("fleet-fixture.json"))
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("PI_CODING_AGENT_DIR", self.home.join(".pi/agent"))
            .args(args)
            .output()
            .unwrap();
        let stdout = common::stdout(&out);
        let v = if args.contains(&"--json") {
            // Exactly one JSON object, on one line.
            assert_eq!(stdout.trim().lines().count(), 1, "stdout: {stdout}");
            serde_json::from_str(stdout.trim()).expect("one JSON object")
        } else {
            Value::String(stdout)
        };
        (out.status.code().unwrap(), v, common::stderr(&out))
    }
}

fn user(text: &str) -> Value {
    json!({ "type": "user", "timestamp": "2026-09-18T18:01:52.017Z", "message": { "role": "user", "content": text } })
}

fn asst(text: &str, id: &str) -> Value {
    json!({ "type": "assistant", "timestamp": "2026-09-18T18:01:57.000Z",
            "message": { "id": id, "role": "assistant", "content": [{ "type": "text", "text": text }], "stop_reason": "end_turn" } })
}

fn session(
    pid: i64,
    id: Option<&str>,
    cwd: &str,
    harness: &str,
    transcript: Option<&Path>,
) -> Value {
    json!({ "pid": pid, "session_id": id, "cwd": cwd, "status": "idle", "backend": "tmux",
            "harness": harness, "transcript": transcript.map(|p| p.display().to_string()) })
}

fn texts(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn claude_transcript_found_after_the_session_moved() {
    // Started in /w/old (the transcript's project dir), now running in /w/new.
    let t = T::new(json!([session(
        101,
        Some("aaaa-1111"),
        "/w/new",
        "claude",
        None
    )]));
    t.write(
        ".claude/projects/-w-old/aaaa-1111.jsonl",
        &[user("Hello there"), asst("Hi.", "m1")],
    );
    let (code, v, _) = t.run(&["messages", "aaaa", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["ok"], true);
    assert_eq!(v["session_id"], "aaaa-1111");
    assert_eq!(v["harness"], "claude");
    assert!(
        v["transcript"]
            .as_str()
            .unwrap()
            .ends_with("/.claude/projects/-w-old/aaaa-1111.jsonl")
    );
    assert_eq!(texts(&v), ["Hello there", "Hi."]);
    assert_eq!(v["entries"][0]["role"], "user");
    assert_eq!(v["entries"][1]["final"], true);
    assert_eq!(v["has_more_before"], false);
    assert_eq!(v["has_more_after"], false);
    assert_eq!(v["updated"], json!([]));
}

#[test]
fn polling_with_the_cursor_returns_only_new_entries() {
    let t = T::new(json!([session(
        101,
        Some("aaaa-1111"),
        "/w",
        "claude",
        None
    )]));
    let p = t.write(".claude/projects/-w/aaaa-1111.jsonl", &[user("one")]);
    let (_, first, _) = t.run(&["messages", "101", "--json"]);
    let cursor = first["cursor"].as_str().unwrap().to_string();

    let (_, idle, _) = t.run(&["messages", "101", "--json", "--after", &cursor]);
    assert_eq!(idle["entries"], json!([]));
    assert_eq!(idle["cursor"], cursor.as_str());

    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str(&format!("{}\n{}\n", asst("two", "m1"), user("three")));
    std::fs::write(&p, text).unwrap();
    let (_, next, _) = t.run(&["messages", "101", "--json", "--after", &cursor]);
    assert_eq!(texts(&next), ["two", "three"]);

    // "Show earlier" from the newest entry.
    let newest = next["entries"][1]["cursor"].as_str().unwrap();
    let (_, older, _) = t.run(&[
        "messages", "101", "--json", "--before", newest, "--limit", "1",
    ]);
    assert_eq!(texts(&older), ["two"]);
    assert_eq!(older["has_more_before"], true);
}

#[test]
fn codex_and_pi_transcripts_from_discovery_or_by_id() {
    let codex_lines = [
        json!({ "type": "session_meta", "payload": { "id": "c-1", "cwd": "/w" } }),
        json!({ "timestamp": "2026-10-11T19:50:53.000Z", "type": "response_item", "payload": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "fix it" }] } }),
        json!({ "type": "response_item", "payload": { "type": "function_call", "name": "exec_command", "call_id": "k1", "arguments": "{\"cmd\":\"cargo test\"}" } }),
        json!({ "type": "response_item", "payload": { "type": "function_call_output", "call_id": "k1", "output": "TOOL OUTPUT" } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Fixed." }] } }),
    ];
    let pi_lines = [
        json!({ "type": "session", "id": "p-1", "cwd": "/w" }),
        json!({ "type": "message", "message": { "role": "user", "content": [{ "type": "text", "text": "hi pi" }] } }),
        json!({ "type": "message", "message": { "role": "assistant", "content": [{ "type": "text", "text": "hello" }], "stopReason": "stop" } }),
    ];
    let t = T::new(json!([]));
    let codex = t.write(
        ".codex/sessions/2026/10/11/rollout-2026-10-11T21-50-53-c-1.jsonl",
        &codex_lines,
    );
    t.write(
        ".pi/agent/sessions/--elsewhere--/2026-10-11T19-50-44-044Z_p-1.jsonl",
        &pi_lines,
    );
    // Codex: the path discovery paired it with. pi: no path, found by its id.
    common::fixture(
        t.env.dir.path(),
        &json!([
            session(201, Some("c-1"), "/w", "codex", Some(&codex)),
            session(202, Some("p-1"), "/w", "pi", None),
            session(203, None, "/w", "codex", None),
        ])
        .to_string(),
    );
    let (code, v, _) = t.run(&["messages", "201", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["harness"], "codex");
    assert_eq!(texts(&v), ["fix it", "exec_command: cargo test", "Fixed."]);
    assert_eq!(v["entries"][1]["kind"], "tool_call");
    assert_eq!(v["entries"][1]["tool"], "exec_command");
    assert_eq!(v["entries"][1]["summary"], "cargo test");
    assert!(!v.to_string().contains("TOOL OUTPUT"));

    let (_, v, _) = t.run(&["messages", "202", "--json"]);
    assert_eq!(v["harness"], "pi");
    assert_eq!(texts(&v), ["hi pi", "hello"]);

    // A fresh Codex session has no transcript yet: an empty conversation, not an error.
    let (code, v, _) = t.run(&["messages", "203", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["transcript"], Value::Null);
    assert_eq!(v["entries"], json!([]));
    assert_eq!(v["cursor"], Value::Null);
}

#[test]
fn failures_are_one_json_object_with_a_kind() {
    let t = T::new(json!([
        session(101, Some("aaaa-1111"), "/w", "claude", None),
        session(102, Some("aaaa-2222"), "/w", "claude", None),
        session(103, None, "/w", "claude", None),
    ]));
    t.write(".claude/projects/-w/aaaa-1111.jsonl", &[user("one")]);
    let cases: [(&[&str], &str); 7] = [
        (&["messages", "zzzz", "--json"], "target_not_found"),
        (&["messages", "aaaa", "--json"], "target_ambiguous"),
        (&["messages", "103", "--json"], "transcript_not_found"),
        (
            &[
                "messages", "101", "--json", "--after", "m1.0.0.0", "--before", "m1.0.0.0",
            ],
            "usage",
        ),
        (&["messages", "101", "--json", "--limit", "0"], "usage"),
        (
            &["messages", "101", "--json", "--after", "garbage"],
            "cursor_invalid",
        ),
        (
            &["messages", "101", "--json", "--after", "m1.3.0.12345678"],
            "cursor_invalid",
        ),
    ];
    for (args, kind) in cases {
        let (code, v, stderr) = t.run(args);
        assert_eq!(code, 1, "{args:?}");
        assert_eq!(v["ok"], false, "{args:?}");
        assert_eq!(v["kind"], kind, "{args:?}: {v}");
        let msg = v["error"].as_str().unwrap();
        assert!(
            !msg.is_empty() && stderr.contains(msg),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn usage_errors_clap_catches_are_one_json_object_too() {
    let t = T::new(json!([]));
    for args in [
        &["messages", "--json"][..],
        &["messages", "x", "--json", "--limit"],
        &["messages", "x", "--json", "--nope"],
        // Global flags grouped or spelled with `=`: clap finds the subcommand all the same.
        &["-nH", "laptop", "messages", "--json"],
        &["--host=laptop", "messages", "--json", "--limit"],
    ] {
        let out = t.env.cmd().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stdout = common::stdout(&out);
        assert_eq!(stdout.trim().lines().count(), 1, "{stdout}");
        let v: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(
            (v["ok"].clone(), v["kind"].clone()),
            (json!(false), json!("usage"))
        );
        assert!(!v["error"].as_str().unwrap().is_empty());
        assert!(common::stderr(&out).contains("error:"));
    }
    let missing = t.env.cmd().args(["messages", "--json"]).output().unwrap();
    assert!(common::stdout(&missing).contains("<TARGET>"));
    // Without --json, and for help, clap answers as always.
    let plain = t.env.cmd().args(["messages"]).output().unwrap();
    assert_eq!(common::stdout(&plain), "");
    let help = t
        .env
        .cmd()
        .args(["messages", "--json", "--help"])
        .output()
        .unwrap();
    assert_eq!(help.status.code(), Some(0));
}

#[test]
fn a_claude_session_before_its_first_prompt_is_empty() {
    let t = T::new(json!([session(
        101,
        Some("bbbb-1111"),
        "/w",
        "claude",
        None
    )]));
    let (code, v, _) = t.run(&["messages", "101", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["transcript"], Value::Null);
    assert_eq!(v["entries"], json!([]));
    assert_eq!(v["cursor"], Value::Null);
}

#[test]
fn text_output_is_one_line_per_entry() {
    let t = T::new(json!([session(
        101,
        Some("aaaa-1111"),
        "/w",
        "claude",
        None
    )]));
    t.write(
        ".claude/projects/-w/aaaa-1111.jsonl",
        &[user("Hello there"), asst("Line one\nLine two", "m1")],
    );
    let (code, v, _) = t.run(&["messages", "101"]);
    assert_eq!(code, 0);
    let out = v.as_str().unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("you") && lines[0].contains("Hello there"));
    assert!(lines[1].contains("claude") && lines[1].contains("Line one …"));
}

// C1: a question's option carries its `preview` as the tool input has it (a recorded Claude
// Code 2.1.312 input: box drawing, `\n` line breaks, inner blanks); an option without one has no
// `preview` key. Nothing else about the entry changes.
#[test]
fn a_question_options_preview_is_in_its_entry() {
    let t = T::new(json!([session(
        101,
        Some("cccc-1111"),
        "/w",
        "claude",
        None
    )]));
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens/live");
    let input: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("claude-question-preview-C-input.json")).unwrap(),
    )
    .unwrap();
    t.write(
        ".claude/projects/-w/cccc-1111.jsonl",
        &[
            user("Pick a layout"),
            json!({ "type": "assistant", "timestamp": "2026-09-18T18:01:57.000Z",
                    "message": { "id": "m1", "role": "assistant", "stop_reason": "tool_use",
                    "content": [{ "type": "tool_use", "id": "toolu_q", "name": "AskUserQuestion", "input": input }] } }),
        ],
    );
    let (code, v, err) = t.run(&["messages", "101", "--json"]);
    assert_eq!(code, 0, "{err}");
    let e = &v["entries"][1];
    assert_eq!(
        (&e["kind"], &e["id"], &e["status"], &e["answer"]),
        (
            &json!("question"),
            &json!("toolu_q"),
            &json!("pending"),
            &Value::Null
        )
    );
    let opts = &e["questions"][0]["options"];
    for i in 0..3 {
        let want = &input["questions"][0]["options"][i]["preview"];
        assert!(want.as_str().unwrap().contains('\n'));
        assert_eq!(&opts[i]["preview"], want, "{i}");
        assert_eq!(
            opts[i]["description"],
            input["questions"][0]["options"][i]["description"]
        );
    }
    // The second question has no previews: no `preview` key at all.
    for o in e["questions"][1]["options"].as_array().unwrap() {
        assert_eq!(o.get("preview"), None, "{o}");
        assert_eq!(
            o.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["label", "description"]
        );
    }
    // The entry's text stays the plain markdown it was.
    assert!(!e["text"].as_str().unwrap().contains('┌'));
}
