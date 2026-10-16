//! `fleet answer` end to end: fake agents in panes of a private tmux server
//! (`tmux -L fleet-test-<rand> -f /dev/null`, sockets in a temp `TMUX_TMPDIR`, a temp `HOME`,
//! `TMUX` / `TMUX_PANE` unset on every call) draw a prompt, redraw it as keys arrive and record
//! every keystroke; a fake Claude registry entry under the temp `HOME` makes fleet discover them.
//! The user's own tmux server, config and sessions are never visible, let alone touched.
#![allow(deprecated)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A permission prompt in Claude Code 2.1's layout (wording synthetic). Keys move the cursor;
/// a digit picks that answer, Enter the one under the cursor. Args: key log, answer file,
/// mode (`replace`: the answer reveals another prompt with the same answers — a queued one).
const FAKE_PERMISSION: &str = r#"#!/bin/bash
log="$1"; out="$2"; mode="$3"; cmd="rm -rf build"
labels=("Yes" "Yes, and always allow access to /w/app from this project" "No")
cur=0
draw() {
  printf '\033[2J\033[H'
  echo "❯ Clean the build output"
  echo "⏺ Removing the build output"
  echo
  echo "────────────────────────────────────────────────────────────────────────"
  echo " Bash command"
  echo
  echo " Remove the build output"
  echo "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌"
  echo " $cmd"
  echo "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌"
  echo " Do you want to proceed?"
  for i in 0 1 2; do
    m="  "; [ $cur -eq $i ] && m="❯ "
    echo " ${m}$((i+1)). ${labels[$i]}"
  done
  echo
  echo " Esc to cancel · Tab to amend"
}
stty -echo -icanon min 1 time 0
draw
while IFS= read -r -s -n1 -d '' c; do
  printf '%s' "$c" >> "$log"
  case "$c" in
    $'\e') IFS= read -r -s -n2 -d '' rest; printf '%s' "$rest" >> "$log"
           [ "$rest" = "[B" ] && [ $cur -lt 2 ] && cur=$((cur+1))
           [ "$rest" = "[A" ] && [ $cur -gt 0 ] && cur=$((cur-1)) ;;
    [1-3]) echo "picked=$c" >> "$out"
           if [ "$mode" = replace ] && [ "$cmd" = "rm -rf build" ]; then cmd="rm -rf dist"; cur=0
           else printf '\033[2J\033[H● done\n'; sleep 60; exit; fi ;;
    $'\n'|$'\r') echo "picked=$((cur+1))" > "$out"; printf '\033[2J\033[H● done\n'; sleep 60; exit ;;
  esac
  draw
done
"#;

