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
/// mode (`replace`: the answer reveals another prompt with the same answers — a queued one;
/// `vanish`: the answer ends the agent, and with it its pane and tmux server).
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
           [ "$mode" = vanish ] && exit
           if [ "$mode" = replace ] && [ "$cmd" = "rm -rf build" ]; then cmd="rm -rf dist"; cur=0
           else printf '\033[2J\033[H● done\n'; sleep 60; exit; fi ;;
    $'\n'|$'\r') echo "picked=$((cur+1))" > "$out"; printf '\033[2J\033[H● done\n'; sleep 60; exit ;;
  esac
  draw
done
"#;

/// A Codex approval prompt (wording synthetic) under its last conversation item, drawn from
/// column 0. Its `y` answer clears the pane, scrollback and all, and the next approval — the
/// same answers, a queued one — comes up with no line at column 0 on the captured text.
/// Args: key log, answer file.
const FAKE_CODEX: &str = r#"#!/bin/bash
log="$1"; out="$2"
approval() {
  echo "  Would you like to run the following command?"
  echo
  echo "  \$ $1"
  echo
  echo "› 1. Yes, proceed (y)"
  echo "  2. No, and tell Codex what to do differently (esc)"
  echo
  echo "  Press enter to confirm or esc to cancel"
}
stty -echo -icanon min 1 time 0
printf '\033[2J\033[H'
echo "› Clean the build output"
echo
echo "• Running rm -rf build"
echo
approval "rm -rf build"
while IFS= read -r -s -n1 -d '' c; do
  printf '%s' "$c" >> "$log"
  if [ "$c" = y ]; then
    echo "picked=y" >> "$out"
    printf '\033[2J\033[3J\033[H'
    approval "rm -rf dist"
  fi
done
"#;

/// A Claude Code 2.1 question form (one question, "Which city?", options Paris and Rome) that
/// behaves like the real one: ↑/↓ move the cursor, typing on "Type something" fills it in,
/// Enter picks (single-select) or ticks (multi-select; on "Submit" it submits).
/// Args: single|multi, key log, answer file, mischief (1: the cursor jumps a row by itself
/// after the first key — a human pressing ↓ mid-answer; 2: Rome starts ticked; 3: the agent
/// exits on the first key, and with it its pane).
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
  [ "$mischief" = 3 ] && exit
  if [ "$mischief" = 1 ] && [ $keys -eq 1 ]; then cur=$((cur+1)); fi
  draw
done
"#;

