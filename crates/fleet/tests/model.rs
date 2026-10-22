//! `fleet model` against a fake Claude Code on the test's private tmux server. The fake draws
//! a captured screen (testdata/screens, `live/…` the live captures of Claude Code 2.1.303 in a
//! 110x34 pane), logs every byte it reads, and answers `/model <id>` + Enter as Claude Code
//! does, by `mode`:
//!
//! - `cold`: writes the switch into its transcript as Claude Code 2.1.306 does (`user` entries:
//!   a meta caveat, the command, then ``Set model to `Sonnet 5.5` …``, whatever id was asked
//!   for; `$FAKE_LABEL` in place of `Sonnet 5.5` when set) and draws its box again — the
//!   prompt cache cold, nothing asked;
//! - `warm`: draws the switch confirmation first (`live/claude-model-confirm`, `<log>.confirming`
//!   made just before), and switches once its first choice's key (`1`) arrives — or, once
//!   `<log>.decline` exists, declines it as a person in the terminal would: `Kept model as …`
//!   into its transcript (`system` / `local_command` entries), the box back
//!   (`live/claude-model-cancelled`), then `<log>.declined`;
//! - `hooks`: draws Claude Code's "Running PreModelSwitch hooks…" line where the box was for
//!   `$FAKE_HOOK_MS` (1500 ms), then switches as `cold` does;
//! - `other`: draws a confirmation for another model (Haiku 5.5) and waits;
//! - `notfound`: draws `Model '…' not found` under the command (`live/claude-model-not-found`),
//!   writing only the command's entry — so the screen alone tells it;
//! - `refused`: writes a `PreModelSwitch` hook's refusal into its transcript, in Claude Code's
//!   words, and draws it;
//! - `forged`: writes only a prompt that is exactly the switch's output (``Set model to `Sonnet
//!   5.5` …`` in `<local-command-stdout>`), as a person typing it would, no command entry
//!   before it, and draws its box again;
//! - `silent` (and any other): does nothing;
//! - `edits`: opens an "editor" at once, a child reading its terminal in its process group, as
//!   Claude Code's Ctrl-G editor does (its pid in `<log>.editor`).
//!
//! `FAKE_BLANKS=<n>` puts the terminal's cursor `n` cells past where the box's text starts, as
//! blanks typed there leave it; `FAKE_HIDE=1` hides the cursor.
//!
//! These tests need tmux and perl, and fail without them rather than pass having run nothing.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fake::{FakeFleet, JOB_SHELL, proc_start};
use common::switch::{self, failed, keys_of};
use common::{stderr, stdout};
use serde_json::Value;

const SID: &str = "dddddddd-1111-2222-3333-444444444444";
const SONNET: &str = "claude-sonnet-5-5";

/// Logs every byte it reads to the file it is given (a Codex or pi session's stand-in).
const LOGGER: &str = r#"open(my $f, ">>", $ARGV[0]) or die; select((select($f), $| = 1)[0]);
system("stty raw -echo");
while (sysread(STDIN, my $b, 64)) { print $f $b }
"#;