/// A Claude Code 2.1 question form (one question, "Which city?", options Paris and Rome) that
/// behaves like the real one: ↑/↓ move the cursor, typing on "Type something" fills it in,
/// Enter picks (single-select) or ticks (multi-select; on "Submit" it submits).
/// Args: single|multi, key log, answer file, mischief (1: the cursor jumps a row by itself
/// after the first key — a human pressing ↓ mid-answer; 2: Rome starts ticked).
const FAKE_FORM: &str = r#"#!/bin/bash
mode="$1"; log="$2"; out="$3"; mischief="$4"
labels=("Paris" "Rome"); descs=("The French capital" "The Italian capital")
cur=0; typed=""; t0=0; t1=0; keys=0
[ "$mischief" = 2 ] && t1=1
last=2; [ "$mode" = multi ] && last=3
rule="────────────────────────────────────────────────────────────"
draw() {
  printf '\033[2J\033[H'
  echo "$rule"
  box="☐"; if [ $t0 -eq 1 ] || [ $t1 -eq 1 ] || [ -n "$typed" ]; then box="☒"; fi
  [ "$mode" = single ] && box="☐"
  echo "←  $box City  ✔ Submit  →"
  echo
  echo "Which city?"
  echo
  for i in 0 1; do
    m="  "; [ $cur -eq $i ] && m="❯ "
    box=""
    if [ "$mode" = multi ]; then
      t=$t0; [ $i -eq 1 ] && t=$t1
      if [ $t -eq 1 ]; then box="[✔] "; else box="[ ] "; fi
    fi
    echo "${m}$((i+1)). ${box}${labels[$i]}"
    echo "     ${descs[$i]}"
  done
  m="  "; [ $cur -eq 2 ] && m="❯ "
  if [ "$mode" = multi ]; then
    if [ -n "$typed" ]; then echo "${m}3. [✔] $typed"; else echo "${m}3. [ ] Type something"; fi
    m="  "; [ $cur -eq 3 ] && m="❯ "
    echo "${m}   Submit"
  else
    if [ -n "$typed" ]; then echo "${m}3. $typed"; else echo "${m}3. Type something."; fi
  fi
  echo "$rule"
  echo "  4. Chat about this"
  echo
  echo "Enter to select · Tab/Arrow keys to navigate · Esc to cancel"
}
stty -echo -icanon min 1 time 0
draw
while IFS= read -r -s -n1 -d '' c; do
  printf '%s' "$c" >> "$log"
  if [ "$c" = $'\e' ]; then
    IFS= read -r -s -n2 -d '' rest; printf '%s' "$rest" >> "$log"
    [ "$rest" = "[B" ] && [ $cur -lt $last ] && cur=$((cur+1))
    [ "$rest" = "[A" ] && [ $cur -gt 0 ] && cur=$((cur-1))
  elif [ "$c" = $'\n' ] || [ "$c" = $'\r' ]; then
    if [ "$mode" = single ] || [ $cur -eq $last ]; then
      echo "row=$cur typed=$typed ticked=$t0$t1" > "$out"
      printf '\033[2J\033[H● answered\n'; sleep 60; exit
    fi
    [ $cur -eq 0 ] && t0=$((1-t0))
    [ $cur -eq 1 ] && t1=$((1-t1))
  elif [ $cur -eq 2 ]; then
    typed="$typed$c"
  fi
  keys=$((keys+1))
  if [ "$mischief" = 1 ] && [ $keys -eq 1 ]; then cur=$((cur+1)); fi
  draw
done
"#;

const QUOTED: &str = "\
● Claude Code will then ask:

   Do you want to proceed?
   ❯ 1. Yes
     2. No

● Want me to go ahead?

────────────────────────────────────────
❯
────────────────────────────────────────
  ? for shortcuts
";

struct Server {
    env: common::Env,
    tmux: PathBuf,
    socket: String,
}