/// Drives web/lib/api.mjs as the web server does, with the real `fleet` CLI under it. Args:
/// web/lib, the `fleet` binary, the session id, `[[action, body], …]`.
const WEB_POST: &str = r#"
import { Readable } from 'node:stream';
import { pathToFileURL } from 'node:url';
const [lib, bin, id, requests] = process.argv.slice(2);
const mod = (f) => import(pathToFileURL(`${lib}/${f}`).href);
const { createApi } = await mod('api.mjs');
const { createFleetCli } = await mod('fleet-cli.mjs');
const { createFleet } = await mod('fleet.mjs');
const { createBackend } = await mod('backends.mjs');
const { normalizeConfig } = await mod('config.mjs');
const { run } = await mod('run.mjs');
const cli = createFleetCli({ run, bin });
const config = normalizeConfig({ self: 'solo' }, { env: {}, home: process.env.HOME });
const api = createApi({ config, fleet: createFleet({ cli, self: 'solo' }), cli, backend: createBackend({ run }), transcripts: {}, spawner: {}, warmFleet: false });
const out = [];
for (const [action, body] of JSON.parse(requests)) {
  const req = Readable.from([Buffer.from(JSON.stringify(body))]);
  req.method = 'POST';
  req.headers = { 'content-type': 'application/json' };
  try {
    const res = await api(req, new URL(`http://localhost/api/hosts/solo/sessions/${id}/${action}`));
    out.push({ status: res.status, body: res.body });
  } catch (e) {
    out.push({ status: e.status ?? 500, body: { error: String(e.message) } });
  }
}
console.log(JSON.stringify(out));
api.stop?.();
process.exit(0);
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
        std::fs::write(env.path("bin/codex"), FAKE_CODEX).unwrap();
        std::fs::write(
            env.path("bin/static"),
            "#!/bin/bash\ncat \"$1\"\nstty -echo -icanon min 1 time 0\nwhile IFS= read -r -s -n1 -d '' c; do printf '%s' \"$c\" >> \"$2\"; done\n",
        )
        .unwrap();
        for f in [
            "bin/tmux",
            "bin/permission",
            "bin/form",
            "bin/codex",
            "bin/static",
        ] {
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
        self.agent_as(name, cmd, session_id, ready, "waiting")
    }

    /// [`Server::agent`] as a Codex session: the process named `codex` (as `ps` shows it), a
    /// Codex home under the temp `HOME`, no Claude registry entry.
    fn codex_agent(&self, name: &str, script: &str, args: &[&Path], ready: &str) -> String {
        std::fs::create_dir_all(self.env.path("home/.codex")).unwrap();
        let quoted: Vec<String> = std::iter::once(self.env.path(&format!("bin/{script}")))
            .chain(args.iter().map(|p| p.to_path_buf()))
            .map(|p| format!("'{}'", p.display()))
            .collect();
        let cmd = format!(
            "exec /bin/bash -c \"exec -a codex /bin/bash {}\"",
            quoted.join(" ")
        );
        let pid = self.agent_cmd(name, &cmd, "s-unused", ready);
        std::fs::remove_file(self.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
        pid
    }

    /// [`Server::agent_cmd`], registered with `status` (what the registry says the session is
    /// doing, whatever its screen shows).
    fn agent_as(
        &self,
        name: &str,
        cmd: &str,
        session_id: &str,
        ready: &str,
        status: &str,
    ) -> String {
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
                          "status": status, "kind": "interactive", "name": name });
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

    /// The web server's session routes (web/lib/api.mjs, with the real `fleet` CLI under it)
    /// against this server: POST each `[action, body]` of `requests` to session `id`, in
    /// order; the `{ status, body }` of each. `None` without node.
    fn web(&self, id: &str, requests: Value) -> Option<Vec<Value>> {
        let node = fleet::core::tools::find_binary("node")?;
        let script = self.env.path("web-post.mjs");
        std::fs::write(&script, WEB_POST).unwrap();
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/lib");
        // The `fleet` the web server runs is isolated exactly like the tests' own.
        let fleet = self.env.std_cmd();
        let mut c = Command::new(node);
        for (k, v) in fleet.get_envs() {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        let out = c
            .env("HOME", self.env.path("home"))
            .env("TMUX_TMPDIR", self.env.dir.path())
            .env("FLEET_TMUX", self.env.path("bin/tmux"))
            .env("FLEET_LOCK_DIR", self.env.path("locks"))
            .env("CODEX_HOME", self.env.path("home/.codex"))
            .env("PI_CODING_AGENT_DIR", self.env.path("home/.pi"))
            .arg(&script)
            .arg(lib)
            .arg(fleet.get_program())
            .arg(id)
            .arg(requests.to_string())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
            panic!(
                "web: {stdout} {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
        });
        Some(v.as_array().unwrap().clone())
    }

    /// A transcript whose last call is a pending "Which city?" question `tq1`.
    fn question_transcript(&self, session_id: &str, multi: bool) {
        let input = json!({ "questions": [{ "question": "Which city?", "header": "City", "multiSelect": multi,
            "options": [{ "label": "Paris", "description": "The French capital" },
                        { "label": "Rome", "description": "The Italian capital" }] }] });
        self.question_transcript_of(session_id, input);
    }

    /// A transcript whose last call is a pending `AskUserQuestion` `tq1` with this input.
    fn question_transcript_of(&self, session_id: &str, input: Value) {
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
    assert_eq!(v["answerable"], true, "{v}");
    assert_eq!(v["detail"], Value::Null);
    assert_eq!(v["pane"], json!({ "cols": 200, "rows": 50 }));
    let id = p["id"].as_str().unwrap().to_string();

    // Refusals type nothing. Each says which check failed as a code (`detail`, `null` for a
    // refusal that is no screen check) and how big the pane is (`null` before the target is
    // known).
    let pane = json!({ "cols": 200, "rows": 50 });
    let refused: [(&[&str], &str, Value, Value); 3] = [
        (
            &["--choice", "2", "--prompt", "p2.00000000"],
            "prompt_not_on_screen",
            json!("different_prompt"),
            pane.clone(),
        ),
        (&["--choice", "4"], "choice_invalid", Value::Null, pane),
        (&["--question", "tq"], "usage", Value::Null, Value::Null),
    ];
    for (args, kind, detail, pane) in refused {
        let mut all = vec!["answer", pid.as_str(), "--json"];
        all.extend_from_slice(args);
        let (code, v, _) = s.fleet(&all);
        assert_eq!((code, v["kind"].as_str()), (1, Some(kind)), "{args:?}");
        assert_eq!(
            (&v["detail"], &v["pane"]),
            (&detail, &pane),
            "{args:?}: {v}"
        );
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
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(false), &json!("no_form"))
    );
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "1", "--json"]);
    assert_eq!(
        (code, v["kind"].as_str(), v["detail"].as_str()),
        (1, Some("prompt_not_on_screen"), Some("no_form"))
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
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(true), &Value::Null)
    );

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
    assert_eq!(v["detail"], "cursor_moved", "{v}");
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
    assert_eq!(v["detail"], "touched", "{v}");
    assert_eq!(v["pane"], json!({ "cols": 200, "rows": 50 }), "{v}");
    // `--list` names the question, and says it cannot be answered now, and why.
    let (code, v, _) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["prompt"]["id"], "tq1", "{v}");
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(false), &json!("touched"))
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
}

