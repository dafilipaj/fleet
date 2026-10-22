//! `fleet effort` against a fake Claude Code on the test's private tmux server. The checks
//! before typing, the lock and the wait are `fleet model`'s (tests/model.rs covers them one by
//! one); here, what `/effort` adds, and that the shared checks guard it too. The fake draws a
//! captured screen (testdata/screens, `live/claude-effort-*` the live captures of Claude Code
//! 2.1.308 in a 200x50 pane), logs every byte it reads, and answers `/effort <level>` + Enter
//! as Claude Code 2.1.308 does, by `mode`, writing into its transcript the command's `user`
//! entry, then its output's under it (`Set effort level to <level> (saved as your default for
//! new sessions): …`, `(this session only)` for `max`, `Effort level set to auto` for `auto`;
//! `$FAKE_SAID` in its place when set):
//!
//! - `cold`: answers at once, and draws its box again — the prompt cache cold, nothing asked;
//! - `warm`: draws the confirmation for the level asked (`live/claude-effort-confirm`) first,
//!   and answers once its first choice's key (`1`) arrives;
//! - `declined`: answers `Kept effort level as medium`, as when the confirmation is declined
//!   in the terminal;
//! - `other`: draws `/model`'s confirmation and waits;
//! - `forged`: writes only a prompt that is exactly the switch's output, no command entry
//!   before it, as a person typing it would;
//! - `banner`: draws only Claude Code's tmux hint banner where the box was for
//!   `$FAKE_BANNER_MS` (1500 ms), then answers as `cold`;
//! - `late`: draws a screen that is neither the box nor a confirmation (`claude-transcript`)
//!   and makes `<log>.other`; once `<log>.write` exists it answers as `cold`, the screen left as it
//!   is, and makes `<log>.written`;
//! - `silent` (and any other): does nothing.
//!
//! These tests need tmux and perl, and fail without them rather than pass having run nothing.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fake::FakeFleet;
use common::stdout;
use common::switch::{self, failed, keys_of};
use serde_json::Value;

const SID: &str = "eeeeeeee-1111-2222-3333-444444444444";

/// Logs every byte it reads to the file it is given (a Codex session's stand-in).
const LOGGER: &str = r#"open(my $f, ">>", $ARGV[0]) or die; select((select($f), $| = 1)[0]);
system("stty raw -echo");
while (sysread(STDIN, my $b, 64)) { print $f $b }
"#;