impl Server {
    /// `None` when tmux or bash is missing — the tests then pass vacuously.
    fn new() -> Option<Self> {
        let tmux = fleet::core::tools::find_binary("tmux")?;
        fleet::core::tools::find_binary("bash")?;
        let env = common::Env::new();
        for d in ["home/.claude/sessions", "home/.claude/projects/-w", "bin"] {
            std::fs::create_dir_all(env.path(d)).unwrap();
        }
        // Short: the socket path (temp dir + name) must fit a Unix socket address.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let socket = format!("fleet-test-{:04x}", (std::process::id() ^ nanos) & 0xffff);
        // fleet reaches the private server through this wrapper (FLEET_TMUX).
        let wrapper = env.path("bin/tmux");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec '{}' -L '{socket}' -f /dev/null \"$@\"\n",
                tmux.display()
            ),
        )
        .unwrap();
        std::fs::write(env.path("bin/permission"), FAKE_PERMISSION).unwrap();
        std::fs::write(env.path("bin/form"), FAKE_FORM).unwrap();
        std::fs::write(
            env.path("bin/static"),
            "#!/bin/bash\ncat \"$1\"\nstty -echo -icanon min 1 time 0\nwhile IFS= read -r -s -n1 -d '' c; do printf '%s' \"$c\" >> \"$2\"; done\n",
        )
        .unwrap();
        for f in ["bin/tmux", "bin/permission", "bin/form", "bin/static"] {
            Command::new("chmod")
                .arg("+x")
                .arg(env.path(f))
                .status()
                .unwrap();
        }
        Some(Server { env, tmux, socket })
    }

    /// One tmux call on the private server, isolated from the user's (no config, temp
    /// socket dir and HOME, no TMUX / TMUX_PANE).
    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new(&self.tmux)
            .args(["-L", &self.socket, "-f", "/dev/null"])
            .args(args)
            .env("TMUX_TMPDIR", self.env.dir.path())
            .env("HOME", self.env.path("home"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap();
        if !out.status.success() && args.first() != Some(&"kill-server") {
            eprintln!(
                "tmux {args:?}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Run `script args…` in tmux session `fleet-test-<name>`, registered as a waiting Claude
    /// session with `session_id`; wait until its screen shows `ready`. Returns the pid.
    fn agent(
        &self,
        name: &str,
        script: &str,
        args: &[&Path],
        session_id: &str,
        ready: &str,
    ) -> String {
        let quoted: Vec<String> = std::iter::once(self.env.path(&format!("bin/{script}")))
            .chain(args.iter().map(|p| p.to_path_buf()))
            .map(|p| format!("'{}'", p.display()))
            .collect();
        self.agent_cmd(name, &quoted.join(" "), session_id, ready)
    }

    fn agent_cmd(&self, name: &str, cmd: &str, session_id: &str, ready: &str) -> String {
        let session = format!("fleet-test-{name}");
        self.tmux(&[
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "200",
            "-y",
            "50",
            cmd,
        ]);
        let pid = self.tmux(&["display-message", "-p", "-t", &session, "#{pane_pid}"]);
        assert!(!pid.is_empty(), "no pane pid");
        let reg = json!({ "pid": pid.parse::<i64>().unwrap(), "sessionId": session_id, "cwd": "/w",
                          "status": "waiting", "kind": "interactive", "name": name });
        std::fs::write(
            self.env.path(&format!("home/.claude/sessions/{pid}.json")),
            reg.to_string(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self
            .tmux(&["capture-pane", "-p", "-t", &session])
            .contains(ready)
        {
            assert!(
                Instant::now() < deadline,
                "the fake agent never drew its screen"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
        pid
    }

    fn fleet(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self
            .env
            .cmd()
            .env("HOME", self.env.path("home"))
            .env("TMUX_TMPDIR", self.env.dir.path())
            .env("FLEET_TMUX", self.env.path("bin/tmux"))
            .env("FLEET_LOCK_DIR", self.env.path("locks"))
            .env("CODEX_HOME", self.env.path("home/.codex"))
            .env("PI_CODING_AGENT_DIR", self.env.path("home/.pi"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("FLEET_FIXTURE")
            .timeout(Duration::from_secs(30))
            .args(args)
            .output()
            .unwrap();
        let stdout = common::stdout(&out);
        let v = serde_json::from_str(stdout.trim()).unwrap_or(Value::String(stdout));
        (out.status.code().unwrap_or(-1), v, common::stderr(&out))
    }

    /// A transcript whose last call is a pending "Which city?" question `tq1`.
    fn question_transcript(&self, session_id: &str, multi: bool) {
        let input = json!({ "questions": [{ "question": "Which city?", "header": "City", "multiSelect": multi,
            "options": [{ "label": "Paris", "description": "The French capital" },
                        { "label": "Rome", "description": "The Italian capital" }] }] });
        let lines = [
            json!({ "type": "user", "message": { "role": "user", "content": "plan the trip" } }),
            json!({ "type": "assistant", "message": { "id": "m1", "role": "assistant", "stop_reason": "tool_use",
                "content": [{ "type": "tool_use", "id": "tq1", "name": "AskUserQuestion", "input": input }] } }),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(
            self.env
                .path(&format!("home/.claude/projects/-w/{session_id}.jsonl")),
            text,
        )
        .unwrap();
    }

    fn files(&self, name: &str) -> (PathBuf, PathBuf) {
        (
            self.env.path(&format!("{name}.keys")),
            self.env.path(&format!("{name}.out")),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.tmux(&["kill-server"]);
    }
}

/// What the fake agent received, once it stops growing.
fn keys(log: &Path) -> String {
    let mut last = String::new();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        let now = std::fs::read_to_string(log).unwrap_or_default();
        if now == last && !now.is_empty() {
            break;
        }
        last = now;
    }
    last.replace('\r', "\n")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

const DOWN: &str = "\u{1b}[B";

#[test]
fn a_permission_prompt_is_answered_with_one_key_only_while_on_screen() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("perm");
    let pid = s.agent(
        "perm",
        "permission",
        &[&log, &out],
        "s-perm",
        "Do you want to proceed?",
    );

    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    let p = &v["prompt"];
    assert_eq!(p["kind"], "permission", "{v}");
    assert_eq!(p["title"], "Do you want to proceed?");
    assert_eq!(p["choices"][2]["label"], "No");
    assert_eq!(
        p["detail"],
        json!(["Bash command", "Remove the build output", "rm -rf build"])
    );
    assert_eq!(p["untouched"], true);
    let id = p["id"].as_str().unwrap().to_string();

    // Refusals type nothing.
    let refused: [(&[&str], &str); 3] = [
        (
            &["--choice", "2", "--prompt", "p1.00000000"],
            "prompt_not_on_screen",
        ),
        (&["--choice", "4"], "choice_invalid"),
        (&["--question", "tq"], "usage"),
    ];
    for (args, kind) in refused {
        let mut all = vec!["answer", pid.as_str(), "--json"];
        all.extend_from_slice(args);
        let (code, v, _) = s.fleet(&all);
        assert_eq!((code, v["kind"].as_str()), (1, Some(kind)), "{args:?}");
    }
    // A dry run says what it would type, and types nothing.
    let (code, v, _) = s.fleet(&[
        "-n", "answer", &pid, "--choice", "3", "--prompt", &id, "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(
        (v["dry_run"].clone(), v["answered"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(v["steps"], json!([{ "key": "3" }]));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");

    let (code, v, err) = s.fleet(&["answer", &pid, "--choice", "3", "--prompt", &id, "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["answered"], true);
    assert_eq!(v["prompt_kind"], "permission");
    assert_eq!(v["method"], "key");
    assert_eq!(v["choice"]["n"], 3);
    assert_eq!(keys(&log), "3", "one key, the answer's number");
    assert_eq!(read(&out).trim(), "picked=3");
}

#[test]
fn a_prompt_merely_quoted_in_the_conversation_gets_nothing() {
    let Some(s) = Server::new() else { return };
    let (log, _) = s.files("quoted");
    let screen = s.env.path("quoted.screen");
    std::fs::write(&screen, QUOTED).unwrap();
    let pid = s.agent(
        "quoted",
        "static",
        &[&screen, &log],
        "s-quoted",
        "Claude Code will then ask",
    );
    let (code, v, _) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["prompt"], Value::Null, "{v}");
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "1", "--json"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("prompt_not_on_screen"))
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
}

#[test]
fn a_pending_question_is_answered_key_by_key_against_the_screen() {
    let Some(s) = Server::new() else { return };
    s.question_transcript("s-single", false);
    let (log, out) = s.files("single");
    let mode = s.env.path("single-mode");
    let _ = &mode;
    let pid = s.agent_cmd(
        "single",
        &format!(
            "'{}' single '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-single",
        "Enter to select",
    );

    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["prompt"]["kind"], "question", "{v}");
    assert_eq!(v["prompt"]["id"], "tq1");
    assert_eq!(v["prompt"]["untouched"], true);

    let bad: [(&[&str], &str); 4] = [
        (
            &["--question", "nope", "--answers", "[{\"options\":[1]}]"],
            "question_not_found",
        ),
        (
            &["--question", "tq1", "--answers", "[{\"options\":[0,1]}]"],
            "answer_invalid",
        ),
        (
            &["--question", "tq1", "--answers", "not json"],
            "answer_invalid",
        ),
        (
            &[
                "--question",
                "tq1",
                "--answers",
                "[{\"text\":\"Os\\u001b[Blo\"}]",
            ],
            "answer_invalid",
        ),
    ];
    for (args, kind) in bad {
        let mut all = vec!["answer", pid.as_str(), "--json"];
        all.extend_from_slice(args);
        let (code, v, _) = s.fleet(&all);
        assert_eq!((code, v["kind"].as_str()), (1, Some(kind)), "{args:?}: {v}");
    }
    let (code, v, _) = s.fleet(&[
        "--dry-run",
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!((code, v["dry_run"].clone()), (0, json!(true)));
    assert_eq!(v["steps"], json!([{ "key": "Down" }, { "key": "Enter" }]));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");

    let (code, v, err) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(v["answered"], true);
    assert_eq!(v["prompt_kind"], "question");
    assert_eq!(keys(&log), format!("{DOWN}\n"));
    assert_eq!(read(&out).trim(), "row=1 typed= ticked=00");
}

#[test]
fn a_multi_select_answer_with_typed_text_is_checked_at_every_key() {
    let Some(s) = Server::new() else { return };
    s.question_transcript("s-multi", true);
    let (log, out) = s.files("multi");
    let pid = s.agent_cmd(
        "multi",
        &format!(
            "'{}' multi '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-multi",
        "Enter to select",
    );
    let (code, v, err) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[1],\"text\":\"Oslo\"}]",
        "--json",
    ]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(read(&out).trim(), "row=3 typed=Oslo ticked=01");
    assert_eq!(keys(&log), format!("{DOWN}\n{DOWN}Oslo{DOWN}\n"));
}

#[test]
fn a_form_that_moves_under_the_answer_stops_it() {
    let Some(s) = Server::new() else { return };
    s.question_transcript("s-race", false);
    let (log, out) = s.files("race");
    // After the first key the cursor jumps a row by itself (someone pressed ↓ too).
    let pid = s.agent_cmd(
        "race",
        &format!(
            "'{}' single '{}' '{}' 1",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-race",
        "Enter to select",
    );
    let (code, v, _) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"text\":\"Oslo\"}]",
        "--json",
    ]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("prompt_changed")),
        "{v}"
    );
    assert!(v["error"].as_str().unwrap().contains("after 1 of"), "{v}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&out), "", "nothing was submitted");
    assert_eq!(keys(&log), DOWN);
}

#[test]
fn a_form_already_touched_is_refused() {
    let Some(s) = Server::new() else { return };
    s.question_transcript("s-ticked", true);
    let (log, out) = s.files("ticked");
    // Rome is ticked already, the cursor back on Paris.
    let pid = s.agent_cmd(
        "ticked",
        &format!(
            "'{}' multi '{}' '{}' 2",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-ticked",
        "Enter to select",
    );
    let (code, v, _) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[0]}]",
        "--json",
    ]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("prompt_not_on_screen")),
        "{v}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
}

#[test]
fn json_usage_errors_are_one_object() {
    let env = common::Env::new();
    let out = env.cmd().args(["answer", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_str(common::stdout(&out).trim()).unwrap();
    assert_eq!(
        (v["ok"].clone(), v["kind"].clone()),
        (json!(false), json!("usage"))
    );
}

// --- round 3 -------------------------------------------------------------------

#[test]
fn a_key_that_reveals_a_lookalike_prompt_is_not_claimed_as_answered() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("replace");
    let pid = s.agent_cmd(
        "replace",
        &format!(
            "'{}' '{}' '{}' replace",
            s.env.path("bin/permission").display(),
            log.display(),
            out.display()
        ),
        "s-replace",
        "Do you want to proceed?",
    );
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    // The prompt it answered is gone, but one with the same answers stands in its place.
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert_eq!(v["ok"], false);
    assert_eq!(v["screen"]["kind"], "permission");
    assert_eq!(
        v["screen"]["detail"],
        json!(["Bash command", "Remove the build output", "rm -rf dist"])
    );
    assert_eq!(keys(&log), "3", "one key went out");
}

#[test]
fn another_fleet_command_holding_the_session_makes_it_busy() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("busy");
    let pid = s.agent(
        "busy",
        "permission",
        &[&log, &out],
        "s-busy",
        "Do you want to proceed?",
    );
    std::fs::create_dir_all(s.env.path("locks")).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(s.env.path("locks/s-busy.lock"))
        .unwrap();
    held.lock().unwrap();
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("busy")), "{v}");
    // `send` takes the same lock.
    let (code, _, err) = s.fleet(&["send", &pid, "hello"]);
    assert_eq!(code, 1);
    assert!(err.contains("another fleet command"), "{err}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
    drop(held);
    let (code, v, err) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(read(&out).trim(), "picked=3");
}

#[test]
fn a_human_attached_to_the_session_is_not_typed_over() {
    let Some(s) = Server::new() else { return };
    let Some(script) = fleet::core::tools::find_binary("script") else {
        return;
    };
    let (log, out) = s.files("attached");
    let pid = s.agent(
        "attached",
        "permission",
        &[&log, &out],
        "s-attached",
        "Do you want to proceed?",
    );
    // A terminal client attached to the session (through a pty: `script`).
    let attach = format!(
        "exec '{}' -L '{}' -f /dev/null attach -t fleet-test-attached",
        s.tmux.display(),
        s.socket
    );
    let mut cmd = Command::new(script);
    if cfg!(target_os = "macos") {
        cmd.args(["-q", "/dev/null", "/bin/sh", "-c", &attach]);
    } else {
        cmd.args(["-qc", &attach, "/dev/null"]);
    }
    let mut client = cmd
        .env("TMUX_TMPDIR", s.env.dir.path())
        .env("HOME", s.env.path("home"))
        .env("TERM", "xterm")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while s
        .tmux(&["list-clients", "-t", "fleet-test-attached"])
        .is_empty()
    {
        if Instant::now() > deadline {
            let _ = client.kill();
            let _ = client.wait();
            return; // no pty to attach through here: nothing to test
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("human_attached")),
        "{v}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
    let (code, v, err) = s.fleet(&[
        "answer",
        &pid,
        "--choice",
        "3",
        "--allow-attached",
        "--json",
    ]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(read(&out).trim(), "picked=3");
    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn a_stale_question_id_with_the_same_wording_is_refused() {
    let Some(s) = Server::new() else { return };
    // The same question asked twice: the session restarted on the first (a new prompt came).
    let input = json!({ "questions": [{ "question": "Which city?", "header": "City",
        "options": [{ "label": "Paris", "description": "The French capital" },
                    { "label": "Rome", "description": "The Italian capital" }] }] });
    let ask = |id: &str, msg: &str| {
        json!({ "type": "assistant", "message": { "id": msg, "role": "assistant", "stop_reason": "tool_use",
        "content": [{ "type": "tool_use", "id": id, "name": "AskUserQuestion", "input": input }] } })
    };
    let lines = [
        json!({ "type": "user", "message": { "role": "user", "content": "plan the trip" } }),
        ask("tq-old", "m1"),
        json!({ "type": "user", "message": { "role": "user", "content": "plan the trip, again" } }),
        ask("tq-new", "m2"),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(s.env.path("home/.claude/projects/-w/s-stale.jsonl"), text).unwrap();
    let (log, out) = s.files("stale");
    let pid = s.agent_cmd(
        "stale",
        &format!(
            "'{}' single '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-stale",
        "Enter to select",
    );
    let (code, v, _) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq-old",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("question_not_active")),
        "{v}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
    let (code, v, err) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq-new",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(read(&out).trim(), "row=1 typed= ticked=00");
}