// SYNTHETIC screens: the "Which city?" form of `FAKE_FORM`, cut as a fullscreen TUI cuts a form
// taller than its pane (no scrollback above the screen). Not Claude Code 2.1.303 captures; real
// ones come from a live test.
const RULE: &str = "────────────────────────────────────────────────────────────";

/// Row 1 off the top: the rows do not run up to it.
fn cut_above_row_2() -> String {
    format!(
        "     The French capital\n  2. Rome\n     The Italian capital\n  3. Type something.\n\
         {RULE}\n  4. Chat about this\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel\n"
    )
}

/// The rule and the tab bar off the top, the question whole: what is there reads as the
/// question, but a tab answered up there could not be seen.
fn cut_above_question() -> String {
    format!("Which city?\n\n❯ 1. Paris\n{}", cut_above_row_2())
}

// The refusal from the field: a form taller than the pane, its top cut off. It is named as
// such — `prompt_incomplete`, `clipped` — with the pane's size, and `--list` says the form
// cannot be answered now, and why.
#[test]
fn a_form_cut_off_by_the_pane_is_named_clipped() {
    let Some(s) = Server::new() else { return };
    let pane = json!({ "cols": 200, "rows": 50 });
    // Either way the same object: `incomplete`, no id — even where what is visible reads as
    // the pending question (the head cut off): a caller must not offer an answer for it.
    for (name, screen) in [
        ("rows", cut_above_row_2()),
        ("head", cut_above_question()),
        // Nothing above row 1: no question text, so no pending question matches and the newest
        // one's own check names the cut.
        ("question", format!("❯ 1. Paris\n{}", cut_above_row_2())),
    ] {
        let sid = format!("s-cut-{name}");
        s.question_transcript(&sid, false);
        let (log, _) = s.files(&format!("cut-{name}"));
        let file = s.env.path(&format!("cut-{name}.screen"));
        std::fs::write(&file, &screen).unwrap();
        let pid = s.agent(
            &format!("cut-{name}"),
            "static",
            &[&file, &log],
            &sid,
            "Enter to select",
        );
        let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            v["prompt"],
            json!({ "kind": "question", "incomplete": true, "id": null, "questions": null,
                    "active": null, "untouched": false }),
            "{name}: {v}"
        );
        assert_eq!(v["answerable"], false, "{name}: {v}");
        assert_eq!(v["detail"], "clipped", "{name}: {v}");
        assert_eq!(v["pane"], pane, "{name}: {v}");
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
            (code, v["kind"].as_str(), v["detail"].as_str()),
            (1, Some("prompt_incomplete"), Some("clipped")),
            "{name}: {v}"
        );
        assert_eq!(v["pane"], pane, "{name}: {v}");
        // The new fields carry no screen or transcript text.
        let fields = format!("{}{}", v["detail"], v["pane"]);
        for word in ["Paris", "Rome", "city", "capital"] {
            assert!(!fields.contains(word), "{name}: {fields}");
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(read(&log), "", "{name}: nothing typed");
    }
}