const AGENT: &str = r#"
use strict; use Time::HiRes qw(time sleep); use POSIX qw(strftime);
my ($log, $mode, $screens, $start, $transcript, $pidfile) = @ARGV;
system("stty raw -echo");
sub draw {
  my ($name, @subs) = @_;
  open(my $s, "<", "$screens/$name.txt") or die; local $/; my $t = <$s>; close $s;
  while (@subs) { my ($from, $to) = splice(@subs, 0, 2); $t =~ s/\Q$from\E/$to/g; }
  $t =~ s/\n/\r\n/g; syswrite(STDOUT, "\e[H\e[2J$t");
}
sub esc { my $s = shift; $s =~ s/\\/\\\\/g; $s =~ s/"/\\"/g; $s =~ s/\n/\\n/g; $s =~ s/\e/\\u001b/g; $s }
# Each entry the parent of the next, as Claude Code chains them.
my ($entries, $last) = (0, "");
sub put {
  my $t = time; my $at = strftime("%Y-%m-%dT%H:%M:%S", gmtime($t)) . sprintf(".%03dZ", int(($t - int($t)) * 1000));
  my $uuid = sprintf("aaaaaaaa-0000-4000-8000-%012d", ++$entries);
  my $parent = $last eq "" ? "null" : "\"$last\""; $last = $uuid;
  open(my $f, ">>", $transcript) or die;
  print $f '{"parentUuid":' . $parent . ',"uuid":"' . $uuid . '",' . $_[0] . ',"timestamp":"' . $at . '","version":"2.1.306"}' . "\n";
  close $f;
}
# Claude Code 2.1.306's two shapes: a `system` / `local_command` entry (the command, then what it
# answered: not found, declined, a hook's refusal), and for a switch three `user` entries — a
# meta caveat, the command, its output.
sub entry { put('"isSidechain":false,"type":"system","subtype":"local_command","content":"' . esc($_[0]) . '","level":"info","isMeta":false') }
sub user { put('"isSidechain":false,"type":"user","message":{"role":"user","content":"' . esc($_[0]) . '"}' . ($_[1] ? ',"isMeta":true' : '')) }
sub command { "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args>$_[0]</command-args>" }
sub reply { entry("<local-command-stdout>$_[0]</local-command-stdout>") }
sub switched {
  my ($label, $id) = @_;
  user("<local-command-caveat>An example caveat.</local-command-caveat>", 1);
  user(command($id));
  user("<local-command-stdout>Set model to `$label` and saved as your default for new sessions</local-command-stdout>");
  draw("live/claude-after-turn");
}
draw($start);
# FAKE_BLANKS=<n>: n blanks typed into the box, which Claude Code draws as nothing — only the
# terminal's cursor, past where the box's text starts, shows them. (The screen's last newline
# scrolled it up a row in the 34-row pane.)
if ($ENV{FAKE_BLANKS}) {
  open(my $s, "<", "$screens/$start.txt") or die; local $/; my $t = <$s>; close $s;
  my @l = split /\n/, $t; my $n = () = $t =~ /\n/g; my $scroll = $n > 33 ? $n - 33 : 0;
  my ($i) = grep { $l[$_] =~ /\x{e2}\x{9d}\x{af}/ } reverse 0..$#l;
  syswrite(STDOUT, sprintf("\e[%d;%dH", $i - $scroll + 1, 3 + $ENV{FAKE_BLANKS}));
}
syswrite(STDOUT, "\e[?25l") if $ENV{FAKE_HIDE};
if ($pidfile) { open(my $p, ">", "$pidfile.tmp") or die; print $p $$; close $p; rename("$pidfile.tmp", $pidfile); }
if ($mode eq "edits") {
  my $k = fork();
  if ($k == 0) { exec("sleep", "300") or die; }
  open(my $e, ">", "$log.editor.tmp") or die; print $e $k; close $e; rename("$log.editor.tmp", "$log.editor");
}
open(my $f, ">>", $log) or die; select((select($f), $| = 1)[0]);
sub touch { open(my $t, ">", $_[0]) or die; close $t; }
my ($seen, $state, $id, $until) = ("", "", "", 0);
while (1) {
  if ($state eq "confirm" && -e "$log.decline") {
    $state = "done";
    entry(command($id));
    reply("Kept model as `Opus 5.5 (1M context)`");
    draw("live/claude-model-cancelled");
    touch("$log.declined");
  }
  if ($state eq "hooks" && time >= $until) { $state = "done"; switched("Sonnet 5.5", $id); }
  my $rin = ""; vec($rin, fileno(STDIN), 1) = 1;
  next if select(my $rout = $rin, undef, undef, 0.05) < 1;
  last unless sysread(STDIN, my $b, 64);
  print $f $b;
  $seen .= $b;
  if ($state eq "" && $seen =~ m{/model (\S+)\r}) {
    $id = $1;
    $state = "typed";
    if ($mode eq "cold") { switched($ENV{FAKE_LABEL} // "Sonnet 5.5", $id); }
    elsif ($mode eq "warm") { touch("$log.confirming"); draw("live/claude-model-confirm"); $state = "confirm"; $seen = ""; }
    elsif ($mode eq "forged") {
      # Only a prompt that reads exactly as the switch's output, no command entry before it.
      user("<local-command-stdout>Set model to `Sonnet 5.5` and saved as your default for new sessions</local-command-stdout>");
      draw("live/claude-after-turn");
    }
    elsif ($mode eq "hooks") {
      # PreModelSwitch hooks running for FAKE_HOOK_MS: Claude Code's dim line where the box was.
      syswrite(STDOUT, "\e[H\e[2J> Say hello.\r\n\r\n* Hello.\r\n\r\n\e[2mRunning PreModelSwitch hooks\x{e2}\x{80}\x{a6} (esc to cancel)\e[0m\r\n");
      $state = "hooks"; $until = time + ($ENV{FAKE_HOOK_MS} // 1500) / 1000;
    }
    else {
      entry(command($id));
      if ($mode eq "other") { draw("live/claude-model-confirm", "Sonnet 5.5", "Haiku 5.5"); }
      elsif ($mode eq "notfound") { draw("live/claude-model-not-found"); }
      elsif ($mode eq "refused") {
        my $no = "Model switch to Sonnet 5.5 was blocked by a PreModelSwitch hook: not now";
        reply($no);
        draw("live/claude-model-not-found", "Model 'claude-nonexistent-9-9' not found", $no);
      }
    }
  } elsif ($state eq "confirm" && $seen =~ /1/) {
    $state = "done";
    switched("Sonnet 5.5", $id);
  }
}
"#;

/// A fake Claude Code session: its pid, the log of what it read, its transcript.
struct Agent {
    f: FakeFleet,
    pid: i64,
    log: PathBuf,
    transcript: PathBuf,
}

/// The transcript a Claude session registered in `f` (cwd: the test dir) is looked up at.
fn transcript_of(f: &FakeFleet) -> PathBuf {
    switch::transcript_of(f, SID)
}

/// The reply its transcript starts with: `model` came from it, at this time.
const REPLY_AT: &str = "2026-01-02T03:04:30.000Z";

/// What the agent runs, for `mode` on screen `start`, writing its pid to `pidfile` if given;
/// with its transcript holding one reply.
fn agent_line(f: &FakeFleet, mode: &str, start: &str, pidfile: Option<&Path>) -> String {
    assert!(
        fleet::core::tools::find_binary("perl").is_some(),
        "these tests need perl"
    );
    let transcript = transcript_of(f);
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let reply = serde_json::json!({
        "type": "assistant", "timestamp": REPLY_AT,
        "message": { "id": "msg_example", "role": "assistant", "model": "claude-opus-5-5",
            "content": [{ "type": "text", "text": "Hello." }], "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "cache_read_input_tokens": 2000, "cache_creation_input_tokens": 0, "output_tokens": 3 } }
    });
    std::fs::write(&transcript, format!("{reply}\n")).unwrap();
    let script = f.env.path("agent.pl");
    std::fs::write(&script, AGENT).unwrap();
    let screens = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens");
    format!(
        "perl '{}' '{}' '{mode}' '{}' '{start}' '{}' '{}'",
        script.display(),
        f.env.path("received.log").display(),
        screens.display(),
        transcript.display(),
        pidfile.map(|p| p.display().to_string()).unwrap_or_default(),
    )
}

/// A fake Claude Code in `mode`, the pane's own process, showing `start` (an empty box:
/// `live/claude-empty`, its placeholder faint).
fn agent(mode: &str, start: &str) -> Agent {
    let f = FakeFleet::new().expect("these tests need tmux");
    let line = agent_line(&f, mode, start, None);
    let pid = f.agent("model", SID, 110, 34, &format!("exec {line}"));
    let log = f.env.path("received.log");
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent {
        transcript: transcript_of(&f),
        f,
        pid,
        log,
    }
}

/// [`agent`], the fake run with `env` (`NAME=value …`, for `env`) set.
fn agent_env(mode: &str, start: &str, env: &str) -> Agent {
    let f = FakeFleet::new().expect("these tests need tmux");
    let line = agent_line(&f, mode, start, None);
    let pid = f.agent("model", SID, 110, 34, &format!("exec env {env} {line}"));
    let log = f.env.path("received.log");
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent {
        transcript: transcript_of(&f),
        f,
        pid,
        log,
    }
}

/// `fleet <args>` with a tmux that first runs `before` (sh, `$*` its arguments) on every call:
/// (exit code, the one JSON object on stdout, stderr, how long it ran).
fn under_tmux(a: &Agent, before: &str, args: &[&str]) -> (i32, Value, String, Duration) {
    switch::under_tmux(&a.f, before, args)
}

/// sh for [`under_tmux`]: the first time the panes' terminals are listed — the lookup's last
/// step, after it read the registry, before the lock — rewrite the agent's registry entry with
/// perl's `s/…/…/` `edit`, once.
fn once_at_lookup(a: &Agent, edit: &str) -> String {
    let fired = a.f.env.path("edited");
    format!(
        "case \"$*\" in *list-panes*pane_tty*)\n\
         if [ ! -e '{fired}' ]; then : > '{fired}'; perl -pi -e '{edit}' '{reg}'; fi;;\n\
         esac",
        fired = fired.display(),
        reg =
            a.f.env
                .path(&format!("home/.claude/sessions/{}.json", a.pid))
                .display(),
    )
}

/// A fake Claude Code in `mode` started by a shell with job control ([`JOB_SHELL`]); once
/// `front` exists in the test dir, the shell brings `to_front` to the front (`job`, `shell`).
fn under_job_control(mode: &str, to_front: &str) -> (Agent, PathBuf) {
    let f = FakeFleet::new().expect("these tests need tmux");
    let pidfile = f.env.path("agent.pid");
    let line = agent_line(&f, mode, "live/claude-empty", Some(&pidfile));
    let shell = f.env.path("jobshell.pl");
    std::fs::write(&shell, JOB_SHELL).unwrap();
    let flag = f.env.path("front");
    let wrapped = format!(
        "exec perl '{}' \"{}\" '{}' '{to_front}'",
        shell.display(),
        line.replace('"', "\\\""),
        flag.display()
    );
    let pane = f.agent("model", SID, 110, 34, &wrapped);
    let log = f.env.path("received.log");
    f.wait_for("the agent to start", || log.exists() && pidfile.exists());
    let pid: i64 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(pid, pane);
    let _ = std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pane}.json")));
    f.register(pid, "model", SID, Some(&proc_start(pid)));
    (
        Agent {
            transcript: transcript_of(&f),
            f,
            pid,
            log,
        },
        flag,
    )
}

impl Agent {
    fn target(&self) -> String {
        format!("pid:{}", self.pid)
    }

    /// `fleet …` → (exit code, the one JSON object on stdout, stderr).
    fn fleet(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self.f.fleet(args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
        (out.status.code().unwrap_or(-1), v, stderr(&out))
    }

    /// `fleet model --json <target> <id> [extra…]`.
    fn model(&self, id: &str, extra: &[&str]) -> (i32, Value, String) {
        let t = self.target();
        let mut args = vec!["model", "--json", t.as_str(), id];
        args.extend(extra);
        self.fleet(&args)
    }

    /// What the agent has read, once a moment has passed for anything on its way.
    fn received(&self) -> String {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Set its registry entry's Status.
    fn status(&self, status: &str) {
        let reg = self
            .f
            .env
            .path(&format!("home/.claude/sessions/{}.json", self.pid));
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
        v["status"] = status.into();
        std::fs::write(&reg, v.to_string()).unwrap();
    }

    /// Its `list --json` row.
    fn listed(&self) -> Value {
        let (_, rows, err) = self.fleet(&["list", "--json"]);
        rows.as_array()
            .unwrap_or_else(|| panic!("{rows} {err}"))
            .iter()
            .find(|r| r["pid"] == self.pid)
            .unwrap_or_else(|| panic!("no row for {}: {rows}", self.pid))
            .clone()
    }

    /// The `timestamp` of the transcript's `Set model to` entry.
    fn switched_at(&self) -> String {
        let text = std::fs::read_to_string(&self.transcript).unwrap();
        let line = text
            .lines()
            .rfind(|l| l.contains("Set model to"))
            .unwrap_or_else(|| panic!("no switch in {text}"));
        let v: Value = serde_json::from_str(line).unwrap();
        v["timestamp"].as_str().unwrap().to_string()
    }
}

#[test]
fn a_cold_cache_switches_at_once() {
    let a = agent("cold", "live/claude-empty");
    // The row a caller acts on: its process, and the time of the reply its model came from.
    let row = a.listed();
    assert_eq!(row["model"], "claude-opus-5-5", "{row}");
    assert_eq!(row["model_at"], REPLY_AT, "{row}");
    let (pid, start) = (
        row["pid"].to_string(),
        row["proc_start"].as_str().unwrap().to_string(),
    );

    let (code, v, err) = a.model(SONNET, &["--expect-pid", &pid, "--expect-start", &start]);
    assert_eq!(code, 0, "{v} {err}");
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
        ],
        "{v}"
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["session_id"], SID);
    assert_eq!(v["pid"], a.pid);
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["model"], SONNET);
    assert_eq!(v["confirmed"], false, "nothing was asked");
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["switched_at"], a.switched_at().as_str(), "{v}");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
    // `model` is still the last reply's: the switch is no reply.
    assert_eq!(a.listed()["model_at"], REPLY_AT);

    // Text output says what it did.
    let a = agent("cold", "live/claude-empty");
    let (code, out, err) = a.fleet(&["model", &a.target(), SONNET]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.as_str().unwrap().contains(&format!("to {SONNET}")),
        "{out}"
    );
}

#[test]
fn a_warm_cache_is_confirmed_with_its_first_choice() {
    let a = agent("warm", "live/claude-after-turn");
    let (code, v, err) = a.model(SONNET, &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["confirmed"], true, "{v}");
    assert_eq!(v["switched_at"], a.switched_at().as_str(), "{v}");
    // The command, then the key the confirmation's first choice names: nothing else.
    assert_eq!(a.received(), format!("/model {SONNET}\r1"));
}

/// `fleet` with a tmux that, on every call, first tries the session's typing lock and logs
/// `held` or `free` with its arguments; the calls logged.
fn under_probe(f: &FakeFleet, args: &[&str]) -> (std::process::Output, Vec<(bool, String)>) {
    let probe = f.env.path("probe-tmux.sh");
    let calls = f.env.path("probe.log");
    std::fs::write(
        &probe,
        format!(
            "#!/bin/sh\n\
             state=$(perl -MFcntl=:flock -e 'open(my $f, \">>\", $ARGV[0]) or do {{ print \"none\"; exit }}; \
             print flock($f, LOCK_EX | LOCK_NB) ? \"free\" : \"held\"' '{lock}')\n\
             printf '%s %s\\n' \"$state\" \"$*\" >> '{calls}'\n\
             exec '{real}' \"$@\"\n",
            lock = f.env.path(&format!("locks/{SID}.lock")).display(),
            calls = calls.display(),
            real = f.env.tmux_wrapper().display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = f
        .env
        .cmd()
        .env("SHELL", "/bin/sh")
        .env("FLEET_TMUX", &probe)
        .timeout(Duration::from_secs(20))
        .args(args)
        .output()
        .unwrap();
    let log = std::fs::read_to_string(&calls).unwrap_or_default();
    let calls = log
        .lines()
        .map(|l| {
            let (state, call) = l.split_once(' ').unwrap_or((l, ""));
            (state == "held", call.to_string())
        })
        .collect();
    (out, calls)
}

// One critical section: the screen read under the lock (`capture-pane -e`), the command, and
// the confirmation's key — with `owns` looked at again (the pane's terminal, `list-panes`)
// between the command and that key — and nothing in between let go of the lock.
#[test]
fn the_lock_is_held_across_the_read_the_command_and_the_confirmation() {
    let a = agent("warm", "live/claude-empty");
    let t = a.target();
    let (out, calls) = under_probe(&a.f, &["model", &t, SONNET]);
    assert!(out.status.success(), "{}", stderr(&out));
    let read = calls
        .iter()
        .position(|(_, c)| c.contains("capture-pane") && c.contains("-e"))
        .unwrap_or_else(|| panic!("a styled read: {calls:?}"));
    let typing: Vec<usize> = (0..calls.len())
        .filter(|&i| calls[i].1.contains("send-keys"))
        .collect();
    let key = *typing.last().unwrap();
    assert!(calls[key].1.ends_with("-l -- 1"), "the key last: {calls:?}");
    assert!(read < typing[0], "read before typing: {calls:?}");
    assert!(calls[read..=key].iter().all(|(held, _)| *held), "{calls:?}");
    let enter = typing
        .iter()
        .rfind(|&&i| calls[i].1.ends_with("Enter"))
        .copied()
        .unwrap();
    assert!(
        calls[enter..key]
            .iter()
            .any(|(_, c)| c.contains("list-panes") && c.contains("pane_tty")),
        "owns checked again before the key: {calls:?}"
    );
    assert_eq!(a.received(), format!("/model {SONNET}\r1"));
}

/// `fleet` with a tmux that logs every call and — the first time the agent is checked to own
/// its pane (`list-panes … pane_tty`) once its confirmation is drawn (`<log>.confirming`) —
/// first runs `hook` (sh): what happens in the terminal while `owns` looks. The calls logged.
fn while_owns_looks(a: &Agent, hook: &str, args: &[&str]) -> (std::process::Output, Vec<String>) {
    let f = &a.f;
    let wrapper = f.env.path("hook-tmux.sh");
    let calls = f.env.path("hook.log");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{calls}'\n\
             case \"$*\" in *list-panes*pane_tty*)\n\
             if [ -e '{log}.confirming' ] && [ ! -e '{fired}' ]; then\n\
             : > '{fired}'\n\
             {hook}\n\
             fi;;\n\
             esac\n\
             exec '{real}' \"$@\"\n",
            calls = calls.display(),
            log = a.log.display(),
            fired = f.env.path("hook.fired").display(),
            real = f.env.tmux_wrapper().display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = f
        .env
        .cmd()
        .env("SHELL", "/bin/sh")
        .env("FLEET_TMUX", &wrapper)
        .timeout(Duration::from_secs(20))
        .args(args)
        .output()
        .unwrap();
    assert!(
        f.env.path("hook.fired").exists(),
        "the hook never ran: {}",
        stderr(&out)
    );
    let log = std::fs::read_to_string(&calls).unwrap_or_default();
    (out, log.lines().map(str::to_string).collect())
}

/// sh: wait (up to 5 s) for `cond`.
fn sh_wait(cond: &str) -> String {
    format!("i=0; until {cond}; do [ $i -ge 250 ] && break; sleep 0.02; i=$((i+1)); done")
}

/// Nothing typed after the command's Enter: its last `send-keys` is that Enter.
fn nothing_after_enter(calls: &[String]) {
    let typing: Vec<&String> = calls.iter().filter(|c| c.contains("send-keys")).collect();
    assert!(
        typing.last().is_some_and(|c| c.ends_with("Enter")),
        "nothing after the Enter: {calls:?}"
    );
}

// The confirmation answered in the terminal while `owns` looks (declined: `Kept model as …`,
// the box back): the second read finds it gone, and nothing is pressed. The switch was
// declined, the model unchanged: `outcome_uncertain`.
#[test]
fn a_confirmation_gone_by_the_second_read_gets_nothing_pressed() {
    let a = agent("warm", "live/claude-after-turn");
    let hook = format!(
        ": > '{log}.decline'\n{}\n{}",
        sh_wait(&format!("[ -e '{}.declined' ]", a.log.display())),
        sh_wait(&format!(
            "'{}' capture-pane -p -t fleet-test-model | grep -q 'Kept model as'",
            a.f.env.tmux_wrapper().display()
        )),
        log = a.log.display(),
    );
    let t = a.target();
    let (out, calls) = while_owns_looks(&a, &hook, &["model", "--json", &t, SONNET]);
    let v: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
    failed(out.status.code().unwrap_or(-1), &v, "outcome_uncertain");
    let error = v["error"].as_str().unwrap();
    assert!(error.contains("declined in the terminal"), "{v}");
    assert!(error.contains("kept Opus 5.5 (1M context)"), "{v}");
    assert!(error.contains("the model is unchanged"), "{v}");
    assert!(!error.contains("pressed"), "{v}");
    // The screen was read again after `owns` looked.
    let owns = calls
        .iter()
        .rposition(|c| c.contains("list-panes") && c.contains("pane_tty"))
        .unwrap();
    assert!(
        calls[owns..].iter().any(|c| c.contains("capture-pane")),
        "{calls:?}"
    );
    nothing_after_enter(&calls);
    assert_eq!(
        a.received(),
        format!("/model {SONNET}\r"),
        "nothing pressed"
    );
}

// The agent stops owning its pane while its confirmation is on screen (suspended, its shell
// in front): `host_shared`, `suspended`, and nothing pressed.
#[test]
fn an_agent_that_stops_owning_its_pane_at_the_confirmation_gets_nothing_pressed() {
    let (a, flag) = under_job_control("warm", "shell");
    let hook = format!(
        "kill -STOP {pid}\n: > '{flag}'\n{}",
        sh_wait(&format!("[ -e '{}.job' ]", flag.display())),
        pid = a.pid,
        flag = flag.display(),
    );
    let t = a.target();
    let (out, calls) = while_owns_looks(&a, &hook, &["model", "--json", &t, SONNET]);
    let _ = std::process::Command::new("kill")
        .args(["-CONT", &a.pid.to_string()])
        .status();
    let v: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
    failed(out.status.code().unwrap_or(-1), &v, "host_shared");
    assert_eq!(v["reason"], "suspended", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("its confirmation is on screen, nothing pressed"),
        "{v}"
    );
    nothing_after_enter(&calls);
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// `Set model to <label>` naming another model than the one asked for (fleet's model list
// pairs `claude-haiku-5-5` with Haiku 5.5): `outcome_uncertain`. An alias is resolved through
// that list: `sonnet` is Sonnet 5.5, switched; `haiku` is Haiku 5.5, not switched to.
#[test]
fn a_switch_to_another_model_is_outcome_uncertain() {
    let a = agent("cold", "live/claude-empty");
    let (code, v, _) = a.model("claude-haiku-5-5", &[]);
    failed(code, &v, "outcome_uncertain");
    let error = v["error"].as_str().unwrap();
    assert!(
        error.contains("set the model to Sonnet 5.5, not claude-haiku-5-5 (Haiku 5.5)"),
        "{v}"
    );
    assert_eq!(a.received(), "/model claude-haiku-5-5\r");

    let a = agent("cold", "live/claude-empty");
    let (code, v, err) = a.model("sonnet", &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["model"], "sonnet", "{v}");

    // Another model by alias: named as the list resolves it, the list maybe stale.
    let a = agent("cold", "live/claude-empty");
    let (code, v, _) = a.model("haiku", &[]);
    failed(code, &v, "outcome_uncertain");
    let error = v["error"].as_str().unwrap();
    assert!(
        error.contains("set the model to Sonnet 5.5, not haiku (Haiku 5.5)")
            && error.contains("out of date")
            && !error.contains("()"),
        "{v}"
    );
}

#[test]
fn only_an_exact_target_and_a_clean_model_id_are_taken() {
    let a = agent("cold", "live/claude-empty");
    let pid = a.pid.to_string();
    let t = a.target();
    for args in [
        // Never a fragment, a bare pid or a title.
        vec!["model", "--json", &pid, SONNET],
        vec!["model", "--json", &SID[..8], SONNET],
        vec!["model", "--json", "model", SONNET],
        // A model id that is not one.
        vec!["model", "--json", &t, "sonnet; rm -rf"],
        vec!["model", "--json", &t, " "],
        // --expect-pid and --expect-start go together, and the start is a time.
        vec!["model", "--json", &t, SONNET, "--expect-pid", "1"],
        vec![
            "model",
            "--json",
            &t,
            SONNET,
            "--expect-pid",
            "1",
            "--expect-start",
            "soon",
        ],
        // Clap's own: a missing model id, an unknown flag.
        vec!["model", "--json", &t],
        vec!["model", &t, SONNET, "--force", "--json"],
    ] {
        let (code, v, _) = a.fleet(&args);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("usage")),
            "{args:?}: {v}"
        );
    }
    let (code, v, _) = a.fleet(&["model", "--json", "pid:abc", SONNET]);
    failed(code, &v, "target_invalid");
    let gone = format!("pid:{}", a.pid + 100_000);
    let (code, v, _) = a.fleet(&["model", "--json", &gone, SONNET]);
    failed(code, &v, "target_not_found");
    // Another process than the one the caller saw.
    let start = a.listed()["proc_start"].as_str().unwrap().to_string();
    let other = (a.pid + 1).to_string();
    let (code, v, _) = a.model(SONNET, &["--expect-pid", &other, "--expect-start", &start]);
    failed(code, &v, "target_changed");
    let (code, v, _) = a.model(
        SONNET,
        &[
            "--expect-pid",
            &pid,
            "--expect-start",
            "2001-01-01T00:00:00Z",
        ],
    );
    failed(code, &v, "target_changed");
    assert_eq!(a.received(), "", "nothing typed");

    // A canned fleet types nowhere.
    let fixture = common::fixture(
        a.f.env.dir.path(),
        &format!(
            r#"[{{"pid": {pid}, "session_id": "{SID}", "status": "idle", "backend": "tmux", "handle": "%0"}}]"#
        ),
    );
    let out =
        a.f.env
            .cmd()
            .env("FLEET_FIXTURE", &fixture)
            .args(["model", "--json", &t, SONNET])
            .output()
            .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    failed(out.status.code().unwrap(), &v, "backend_unsupported");
}

#[test]
fn codex_and_pi_are_harness_unsupported() {
    for (harness, home) in [("codex", "home/.codex"), ("pi", "home/.pi/agent")] {
        let f = FakeFleet::new().expect("these tests need tmux");
        std::fs::create_dir_all(f.env.path(home)).unwrap();
        let log = f.env.path("received.log");
        let logger = f.env.path("logger.pl");
        std::fs::write(&logger, LOGGER).unwrap();
        let pid = f.agent(
            "model",
            SID,
            80,
            24,
            &format!(
                "exec /bin/bash -c \"exec -a {harness} perl '{}' '{}'\"",
                logger.display(),
                log.display()
            ),
        );
        std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
        let a = Agent {
            transcript: transcript_of(&f),
            f,
            pid,
            log,
        };
        a.f.wait_for("the agent to start", || a.log.exists());
        let (code, v, _) = a.model("gpt-6", &[]);
        failed(code, &v, "harness_unsupported");
        assert_eq!(a.received(), "", "{harness}: nothing typed");
    }
}

#[test]
fn a_held_lock_is_busy() {
    let a = agent("cold", "live/claude-empty");
    std::fs::create_dir_all(a.f.env.path("locks")).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(a.f.env.path(&format!("locks/{SID}.lock")))
        .unwrap();
    held.lock().unwrap();
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "busy");
    assert_eq!(v["reason"], "locked");
    assert_eq!(a.received(), "");
}

// Not in front of its pane: its editor open (`busy`), suspended behind its shell's prompt
// (`host_shared`, `suspended` — what is typed would reach the shell; close would kill it
// instead), or behind another job (`host_shared`, `job_in_front`).
#[test]
fn an_agent_that_does_not_own_its_pane_gets_nothing_typed() {
    let a = agent("edits", "live/claude-empty");
    let editor = PathBuf::from(format!("{}.editor", a.log.display()));
    a.f.wait_for("the editor", || editor.exists());
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "busy");
    assert_eq!(v["reason"], "editor_open", "{v}");
    assert_eq!(a.received(), "");

    for (front, reason) in [("shell", "suspended"), ("job", "job_in_front")] {
        let (a, flag) = under_job_control("cold", front);
        assert!(
            std::process::Command::new("kill")
                .args(["-STOP", &a.pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(&flag, "").unwrap();
        let job = PathBuf::from(format!("{}.job", flag.display()));
        a.f.wait_for("the shell to bring its job to the front", || job.exists());
        let (code, v, _) = a.model(SONNET, &[]);
        failed(code, &v, "host_shared");
        assert_eq!(v["reason"], reason, "{v}");
        let _ = std::process::Command::new("kill")
            .args(["-CONT", &a.pid.to_string()])
            .status();
        assert_eq!(a.received(), "", "{front}: nothing typed");
        let pid = std::fs::read_to_string(&job).unwrap();
        if !pid.trim().is_empty() {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", pid.trim()])
                .status();
        }
    }
}

// Not idle by its registry (`status` as it reads), or by a working line on screen though the
// registry says idle (it lags a prompt typed in the terminal): `not_idle`, nothing typed.
#[test]
fn a_session_not_idle_gets_nothing_typed() {
    let a = agent("cold", "live/claude-empty");
    for status in ["busy", "waiting"] {
        a.status(status);
        let (code, v, _) = a.model(SONNET, &[]);
        failed(code, &v, "not_idle");
        assert_eq!(v["status"], status, "{v}");
    }
    assert_eq!(a.received(), "");

    let a = agent("cold", "live/claude-busy");
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "not_idle");
    assert_eq!(v["status"], "busy", "{v}");
    assert!(v["error"].as_str().unwrap().contains("Actualizing"), "{v}");
    assert_eq!(a.received(), "");
}

// Claude Code 2.1.308 writes `shell` for a session idle at its prompt with a background shell
// still running: it switches as on `idle`, and `list --json` keeps the Status as written.
#[test]
fn a_session_idle_with_a_background_shell_switches() {
    let a = agent("cold", "live/claude-empty");
    a.status("shell");
    assert_eq!(a.listed()["status"], "shell");
    let (code, v, err) = a.model(SONNET, &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["model"], SONNET, "{v}");
    assert_eq!(v["switched_at"], a.switched_at().as_str(), "{v}");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));

    // `-n` too: every check passes, nothing typed.
    let a = agent("cold", "live/claude-empty");
    a.status("shell");
    let (code, v, err) = a.fleet(&["-n", "model", "--json", &a.target(), SONNET]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(a.received(), "");
}

// The box read as `send --require-empty` reads it: text in it, a prompt in its place, or no
// box found hold the switch — `send_held`, exit 3, `send`'s reason; the box's text only in
// `input`.
#[test]
fn a_box_that_is_not_empty_holds_it() {
    for (screen, reason, input) in [
        (
            "live/claude-typed",
            "text",
            Some("draft text typed but not sent"),
        ),
        ("claude-permission", "prompt", None),
        ("claude-transcript", "no_input_box", None),
    ] {
        let a = agent("cold", screen);
        let (code, v, err) = a.model(SONNET, &[]);
        assert_eq!(code, 3, "{screen}: {v} {err}");
        assert_eq!(
            (v["ok"].as_bool(), v["kind"].as_str(), v["reason"].as_str()),
            (Some(false), Some("send_held"), Some(reason)),
            "{screen}: {v}"
        );
        match input {
            Some(t) => {
                assert_eq!(v["input"], t, "{v}");
                assert!(!v["error"].as_str().unwrap().contains(t), "{v}");
                assert!(!err.contains(t), "{err}");
            }
            None => assert_eq!(v["input"], Value::Null, "{v}"),
        }
        assert_eq!(a.received(), "", "{screen}: nothing typed");
    }
}

// Blanks in Claude Code's box show as nothing; `send`'s reader tells them by the cursor, and
// `/model` typed behind them would be a prompt, not the command.
#[test]
fn blanks_left_in_the_box_hold_it() {
    let a = agent_env("cold", "live/claude-empty", "FAKE_BLANKS=3");
    let (code, v, err) = a.model(SONNET, &[]);
    assert_eq!(code, 3, "{v} {err}");
    assert_eq!(
        (
            v["kind"].as_str(),
            v["reason"].as_str(),
            v["input"].as_str()
        ),
        (Some("send_held"), Some("text"), Some("   ")),
        "{v}"
    );
    assert_eq!(a.received(), "", "nothing typed");
}

// `--require-empty` is `model`'s own guard already: given or not, the same holds and the same
// switch.
#[test]
fn require_empty_changes_nothing() {
    for (screen, reason) in [
        ("live/claude-typed", "text"),
        ("claude-permission", "prompt"),
        ("claude-transcript", "no_input_box"),
    ] {
        let a = agent("cold", screen);
        let (code, v, err) = a.model(SONNET, &["--require-empty"]);
        assert_eq!(code, 3, "{screen}: {v} {err}");
        assert_eq!(
            (v["kind"].as_str(), v["reason"].as_str()),
            (Some("send_held"), Some(reason)),
            "{screen}: {v}"
        );
        assert_eq!(a.received(), "", "{screen}: nothing typed");
    }
    let a = agent("cold", "live/claude-empty");
    let (code, v, err) = a.model(SONNET, &["--require-empty"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

#[test]
fn dry_run_checks_and_types_nothing() {
    let a = agent("cold", "live/claude-empty");
    let (code, v, err) = a.fleet(&["-n", "model", "--json", &a.target(), SONNET]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(
        (
            v["dry_run"].as_bool(),
            v["confirmed"].as_bool(),
            v["switched_at"].is_null()
        ),
        (Some(true), Some(false), true),
        "{v}"
    );
    a.status("busy");
    let (code, v, _) = a.fleet(&["-n", "model", "--json", &a.target(), SONNET]);
    failed(code, &v, "not_idle");
    assert_eq!(a.received(), "");
}

// A confirmation for another model is not this switch's: nothing pressed.
#[test]
fn an_unrecognized_screen_stops_it_with_nothing_pressed() {
    let a = agent("other", "live/claude-empty");
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "confirm_unrecognized");
    let said = format!("Claude Code's switch confirmation for {SONNET} or its input box");
    assert!(v["error"].as_str().unwrap().contains(&said), "{v}");
    assert_eq!(
        a.received(),
        format!("/model {SONNET}\r"),
        "nothing pressed"
    );
}

// Claude Code answers an unknown id under the command; the screen says so.
#[test]
fn an_unknown_model_is_model_not_found() {
    let a = agent("notfound", "live/claude-empty");
    let (code, v, _) = a.model("claude-nonexistent-9-9", &[]);
    failed(code, &v, "model_not_found");
    assert_eq!(a.received(), "/model claude-nonexistent-9-9\r");
}

// A PreModelSwitch hook says no, in the transcript and on screen.
#[test]
fn a_hooks_refusal_is_switch_refused() {
    let a = agent("refused", "live/claude-empty");
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "switch_refused");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// Typed, but no "Set model to" within the wait (5 s): it cannot be told whether it switched.
#[test]
fn no_switch_in_the_transcript_is_outcome_uncertain() {
    let a = agent("silent", "live/claude-empty");
    let began = std::time::Instant::now();
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "outcome_uncertain");
    assert!(began.elapsed() >= Duration::from_secs(5), "it waited");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// A prompt that is exactly the switch's output, written as the switch's own entry is, but
// with no `/model` command entry before it: no switch, `outcome_uncertain` once the wait ends.
#[test]
fn a_prompt_typed_as_the_switch_is_no_switch() {
    let a = agent("forged", "live/claude-empty");
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "outcome_uncertain");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("wrote no \"Set model to\""),
        "{v}"
    );
    let written = std::fs::read_to_string(transcript_of(&a.f)).unwrap();
    assert!(
        written.contains("<local-command-stdout>Set model to"),
        "{written}"
    );
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// Success needs the label to be the model asked for: one fleet cannot match to it (Claude
// Code's "Default (recommended)") is `outcome_uncertain`, never `ok`.
#[test]
fn a_label_fleet_cannot_match_to_the_model_is_outcome_uncertain() {
    let a = agent_env(
        "cold",
        "live/claude-empty",
        "FAKE_LABEL='Default (recommended)'",
    );
    let (code, v, _) = a.model(SONNET, &[]);
    failed(code, &v, "outcome_uncertain");
    let error = v["error"].as_str().unwrap();
    assert!(
        error.contains("Default (recommended), which fleet cannot match to claude-sonnet-5-5"),
        "{v}"
    );
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// The session id changes between the lookup and the lock (a `/clear` finishing, the same
// process, pane and start): the lock held is the old id's, and another fleet command could
// take the new one's. `target_changed`, nothing typed.
#[test]
fn a_session_id_changed_since_the_lookup_gets_nothing_typed() {
    let a = agent("cold", "live/claude-empty");
    let new = "dddddddd-5555-6666-7777-888888888888";
    let edit = once_at_lookup(&a, &format!("s/{SID}/{new}/"));
    let t = a.target();
    let (code, v, err, _) = under_tmux(&a, &edit, &["model", "--json", &t, SONNET]);
    failed(code, &v, "target_changed");
    let error = v["error"].as_str().unwrap();
    assert!(error.contains(SID) && error.contains(new), "{v} {err}");
    assert_eq!(a.received(), "", "nothing typed");
}

// The Status is read again under the lock, not taken from the lookup: busy since then is
// `not_idle`, nothing typed.
#[test]
fn the_status_is_read_again_under_the_lock() {
    let a = agent("cold", "live/claude-empty");
    let edit = once_at_lookup(&a, r#"s/"status":"idle"/"status":"busy"/"#);
    let t = a.target();
    let (code, v, err, _) = under_tmux(&a, &edit, &["model", "--json", &t, SONNET]);
    failed(code, &v, "not_idle");
    assert_eq!(v["status"], "busy", "{v} {err}");
    assert_eq!(a.received(), "", "nothing typed");
}

// Without the terminal's cursor (hidden, or its query failing) a box showing nothing cannot be
// told from blanks typed in it: `send_held`, `unreadable`, no `input`, blanks or none.
#[test]
fn a_box_without_its_cursor_holds_it_as_unreadable() {
    let fail_cursor = "case \"$*\" in *cursor_flag*) exit 1;; esac";
    for blanks in [0, 3] {
        for hidden in [true, false] {
            let env = format!("FAKE_BLANKS={blanks} FAKE_HIDE={}", u8::from(hidden));
            let a = agent_env("cold", "live/claude-empty", &env);
            let t = a.target();
            let (code, v, err, _) = under_tmux(
                &a,
                if hidden { ":" } else { fail_cursor },
                &["model", "--json", &t, SONNET],
            );
            assert_eq!(code, 3, "{env}: {v} {err}");
            assert_eq!(
                (
                    v["kind"].as_str(),
                    v["reason"].as_str(),
                    v["input"].is_null()
                ),
                (Some("send_held"), Some("unreadable"), true),
                "{env}: {v}"
            );
            assert!(
                v["error"].as_str().unwrap().contains("cannot be read"),
                "{v}"
            );
            assert_eq!(a.received(), "", "{env}: nothing typed");
        }
    }
}

// A tmux that stops answering once the command is typed: every call while waiting for the
// outcome gets only what is left of the wait, so the lock is let go within it —
// `outcome_uncertain`, not a hang.
#[test]
fn a_tmux_that_stops_answering_after_the_command_cannot_outlast_the_wait() {
    let a = agent("silent", "live/claude-empty");
    let hang = format!(
        "if [ -e '{log}' ] && perl -0777 -ne 'exit(/\\r/ ? 0 : 1)' '{log}'; then exec sleep 30; fi",
        log = a.log.display()
    );
    let t = a.target();
    let (code, v, err, took) = under_tmux(&a, &hang, &["model", "--json", &t, SONNET]);
    failed(code, &v, "outcome_uncertain");
    assert!(
        took < Duration::from_secs(5) + Duration::from_millis(2500),
        "it took {took:?}: {v} {err}"
    );
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}

// An `io` failure says whether anything reached the pane: the typing lock that cannot be taken
// (its directory cannot be made) is `typed: false`, nothing typed, so a caller may try again;
// `/model` failing to go out is `typed: true`, part of it may have.
#[test]
fn an_io_failure_says_whether_anything_was_typed() {
    let a = agent("cold", "live/claude-empty");
    let blocker = a.f.env.path("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let t = a.target();
    let out =
        a.f.env
            .cmd()
            .env("SHELL", "/bin/sh")
            .env("FLEET_LOCK_DIR", blocker.join("locks"))
            .timeout(Duration::from_secs(20))
            .args(["model", "--json", &t, SONNET])
            .output()
            .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    failed(out.status.code().unwrap_or(-1), &v, "io");
    assert_eq!(v["typed"], false, "{v}");
    assert_eq!(a.received(), "", "nothing typed");

    let a = agent("cold", "live/claude-empty");
    let t = a.target();
    let (code, v, err, _) = under_tmux(
        &a,
        "case \"$*\" in *send-keys*) echo 'tmux: lost server' >&2; exit 1;; esac",
        &["model", "--json", &t, SONNET],
    );
    failed(code, &v, "io");
    assert_eq!(v["typed"], true, "{v} {err}");
    assert!(
        v["error"].as_str().unwrap().contains("may be partly typed"),
        "{v}"
    );
}

// `model id:<session id>` with a `/clear` landing between the lookup and the lock: the id it
// names now runs no session, and the process runs another — `target_changed`, as `send` says
// it, not `target_not_found`. Nothing typed.
#[test]
fn an_id_target_whose_id_changed_since_the_lookup_is_target_changed() {
    let a = agent("cold", "live/claude-empty");
    let new = "dddddddd-5555-6666-7777-888888888888";
    let edit = once_at_lookup(&a, &format!("s/{SID}/{new}/"));
    let t = format!("id:{SID}");
    let (code, v, err, _) = under_tmux(&a, &edit, &["model", "--json", &t, SONNET]);
    failed(code, &v, "target_changed");
    let error = v["error"].as_str().unwrap();
    assert!(error.contains(SID) && error.contains(new), "{v} {err}");
    assert_eq!(a.received(), "", "nothing typed");
}

// A warm cache asked for by alias: Claude Code's confirmation names the model the alias
// resolves to ("Yes, switch to Sonnet 5.5" for `sonnet`). That is this switch's: confirmed
// with its first choice, switched.
#[test]
fn an_alias_is_confirmed_at_a_warm_cache() {
    let a = agent("warm", "live/claude-after-turn");
    let (code, v, err) = a.model("sonnet", &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["model"], "sonnet", "{v}");
    assert_eq!(v["confirmed"], true, "{v}");
    assert_eq!(v["switched_at"], a.switched_at().as_str(), "{v}");
    assert_eq!(a.received(), "/model sonnet\r1");
}

// PreModelSwitch hooks that take longer than a redraw: Claude Code shows "Running
// PreModelSwitch hooks… (esc to cancel)" where the box was until they are done. Pending, not
// an unrecognized screen: the switch that follows is read as any other.
#[test]
fn hooks_still_running_are_waited_for() {
    let a = agent_env("hooks", "live/claude-empty", "FAKE_HOOK_MS=1500");
    let (code, v, err) = a.model(SONNET, &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["confirmed"], false, "{v}");
    assert_eq!(v["switched_at"], a.switched_at().as_str(), "{v}");
    assert_eq!(a.received(), format!("/model {SONNET}\r"));
}