const AGENT: &str = r#"
use strict; use Time::HiRes qw(time sleep); use POSIX qw(strftime);
my ($log, $mode, $screens, $start, $transcript) = @ARGV;
system("stty raw -echo");
sub draw {
  my ($name, @subs) = @_;
  open(my $s, "<", "$screens/$name.txt") or die; local $/; my $t = <$s>; close $s;
  while (@subs) { my ($from, $to) = splice(@subs, 0, 2); $t =~ s/\Q$from\E/$to/g; }
  $t =~ s/\n/\r\n/g; syswrite(STDOUT, "\e[H\e[2J$t");
}
sub esc { my $s = shift; $s =~ s/\\/\\\\/g; $s =~ s/"/\\"/g; $s =~ s/\n/\\n/g; $s =~ s/\e/\\u001b/g; $s }
# Each entry the parent of the next, the command and its output under one prompt id, as
# Claude Code chains them.
my ($entries, $last, $prompt) = (0, "", "");
sub user {
  my $t = time; my $at = strftime("%Y-%m-%dT%H:%M:%S", gmtime($t)) . sprintf(".%03dZ", int(($t - int($t)) * 1000));
  my $uuid = sprintf("aaaaaaaa-0000-4000-8000-%012d", ++$entries);
  my $parent = $last eq "" ? "null" : "\"$last\""; $last = $uuid;
  open(my $f, ">>", $transcript) or die;
  print $f '{"parentUuid":' . $parent . ',"isSidechain":false,"promptId":"' . $prompt . '","type":"user","message":{"role":"user","content":"' . esc($_[0]) . '"},"uuid":"' . $uuid . '","timestamp":"' . $at . '","version":"2.1.308"}' . "\n";
  close $f;
}
sub said {
  my $l = shift;
  return $ENV{FAKE_SAID} if defined $ENV{FAKE_SAID};
  return "Effort level set to auto" if $l eq "auto";
  return "Set effort level to max (this session only): The deepest reasoning." if $l eq "max";
  return "Set effort level to $l (saved as your default for new sessions): An example description";
}
sub answer {
  my ($l, $out) = @_;
  $prompt = sprintf("bbbbbbbb-0000-4000-8000-%012d", $entries + 1);
  user("<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>$l</command-args>");
  user("<local-command-stdout>$out</local-command-stdout>");
  draw("live/claude-effort-confirmed") unless $mode eq "late";
}
sub touch { open(my $t, ">", $_[0]) or die; close $t; }
draw($start);
open(my $f, ">>", $log) or die; select((select($f), $| = 1)[0]);
my ($seen, $state, $level, $until) = ("", "", "", 0);
while (1) {
  if ($state eq "banner" && time >= $until) { $state = "done"; answer($level, said($level)); }
  if ($state eq "late" && -e "$log.write") { $state = "done"; answer($level, said($level)); touch("$log.written"); }
  my $rin = ""; vec($rin, fileno(STDIN), 1) = 1;
  next if select(my $rout = $rin, undef, undef, 0.05) < 1;
  last unless sysread(STDIN, my $b, 64);
  print $f $b;
  $seen .= $b;
  if ($state eq "" && $seen =~ m{/effort (\S+)\r}) {
    $level = $1;
    $state = "typed";
    if ($mode eq "cold") { answer($level, said($level)); }
    elsif ($mode eq "warm") { draw("live/claude-effort-confirm", "xhigh", $level); $state = "confirm"; $seen = ""; }
    elsif ($mode eq "declined") { answer($level, "Kept effort level as medium"); }
    elsif ($mode eq "other") { draw("live/claude-model-confirm"); }
    elsif ($mode eq "forged") {
      user("<local-command-stdout>" . said($level) . "</local-command-stdout>");
      draw("live/claude-effort-confirmed");
    }
    elsif ($mode eq "banner") {
      syswrite(STDOUT, "\e[H\e[2J" . (" " x 98) . "tmux focus-events off \x{c2}\x{b7} add 'set -g focus-events on' to ~/.tmux.conf and reattach for focus tracking\r\n");
      $state = "banner"; $until = time + ($ENV{FAKE_BANNER_MS} // 1500) / 1000;
    }
    elsif ($mode eq "late") { draw("claude-transcript"); touch("$log.other"); $state = "late"; }
  } elsif ($state eq "confirm" && $seen =~ /1/) {
    $state = "done";
    answer($level, said($level));
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

/// The reply its transcript starts with.
const REPLY_AT: &str = "2026-01-02T03:04:30.000Z";

/// A fake Claude Code in `mode`, the pane's own process, showing `start` (by default an empty
/// box: `live/claude-effort-declined`), run with `env` (`NAME=value …`) set.
fn agent_env(mode: &str, start: &str, env: &str) -> Agent {
    assert!(
        fleet::core::tools::find_binary("perl").is_some(),
        "these tests need perl"
    );
    let f = FakeFleet::new().expect("these tests need tmux");
    let transcript = switch::transcript_of(&f, SID);
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
    let log = f.env.path("received.log");
    let line = format!(
        "exec env {env} perl '{}' '{}' '{mode}' '{}' '{start}' '{}'",
        script.display(),
        log.display(),
        screens.display(),
        transcript.display(),
    );
    let pid = f.agent("effort", SID, 200, 50, &line);
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent {
        f,
        pid,
        log,
        transcript,
    }
}

fn agent(mode: &str) -> Agent {
    agent_env(mode, "live/claude-effort-declined", "")
}

impl Agent {
    fn target(&self) -> String {
        format!("pid:{}", self.pid)
    }

    /// `fleet …` → (exit code, the one JSON object on stdout, stderr).
    fn fleet(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self.f.fleet(args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| Value::String(stdout(&out)));
        (out.status.code().unwrap_or(-1), v, common::stderr(&out))
    }

    /// `fleet effort --json <target> <level> [extra…]`.
    fn effort(&self, level: &str, extra: &[&str]) -> (i32, Value, String) {
        let t = self.target();
        let mut args = vec!["effort", "--json", t.as_str(), level];
        args.extend(extra);
        self.fleet(&args)
    }

    /// What the agent has read, once a moment has passed for anything on its way.
    fn received(&self) -> String {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The `timestamp` of the transcript's last `/effort` output entry.
    fn answered_at(&self) -> String {
        let text = std::fs::read_to_string(&self.transcript).unwrap();
        let line = text
            .lines()
            .rfind(|l| l.contains("local-command-stdout"))
            .unwrap_or_else(|| panic!("no answer in {text}"));
        let v: Value = serde_json::from_str(line).unwrap();
        v["timestamp"].as_str().unwrap().to_string()
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
}

#[test]
fn a_cold_cache_switches_at_once_and_says_whether_it_was_saved() {
    let a = agent("cold");
    let (_, rows, _) = a.fleet(&["list", "--json"]);
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["pid"] == a.pid)
        .unwrap()
        .clone();
    let (pid, start) = (
        row["pid"].to_string(),
        row["proc_start"].as_str().unwrap().to_string(),
    );
    let (code, v, err) = a.effort("medium", &["--expect-pid", &pid, "--expect-start", &start]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(
        keys_of(&v),
        [
            "confirmed",
            "dry_run",
            "effort",
            "harness",
            "host",
            "ok",
            "pid",
            "saved",
            "session_id",
            "switched_at"
        ],
        "{v}"
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["session_id"], SID);
    assert_eq!(v["pid"], a.pid);
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["effort"], "medium");
    assert_eq!(v["confirmed"], false, "nothing was asked");
    assert_eq!(v["saved"], true, "{v}");
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["switched_at"], a.answered_at().as_str(), "{v}");
    assert_eq!(a.received(), "/effort medium\r");

    // Text output says what it did.
    let a = agent("cold");
    let (code, out, err) = a.fleet(&["effort", &a.target(), "high"]);
    assert_eq!(code, 0, "{err}");
    let out = out.as_str().unwrap();
    assert!(
        out.contains("to high effort, saved as Claude Code's default"),
        "{out}"
    );
}

// `max` is for this session only (`saved: false`); `auto` clears the model's saved level, so
// new sessions start at its default — the default changed (`saved: true`).
#[test]
fn max_is_not_saved_and_auto_clears_the_saved_level() {
    for (level, saved, says) in [
        ("max", false, "for this session only"),
        ("auto", true, "saved level cleared"),
    ] {
        let a = agent("cold");
        let (code, v, err) = a.effort(level, &[]);
        assert_eq!(code, 0, "{level}: {v} {err}");
        assert_eq!(
            (v["effort"].as_str(), v["saved"].as_bool()),
            (Some(level), Some(saved)),
            "{v}"
        );
        assert_eq!(v["switched_at"], a.answered_at().as_str(), "{v}");
        let a = agent("cold");
        let (code, out, err) = a.fleet(&["effort", &a.target(), level]);
        assert_eq!(code, 0, "{err}");
        assert!(out.as_str().unwrap().contains(says), "{out}");
    }
    // The text says what the answer says: `auto` for this session only is not saved, nor
    // its saved level cleared.
    let a = agent_env(
        "cold",
        "live/claude-effort-declined",
        "FAKE_SAID='Set effort level to auto (this session only): An example description'",
    );
    let (code, out, err) = a.fleet(&["effort", &a.target(), "auto"]);
    assert_eq!(code, 0, "{err}");
    let out = out.as_str().unwrap();
    assert!(out.contains("for this session only"), "{out}");
    assert!(!out.contains("saved level cleared"), "{out}");
}

#[test]
fn a_warm_cache_is_confirmed_with_its_first_choice() {
    let a = agent("warm");
    let (code, v, err) = a.effort("xhigh", &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["confirmed"], true, "{v}");
    assert_eq!(v["saved"], true, "{v}");
    assert_eq!(v["switched_at"], a.answered_at().as_str(), "{v}");
    // The command, then the key the confirmation's first choice names: nothing else.
    assert_eq!(a.received(), "/effort xhigh\r1");
}

// Declined in the terminal (`Kept effort level as …`): the level is unchanged.
#[test]
fn a_declined_switch_is_outcome_uncertain() {
    let a = agent("declined");
    let (code, v, _) = a.effort("xhigh", &[]);
    failed(code, &v, "outcome_uncertain");
    let error = v["error"].as_str().unwrap();
    assert!(error.contains("declined in the terminal"), "{v}");
    assert!(error.contains("kept medium"), "{v}");
    assert!(error.contains("the effort level is unchanged"), "{v}");
    assert_eq!(a.received(), "/effort xhigh\r");
}

// Another level than the one asked for, or words fleet does not read as a switch (quoted):
// `outcome_uncertain`, never `ok`.
#[test]
fn an_answer_that_is_not_that_switch_is_outcome_uncertain() {
    for (said, says) in [
        (
            "Set effort level to high (saved as your default for new sessions): Thorough",
            "set the effort level to high, not xhigh",
        ),
        (
            "Invalid effort level: xhigh. Valid options are: low, medium, high",
            "answered \"Invalid effort level: xhigh. Valid options are: low, medium, high\"",
        ),
    ] {
        let a = agent_env(
            "cold",
            "live/claude-effort-declined",
            &format!("FAKE_SAID='{said}'"),
        );
        let (code, v, _) = a.effort("xhigh", &[]);
        failed(code, &v, "outcome_uncertain");
        assert!(v["error"].as_str().unwrap().contains(says), "{v}");
        assert_eq!(a.received(), "/effort xhigh\r");
    }
}

// `/model`'s confirmation is not this switch's: nothing pressed.
#[test]
fn an_unrecognized_screen_stops_it_with_nothing_pressed() {
    let a = agent("other");
    let (code, v, _) = a.effort("xhigh", &[]);
    failed(code, &v, "confirm_unrecognized");
    assert_eq!(a.received(), "/effort xhigh\r", "nothing pressed");
}

// A prompt that is exactly the switch's output, with no `/effort` command entry before it: no
// switch, `outcome_uncertain` once the wait ends.
#[test]
fn a_prompt_typed_as_the_switch_is_no_switch() {
    let a = agent("forged");
    let began = std::time::Instant::now();
    let (code, v, _) = a.effort("medium", &[]);
    failed(code, &v, "outcome_uncertain");
    assert!(began.elapsed() >= Duration::from_secs(5), "it waited");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("wrote no \"Set effort level to\""),
        "{v}"
    );
    assert_eq!(a.received(), "/effort medium\r");
}

// Claude Code's tmux hint banner alone where the box was, longer than a redraw: waited for, not
// taken for another screen.
#[test]
fn a_hint_banner_where_the_box_was_is_waited_for() {
    let a = agent_env(
        "banner",
        "live/claude-effort-declined",
        "FAKE_BANNER_MS=1500",
    );
    let (code, v, err) = a.effort("medium", &[]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(v["switched_at"], a.answered_at().as_str(), "{v}");
    assert_eq!(a.received(), "/effort medium\r");
}

// The answer written while the screen shows something else, read just before that screen has
// stood long enough to stop the switch: the transcript, read once more, wins over
// `confirm_unrecognized`. (A tmux that, at its second read of the screen after the command,
// lets the fake answer and only then reads, `SETTLE` later.)
#[test]
fn the_transcript_read_once_more_wins_over_an_unrecognized_screen() {
    let a = agent("late");
    let (log, n) = (a.log.display(), a.f.env.path("reads").display().to_string());
    let hook = format!(
        "case \"$*\" in *capture-pane*)\n\
         if [ -e '{log}.other' ] && [ ! -e '{log}.write' ]; then\n\
         c=$(cat '{n}' 2>/dev/null || echo 0); c=$((c+1)); echo $c > '{n}'\n\
         if [ $c -ge 2 ]; then : > '{log}.write'; i=0; until [ -e '{log}.written' ]; do [ $i -ge 250 ] && break; sleep 0.02; i=$((i+1)); done; sleep 0.7; fi\n\
         fi;;\n\
         esac"
    );
    let t = a.target();
    let (code, v, err, _) = switch::under_tmux(&a.f, &hook, &["effort", "--json", &t, "medium"]);
    assert_eq!(code, 0, "{v} {err}");
    assert!(PathBuf::from(format!("{log}.written")).exists());
    assert_eq!(v["switched_at"], a.answered_at().as_str(), "{v}");
    assert_eq!(a.received(), "/effort medium\r");
}

#[test]
fn only_an_exact_target_and_a_level_are_taken() {
    let a = agent("cold");
    let pid = a.pid.to_string();
    let t = a.target();
    for args in [
        vec!["effort", "--json", &pid, "high"],
        vec!["effort", "--json", &SID[..8], "high"],
        // Not a level: lowercase letters only, at most 16.
        vec!["effort", "--json", &t, "High"],
        vec!["effort", "--json", &t, "x-high"],
        vec!["effort", "--json", &t, "high; rm -rf"],
        vec!["effort", "--json", &t, " "],
        vec!["effort", "--json", &t, "abcdefghijklmnopq"],
        vec!["effort", "--json", &t, "high", "--expect-pid", "1"],
        // Clap's own: a missing level, an unknown flag.
        vec!["effort", "--json", &t],
        vec!["effort", &t, "high", "--force", "--json"],
    ] {
        let (code, v, _) = a.fleet(&args);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("usage")),
            "{args:?}: {v}"
        );
    }
    let (code, v, _) = a.fleet(&["effort", "--json", "pid:abc", "high"]);
    failed(code, &v, "target_invalid");
    let start = "2001-01-01T00:00:00Z";
    let (code, v, _) = a.effort("high", &["--expect-pid", &pid, "--expect-start", start]);
    failed(code, &v, "target_changed");
    assert_eq!(a.received(), "", "nothing typed");
}

#[test]
fn codex_is_harness_unsupported() {
    let f = FakeFleet::new().expect("these tests need tmux");
    std::fs::create_dir_all(f.env.path("home/.codex")).unwrap();
    let log = f.env.path("received.log");
    let logger = f.env.path("logger.pl");
    std::fs::write(&logger, LOGGER).unwrap();
    let pid = f.agent(
        "effort",
        SID,
        80,
        24,
        &format!(
            "exec /bin/bash -c \"exec -a codex perl '{}' '{}'\"",
            logger.display(),
            log.display()
        ),
    );
    std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
    let a = Agent {
        transcript: switch::transcript_of(&f, SID),
        f,
        pid,
        log,
    };
    a.f.wait_for("the agent to start", || a.log.exists());
    let (code, v, _) = a.effort("high", &[]);
    failed(code, &v, "harness_unsupported");
    assert_eq!(a.received(), "", "nothing typed");
}

// `model`'s checks guard it: not idle, a box holding text (`send_held`, exit 3, its text only
// in `input`), and `-n` checks and types nothing.
#[test]
fn the_checks_before_typing_are_models() {
    let a = agent("cold");
    a.status("busy");
    let (code, v, _) = a.effort("high", &[]);
    failed(code, &v, "not_idle");
    assert_eq!(v["status"], "busy", "{v}");
    let (code, v, _) = a.fleet(&["-n", "effort", "--json", &a.target(), "high"]);
    failed(code, &v, "not_idle");
    a.status("idle");
    let (code, v, err) = a.fleet(&["-n", "effort", "--json", &a.target(), "high"]);
    assert_eq!(code, 0, "{v} {err}");
    assert_eq!(
        (
            v["dry_run"].as_bool(),
            v["confirmed"].as_bool(),
            v["saved"].is_null(),
            v["switched_at"].is_null()
        ),
        (Some(true), Some(false), true, true),
        "{v}"
    );
    assert_eq!(a.received(), "", "nothing typed");

    let a = agent_env("cold", "live/claude-typed", "");
    let (code, v, err) = a.effort("high", &["--require-empty"]);
    assert_eq!(code, 3, "{v} {err}");
    assert_eq!(
        (v["kind"].as_str(), v["reason"].as_str()),
        (Some("send_held"), Some("text")),
        "{v}"
    );
    assert_eq!(v["input"], "draft text typed but not sent", "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("`/effort` would go out"),
        "{v}"
    );
    assert_eq!(a.received(), "", "nothing typed");
}