// A single-select question with previews is drawn in the preview layout (C2), never plainly:
// a plain form showing its words is one fleet cannot read for sure (no capture shows it) —
// `layout_unsupported`, never `options` or "another question"; nothing typed.
#[test]
fn a_question_with_previews_drawn_plainly_is_not_that_question() {
    let Some(s) = Server::new() else { return };
    let input = json!({ "questions": [{ "question": "Which city?", "header": "City",
        "options": [{ "label": "Paris", "description": "The French capital", "preview": "Seine, Louvre" },
                    { "label": "Rome", "description": "The Italian capital", "preview": "Tiber, Forum" }] }] });
    s.question_transcript_of("s-preview", input);
    let (log, out) = s.files("preview");
    let pid = s.agent_cmd(
        "preview",
        &format!(
            "'{}' single '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-preview",
        "Enter to select",
    );
    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["prompt"]["kind"], "question", "{v}");
    assert_eq!(v["prompt"]["id"], Value::Null, "{v}");
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(false), &json!("layout_unsupported"))
    );
    let (code, v, _) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!(
        (code, v["kind"].as_str(), v["detail"].as_str()),
        (1, Some("prompt_incomplete"), Some("layout_unsupported")),
        "{v}"
    );
    assert!(
        !v["error"].as_str().unwrap().contains("another question"),
        "{v}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "", "nothing typed");
}

/// `testdata/screens/live/claude-question-preview-<name>`: a recorded Claude Code 2.1.312
/// screen (`.ans`, SGR kept) or `AskUserQuestion` input (`-input.json`).
fn recorded(name: &str) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens/live");
    std::fs::read_to_string(dir.join(format!("claude-question-preview-{name}"))).unwrap()
}

impl Server {
    /// A `static` agent showing `screen`, its transcript's pending question `tq1` the recorded
    /// input `input`: (pid, key log).
    fn showing(&self, name: &str, screen: &str, input: &str) -> (String, PathBuf) {
        let input: Value = serde_json::from_str(&recorded(&format!("{input}-input.json"))).unwrap();
        let session = format!("s-{name}");
        self.question_transcript_of(&session, input);
        let (log, _) = self.files(name);
        let file = self.env.path(&format!("{name}.screen"));
        std::fs::write(&file, screen).unwrap();
        let pid = self.agent(name, "static", &[&file, &log], &session, "Enter to select");
        (pid, log)
    }
}

