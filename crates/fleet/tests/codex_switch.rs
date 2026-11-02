//! `fleet model` and `fleet effort` against a fake Codex on the test's private tmux server. The
//! checks, the lock and the wait are Claude Code's (tests/model.rs, tests/effort.rs); here, what
//! Codex changes. The fake runs as `codex … resume -- <id>`, so discovery pairs it with its
//! rollout by that id; it draws a captured screen (testdata/screens, `live/codex-*` the live
//! captures, `codex-model-*` and `codex-reasoning-unavailable` of Codex 0.166 in a 110x34
//! pane), logs every byte it reads, and answers `/model <slug>` and `/model --reasoning
//! <level>` + Enter as Codex 0.166 does, by `mode`:
//!
//! - `ok`: appends a `thread_settings_applied` event naming the settings asked for to its
//!   rollout — a model switch with that model's default level, `medium`, as Codex 0.166 moves
//!   it — and draws `• Model changed to …` (`live/codex-model-changed`);
//! - `other`: appends one naming another model (`gpt-6-other`) and level (`low`);
//! - `unavailable`: draws the refusal for what was asked (`live/codex-model-unavailable`,
//!   `live/codex-reasoning-unavailable`, the slug or level in it replaced), its box back;
//! - `silent` (and any other): does nothing.
//!
//! These tests need tmux and perl, and fail without them rather than pass having run nothing.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fake::FakeFleet;
use common::stdout;
use common::switch::{failed, keys_of};
use serde_json::Value;

const ID: &str = "01a193f7-d7e7-7cb2-987b-91673586e707";

const AGENT: &str = r#"
use strict; use Time::HiRes qw(time); use POSIX qw(strftime);
my ($log, $mode, $screens, $rollout) = @ARGV;
system("stty raw -echo");
sub draw {
  my ($name, @subs) = @_;
  open(my $s, "<", "$screens/$name.txt") or die; local $/; my $t = <$s>; close $s;
  while (@subs) { my ($from, $to) = splice(@subs, 0, 2); $t =~ s/\Q$from\E/$to/g; }
  $t =~ s/\n/\r\n/g; syswrite(STDOUT, "\e[H\e[2J$t");
}
my ($model, $effort) = ("gpt-6-luna", "low");
sub applied {
  my ($m, $e) = @_;
  my $t = time; my $at = strftime("%Y-%m-%dT%H:%M:%S", gmtime($t)) . sprintf(".%03dZ", int(($t - int($t)) * 1000));
  open(my $f, ">>", $rollout) or die;
  print $f '{"timestamp":"' . $at . '","type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"t","thread_settings":{"model":"' . $m . '","model_provider_id":"openai","reasoning_effort":"' . $e . '"}}}' . "\n";
  close $f;
}
draw("live/codex-empty");
open(my $f, ">>", $log) or die; select((select($f), $| = 1)[0]);
my $seen = "";
while (sysread(STDIN, my $b, 64)) {
  print $f $b;
  $seen .= $b;
  next unless $seen =~ m{/model (?:--reasoning (\S+)|(\S+))\r};
  my ($level, $slug) = ($1, $2);
  $seen = "";
  if ($mode eq "ok") {
    if (defined $slug) { $model = $slug; $effort = "medium"; } else { $effort = $level; }
    applied($model, $effort);
    draw("live/codex-model-changed", "gpt-6-sol medium", "$model $effort");
  } elsif ($mode eq "other") {
    applied("gpt-6-other", "low");
  } elsif ($mode eq "unavailable") {
    if (defined $slug) { draw("live/codex-model-unavailable", "gpt-nope", $slug); }
    else { draw("live/codex-reasoning-unavailable", "'ultra'", "'$level'"); }
  }
}
"#;

/// A fake Codex session: its pid, the log of what it read, its rollout.
struct Agent {
    f: FakeFleet,
    pid: i64,
    log: PathBuf,
    rollout: PathBuf,
}