// C4 / C5 end to end on recorded screens: `--list` reads a preview form as any question form
// (answerable when untouched, `preview` in its questions: C1), `--question` plans ↓ and Enter
// and takes exactly one option, no text. Dry runs only: a recorded screen does not react.
#[test]
fn a_recorded_preview_form_is_answerable() {
    let Some(s) = Server::new() else { return };
    let (pid, log) = s.showing("prev-a", &recorded("A-initial.ans"), "A");
    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(true), &Value::Null),
        "{v}"
    );
    let p = &v["prompt"];
    assert_eq!(
        (&p["kind"], &p["id"], &p["active"], &p["untouched"]),
        (&json!("question"), &json!("tq1"), &json!(0), &json!(true))
    );
    let input: Value = serde_json::from_str(&recorded("A-input.json")).unwrap();
    for i in 0..3 {
        assert_eq!(
            p["questions"][0]["options"][i]["preview"],
            input["questions"][0]["options"][i]["preview"],
            "{i}"
        );
    }
    let (code, v, err) = s.fleet(&[
        "--dry-run",
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[2]}]",
        "--json",
    ]);
    assert_eq!((code, &v["dry_run"]), (0, &json!(true)), "{err} {v}");
    assert_eq!(
        v["steps"],
        json!([{ "key": "Down" }, { "key": "Down" }, { "key": "Enter" }])
    );
    for answers in [
        "[{\"text\":\"A wizard\"}]",
        "[{\"options\":[0],\"text\":\"a note\"}]",
    ] {
        let (code, v, _) = s.fleet(&[
            "answer",
            &pid,
            "--question",
            "tq1",
            "--answers",
            answers,
            "--json",
        ]);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("answer_invalid")),
            "{v}"
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "", "nothing typed");
}

// C4 on the two-question scratch form and the live four-question form (its question in the
// `│ ` gutter, C2a): answerable, and the live answer planned across both layouts.
#[test]
fn recorded_forms_with_several_questions_are_answerable() {
    let Some(s) = Server::new() else { return };
    for (name, screen, input) in [
        ("prev-c", "C-initial.ans", "C"),
        ("prev-live", "N1-q1.ans", "N1"),
        ("prev-h4", "H4-q1.ans", "H4"),
    ] {
        let (pid, log) = s.showing(name, &recorded(screen), input);
        let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
        assert_eq!(code, 0, "{name}: {err}");
        assert_eq!(
            (&v["answerable"], &v["prompt"]["id"]),
            (&json!(true), &json!("tq1")),
            "{name}: {v}"
        );
        if name == "prev-live" {
            let (code, v, _) = s.fleet(&[
                "--dry-run",
                "answer",
                &pid,
                "--question",
                "tq1",
                "--answers",
                "[{\"options\":[1]},{\"options\":[0]},{\"options\":[1]},{\"text\":\"Ask first\"}]",
                "--json",
            ]);
            assert_eq!(code, 0, "{v}");
            let keys: Vec<String> = v["steps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    s["key"]
                        .as_str()
                        .map_or_else(|| format!("{}", s["text"]), str::to_string)
                })
                .collect();
            assert_eq!(
                keys.join(" "),
                "Down Enter Enter Down Enter Down Down \"Ask first\" Enter Enter"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(read(&log), "", "{name}: nothing typed");
    }
}

// C2's last paragraph end to end: the preview layout's footer over rows fleet cannot walk (no
// `┌` on option 1's row) is `layout_unsupported`, never `no_form`; nothing typed.
#[test]
fn a_preview_form_fleet_cannot_walk_is_layout_unsupported() {
    let Some(s) = Server::new() else { return };
    let plain: String = recorded("A-initial.ans")
        .lines()
        .map(|l| fleet::core::backend::strip_ansi(l) + "\n")
        .collect();
    let damaged = plain.replacen("with      ┌", "with       ", 1);
    assert_ne!(damaged, plain);
    let (pid, log) = s.showing("prev-bad", &damaged, "A");
    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        (&v["answerable"], &v["detail"]),
        (&json!(false), &json!("layout_unsupported")),
        "{v}"
    );
    assert_eq!(
        (
            &v["prompt"]["kind"],
            &v["prompt"]["incomplete"],
            &v["prompt"]["id"]
        ),
        (&json!("question"), &json!(true), &Value::Null)
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
        (code, v["kind"].as_str(), v["detail"].as_str()),
        (1, Some("prompt_incomplete"), Some("layout_unsupported")),
        "{v}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "", "nothing typed");
}

// C4: a multi-select question with previews is drawn plainly (capture E) and answered as a
// plain form, key by key.
#[test]
fn a_multi_select_question_with_previews_is_answered_as_a_plain_form() {
    let Some(s) = Server::new() else { return };
    let input = json!({ "questions": [{ "question": "Which city?", "header": "City", "multiSelect": true,
        "options": [{ "label": "Paris", "description": "The French capital", "preview": "Seine, Louvre" },
                    { "label": "Rome", "description": "The Italian capital", "preview": "Tiber, Forum" }] }] });
    s.question_transcript_of("s-mprev", input);
    let (log, out) = s.files("mprev");
    let pid = s.agent_cmd(
        "mprev",
        &format!(
            "'{}' multi '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-mprev",
        "Enter to select",
    );
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
    assert_eq!(keys(&log), format!("{DOWN}\n{DOWN}{DOWN}\n"));
    assert_eq!(read(&out).trim(), "row=3 typed= ticked=01");
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
    // Every answer failure carries `detail` and `pane`: `null` before any screen check.
    let fields = v.as_object().unwrap();
    for k in ["detail", "pane"] {
        assert_eq!(fields.get(k), Some(&Value::Null), "{k}: {v}");
    }
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

// Codex: the key clears the pane, and the next approval — the same answers — comes up with no
// line at column 0 captured, so it cannot be read whole. Taken for "no prompt", that once
// reported the answer as landed.
#[test]
fn a_key_that_reveals_a_prompt_not_read_whole_is_not_claimed_as_answered() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("cleared");
    let pid = s.codex_agent(
        "cleared",
        "codex",
        &[&log, &out],
        "Would you like to run the following command?",
    );
    let (code, v, err) = s.fleet(&["answer", &pid, "--list", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["harness"], "codex", "{v}");
    assert_eq!(v["prompt"]["detail"], json!(["$ rm -rf build"]), "{v}");
    let id = v["prompt"]["id"].as_str().unwrap().to_string();

    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "1", "--prompt", &id, "--json"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert_ne!(v["answered"], true);
    assert_eq!(v["method"], "key");
    assert_eq!(v["screen"]["incomplete"], true, "{v}");
    assert!(
        v["screen"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("off the captured screen"),
        "{v}"
    );
    assert_eq!(keys(&log), "y", "one key went out");
    assert_eq!(read(&out).trim(), "picked=y");
}

// The pane (and the tmux server) gone right after the key: its captures fail, and an empty
// failed capture once read as "the prompt is gone" — answered.
#[test]
fn a_terminal_gone_after_the_key_is_never_claimed_as_answered() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("vanish");
    let pid = s.agent_cmd(
        "vanish",
        &format!(
            "'{}' '{}' '{}' vanish",
            s.env.path("bin/permission").display(),
            log.display(),
            out.display()
        ),
        "s-vanish",
        "Do you want to proceed?",
    );
    let (code, v, _) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert_ne!(v["answered"], true);
    assert!(
        v["error"].as_str().unwrap_or("").contains("capture-pane"),
        "{v}"
    );
    assert_eq!(read(&out).trim(), "picked=3", "the key went out");
}

// The registry says the session is busy (no hold for a Claude rename), but its screen shows a
// permission prompt that came up since: under the lock the screen is read again, and the
// rename is held — `/rename …` and its Enter would have answered the prompt.
#[test]
fn a_rename_reads_the_screen_again_under_the_lock() {
    let Some(s) = Server::new() else { return };
    let (log, out) = s.files("reread");
    let pid = s.agent_as(
        "reread",
        &format!(
            "'{}' '{}' '{}'",
            s.env.path("bin/permission").display(),
            log.display(),
            out.display()
        ),
        "s-reread",
        "Do you want to proceed?",
        "busy",
    );
    let (code, _, err) = s.fleet(&["rename", &pid, "probe", "--no-tmux-sync"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("waiting"), "{err}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "", "nothing typed into the prompt");
    assert_eq!(read(&out), "");
}

// The form's pane exits on the first key: the next read fails. That is `io`, saying how many
// keys went out — never `answered`.
#[test]
fn a_form_gone_after_the_first_key_says_how_many_keys_went_out() {
    let Some(s) = Server::new() else { return };
    s.question_transcript("s-gone", false);
    let (log, out) = s.files("gone");
    let pid = s.agent_cmd(
        "gone",
        &format!(
            "'{}' single '{}' '{}' 3",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-gone",
        "Enter to select",
    );
    let (code, v, _) = s.fleet(&[
        "answer",
        &pid,
        "--question",
        "tq1",
        "--answers",
        "[{\"options\":[1]}]",
        "--json",
    ]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("io")), "{v}");
    // A key went out before the screen could no longer be read.
    assert_eq!(v["typed"], true, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("stopped after 1 of 2 keys"),
        "{v}"
    );
    assert_ne!(v["answered"], true);
    assert_eq!(keys(&log), DOWN, "one key went out");
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
    // So does a rename, even forced past the waiting hold: `/rename …` and its Enter never
    // land between an answer's keys.
    let (code, _, err) = s.fleet(&["rename", &pid, "probe", "--force", "--no-tmux-sync"]);
    assert_eq!(code, 1);
    assert!(err.contains("another fleet command"), "{err}");
    // With `--json`, both say so as one object: kind `busy` (the web server maps it to a 409).
    let (code, v, _) = s.fleet(&["send", "--json", "--", &pid, "hello"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("busy")), "{v}");
    let (code, v, _) = s.fleet(&[
        "rename",
        &pid,
        "probe",
        "--force",
        "--no-tmux-sync",
        "--json",
    ]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("busy")), "{v}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "");
    drop(held);
    let (code, v, err) = s.fleet(&["answer", &pid, "--choice", "3", "--json"]);
    assert_eq!(code, 0, "{err} {v}");
    assert_eq!(read(&out).trim(), "picked=3");
}

// The web server's `send`, `keys` and `answer` once typed straight into tmux, past the lock:
// their keys could land between an agent's `fleet answer` keys. Now they type through the CLI,
// under the same lock: `busy` (409), nothing typed, while another command holds it.
#[test]
fn the_web_server_types_under_the_same_lock() {
    let Some(s) = Server::new() else { return };
    if fleet::core::tools::find_binary("node").is_none() {
        return;
    }
    s.question_transcript("s-web", false);
    let (log, out) = s.files("web");
    s.agent_cmd(
        "web",
        &format!(
            "'{}' single '{}' '{}' 0",
            s.env.path("bin/form").display(),
            log.display(),
            out.display()
        ),
        "s-web",
        "Enter to select",
    );
    std::fs::create_dir_all(s.env.path("locks")).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(s.env.path("locks/s-web.lock"))
        .unwrap();
    held.lock().unwrap();
    let answer = json!(["answer", { "toolUseId": "tq1", "answers": [{ "options": [1] }] }]);
    let res = s
        .web(
            "s-web",
            json!([["send", { "text": "hello" }], ["keys", { "key": "Down" }], answer]),
        )
        .unwrap();
    for r in &res {
        assert_eq!(
            (r["status"].as_i64(), r["body"]["kind"].as_str()),
            (Some(409), Some("busy")),
            "{r}"
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&log), "", "nothing typed while the lock is held");

    // Released: the same answer goes through the CLI's checked path, and lands.
    drop(held);
    let res = s.web("s-web", json!([answer])).unwrap();
    assert_eq!(res[0]["status"], 200, "{}", res[0]);
    assert_eq!(res[0]["body"]["answered"], true, "{}", res[0]);
    assert_eq!(read(&out).trim(), "row=1 typed= ticked=00");
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