/// A fake Codex in `mode`, the pane's own process; with `rollout` false, before its first
/// prompt (no rollout written yet).
fn agent_with(mode: &str, rollout: bool) -> Agent {
    assert!(
        fleet::core::tools::find_binary("perl").is_some(),
        "these tests need perl"
    );
    let f = FakeFleet::new().expect("these tests need tmux");
    let dir = f.env.path("home/.codex/sessions/2026/10/31");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-10-31T22-57-35-{ID}.jsonl"));
    if rollout {
        let cwd = f.env.dir.path().display().to_string();
        let lines = [
            serde_json::json!({"timestamp": "2026-10-31T21:58:19.495Z", "type": "session_meta",
                "payload": {"id": ID, "cwd": cwd}}),
            serde_json::json!({"timestamp": "2026-10-31T21:58:19.496Z", "type": "event_msg",
                "payload": {"type": "task_started"}}),
            serde_json::json!({"timestamp": "2026-10-31T21:58:19.890Z", "type": "turn_context",
                "payload": {"model": "gpt-6-luna", "effort": "low"}}),
            serde_json::json!({"timestamp": "2026-10-31T21:58:20.796Z", "type": "event_msg",
                "payload": {"type": "task_complete"}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
    }
    let script = f.env.path("agent.pl");
    std::fs::write(&script, AGENT).unwrap();
    let screens = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens");
    let log = f.env.path("received.log");
    let line = format!(
        "exec /bin/bash -c \"exec -a codex perl '{}' '{}' '{mode}' '{}' '{}' resume -- {ID}\"",
        script.display(),
        log.display(),
        screens.display(),
        path.display(),
    );
    let pid = f.agent("codex", ID, 110, 34, &line);
    // A Codex session has no Claude Code registry entry: discovery finds it by `ps`.
    std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent {
        f,
        pid,
        log,
        rollout: path,
    }
}

fn agent(mode: &str) -> Agent {
    agent_with(mode, true)
}

impl Agent {
    fn target(&self) -> String {
        format!("pid:{}", self.pid)
    }

    /// `fleet <verb> --json <target> <value>` → (exit code, the one JSON object on stdout).
    fn switch(&self, verb: &str, value: &str) -> (i32, Value) {
        let t = self.target();
        let out = self.f.fleet(&[verb, "--json", t.as_str(), value]);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
        (out.status.code().unwrap_or(-1), v)
    }

    /// What the agent has read, once a moment has passed for anything on its way.
    fn received(&self) -> String {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The `timestamp` of the rollout's last `thread_settings_applied`.
    fn applied_at(&self) -> String {
        let text = std::fs::read_to_string(&self.rollout).unwrap();
        let line = text
            .lines()
            .rfind(|l| l.contains("thread_settings_applied"))
            .expect("a thread_settings_applied event");
        let v: Value = serde_json::from_str(line).unwrap();
        v["timestamp"].as_str().unwrap().to_string()
    }

    /// This session's `list --json` row.
    fn row(&self) -> Value {
        let out = self.f.fleet(&["list", "--json"]);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let rows = v
            .as_array()
            .cloned()
            .or_else(|| v["sessions"].as_array().cloned());
        rows.unwrap_or_default()
            .into_iter()
            .find(|r| r["pid"] == self.pid)
            .unwrap_or_else(|| panic!("no row for pid {} in {v}", self.pid))
    }
}

#[test]
fn a_codex_model_switch_types_model_slug_and_succeeds_on_its_event() {
    let a = agent("ok");
    let (code, v) = a.switch("model", "gpt-6-sol");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["harness"], "codex");
    assert_eq!(v["model"], "gpt-6-sol");
    assert_eq!(v["confirmed"], false, "Codex asks nothing");
    assert_eq!(v["session_id"], ID);
    assert_eq!(v["switched_at"], a.applied_at());
    assert_eq!(
        keys_of(&v),
        [
            "confirmed",
            "dry_run",
            "harness",
            "host",
            "model",
            "ok",
            "pid",
            "session_id",
            "switched_at"
        ]
    );
    assert_eq!(
        a.received(),
        "/model gpt-6-sol\r",
        "the command, nothing more"
    );
    // `list` shows it at once, dated by the event.
    let row = a.row();
    assert_eq!(row["model"], "gpt-6-sol");
    assert_eq!(row["effort"], "medium", "the new model's default level");
    assert_eq!(row["model_at"], v["switched_at"]);
}

#[test]
fn a_codex_effort_switch_types_model_reasoning_level_and_saves_nothing() {
    let a = agent("ok");
    let (code, v) = a.switch("effort", "high");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["effort"], "high");
    assert_eq!(v["saved"], false, "for this session only");
    assert_eq!(v["confirmed"], false);
    assert_eq!(v["switched_at"], a.applied_at());
    assert_eq!(a.received(), "/model --reasoning high\r");
    let row = a.row();
    assert_eq!(
        (&row["model"], &row["effort"]),
        (&Value::from("gpt-6-luna"), &Value::from("high"))
    );
}

#[test]
fn a_refusal_codex_draws_is_model_not_found_or_effort_not_found() {
    let a = agent("unavailable");
    let (code, v) = a.switch("model", "gpt-nope");
    failed(code, &v, "model_not_found");
    let (code, v) = a.switch("effort", "ultra");
    failed(code, &v, "effort_not_found");
    assert_eq!(
        a.received(),
        "/model gpt-nope\r/model --reasoning ultra\r",
        "each typed once, nothing pressed after"
    );
}

#[test]
fn an_event_naming_other_settings_is_outcome_uncertain() {
    let a = agent("other");
    let (code, v) = a.switch("model", "gpt-6-sol");
    failed(code, &v, "outcome_uncertain");
    assert!(v["error"].as_str().unwrap().contains("gpt-6-other"), "{v}");
    let (code, v) = a.switch("effort", "high");
    failed(code, &v, "outcome_uncertain");
}

#[test]
fn no_event_within_the_wait_is_outcome_uncertain() {
    let a = agent("silent");
    let (code, v) = a.switch("effort", "high");
    failed(code, &v, "outcome_uncertain");
}

#[test]
fn before_its_first_prompt_a_codex_session_is_transcript_not_found_and_nothing_is_typed() {
    let a = agent_with("ok", false);
    for (verb, value) in [("model", "gpt-6-sol"), ("effort", "high")] {
        let (code, v) = a.switch(verb, value);
        failed(code, &v, "transcript_not_found");
    }
    assert_eq!(a.received(), "", "nothing typed");
}
