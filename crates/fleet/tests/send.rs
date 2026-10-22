//! `fleet send` (and `rename`, which reads the input box the same way) against fake agents on
//! the test's private tmux server: each draws one of the captured screens in testdata/screens
//! (`live/…`: a live capture of a real session, drawn in a pane its size), then logs every byte it reads, so the test sees whether the input box check held the text or
//! let it through. A tmux wrapper that tries the session's lock on every call shows that the
//! box is read, and the text typed, under one lock.
//!
//! These tests need tmux and perl, and fail without them rather than pass having run nothing.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fake::FakeFleet;
use common::{stderr, stdout};
use serde_json::Value;

const SID: &str = "bbbbbbbb-1111-2222-3333-444444444444";

/// What the fake agent runs once it has drawn its screen: every byte it reads, appended to
/// the file it is given.
const LOGGER: &str = r#"open(my $f, ">>", $ARGV[0]) or die; select((select($f), $| = 1)[0]);
while (sysread(STDIN, my $b, 64)) { print $f $b }
"#;

fn screen(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../testdata/screens/{name}.txt"))
}

/// A fake session: what it runs, the pid fleet knows it by, and the log of what it read.
struct Agent {
    f: FakeFleet,
    pid: i64,
    target: String,
    log: PathBuf,
}

/// A fake Claude session that draws `name` and then logs what it reads.
fn agent(name: &str) -> Agent {
    agent_of("claude", name)
}

/// A fake `harness` session (`claude`, `codex`, `pi`) that draws `name` and then logs what it
/// reads. Claude is found by its registry entry; Codex and pi, as fleet finds them, by their
/// command name (and their state dir under the temp `$HOME`). Panics without tmux or perl.
fn agent_of(harness: &str, name: &str) -> Agent {
    agent_as(harness, name, ":")
}

/// A fake Claude session that draws `name`, runs `after` (sh), then logs what it reads.
fn agent_after(name: &str, after: &str) -> Agent {
    agent_as("claude", name, after)
}

fn agent_as(harness: &str, name: &str, after: &str) -> Agent {
    assert!(
        fleet::core::tools::find_binary("perl").is_some(),
        "these tests need perl"
    );
    let f = FakeFleet::new().expect("these tests need tmux");
    let log = f.env.path("received.log");
    let logger = f.env.path("logger.pl");
    std::fs::write(&logger, LOGGER).unwrap();
    let run = format!("perl '{}' '{}'", logger.display(), log.display());
    let run = match harness {
        "claude" => format!("exec {run}"),
        h => {
            let home = if h == "pi" {
                "home/.pi/agent"
            } else {
                "home/.codex"
            };
            std::fs::create_dir_all(f.env.path(home)).unwrap();
            format!("exec /bin/bash -c \"exec -a {h} {run}\"")
        }
    };
    let script = format!(
        "cat '{}'; {after}; stty raw -echo; {run}",
        screen(name).display()
    );
    // A live capture is of a 110x34 pane: narrower, its rules would wrap.
    let (cols, rows) = if name.starts_with("live/") {
        (110, 34)
    } else {
        (80, 24)
    };
    let pid = f.agent("send", SID, cols, rows, &script);
    if harness != "claude" {
        std::fs::remove_file(f.env.path(&format!("home/.claude/sessions/{pid}.json"))).unwrap();
    }
    // The log appears once the screen is drawn and perl owns the tty.
    f.wait_for("the agent to draw its screen", || log.exists());
    Agent {
        f,
        pid,
        target: format!("pid:{pid}"),
        log,
    }
}

impl Agent {
    fn fleet(&self, args: &[&str]) -> std::process::Output {
        self.f.fleet(args)
    }

    /// What the agent has read, once a moment has passed for anything on its way.
    fn received(&self) -> String {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Wait for what the agent has read to end in Enter.
    fn wait_for_enter(&self) {
        self.f.wait_for("the text and its Enter", || {
            std::fs::read_to_string(&self.log).is_ok_and(|r| r.ends_with('\r'))
        });
    }
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

/// `{ ok: false, host, kind: "send_held", reason, error, input? }`, exit 3; `input` the box's
/// text, which appears nowhere else — not in `error`, not on stderr.
fn held(out: &std::process::Output, reason: &str, input: Option<&str>) {
    assert_eq!(out.status.code(), Some(3), "{}", stderr(out));
    let v = json(out);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["kind"], "send_held", "{v}");
    assert_eq!(v["reason"], reason, "{v}");
    assert!(v["host"].as_str().is_some_and(|h| !h.is_empty()), "{v}");
    match input {
        Some(t) => {
            assert_eq!(v["input"], t, "{v}");
            assert!(!v["error"].as_str().unwrap().contains(t), "{v}");
            assert!(!stderr(out).contains(t), "{}", stderr(out));
        }
        None => assert_eq!(v["input"], Value::Null, "{v}"),
    }
}

// Escape stopped a turn before Claude Code wrote anything, and it put the prompt back in its
// input box: a send would go out with it as one prompt. Held, exit 3, nothing typed.
#[test]
fn text_in_the_input_box_holds_the_send() {
    let a = agent("live/claude-restored");
    let text = "Write a 300-word essay about tmux.";
    held(
        &a.fleet(&["send", "--json", &a.target, "hello"]),
        "text",
        Some(text),
    );

    let out = a.fleet(&["send", &a.target, "hello"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("already holds text"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains(text), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert_eq!(a.received(), "", "nothing typed");

    // --force types as before the check: after what is there — --require-empty or not.
    let out = a.fleet(&["send", "--force", "--require-empty", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// A placeholder (faint, the drawn cursor on its first character) is an empty box.
#[test]
fn an_empty_input_box_takes_the_send_placeholder_and_all() {
    let a = agent("claude-placeholder");
    let out = a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["warning"], Value::Null, "{v}");
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// An IDE connected: its selection's pill leads the empty box. Not text.
#[test]
fn an_ide_pill_in_an_empty_box_takes_the_send() {
    let a = agent("claude-ide-pill");
    let out = a.fleet(&["send", "--json", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// A prompt waiting for its answer where the box was: typed text would be its answer. Held.
#[test]
fn a_prompt_in_place_of_the_box_holds_the_send() {
    let a = agent("claude-permission");
    held(
        &a.fleet(&["send", "--json", &a.target, "hello"]),
        "prompt",
        None,
    );
    assert_eq!(a.received(), "", "nothing typed into the prompt");
}

// No input box fleet can find (here the transcript view): typed as before the check, with a
// warning — unless --require-empty, which holds.
#[test]
fn no_input_box_types_with_a_warning_unless_an_empty_box_is_required() {
    let a = agent("claude-transcript");
    held(
        &a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]),
        "no_input_box",
        None,
    );
    assert_eq!(a.received(), "", "nothing typed");

    let out = a.fleet(&["send", "--json", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(
        (v["ok"].as_bool(), v["warning"].as_str()),
        (Some(true), Some("no_input_box")),
        "{v}"
    );
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 1, "one line: {err}");
    assert!(
        err.starts_with("warning:") && err.contains("cannot find"),
        "{err}"
    );
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// Codex: a stopped prompt's words in its composer hold the send; an empty composer takes it.
// Both drawn on a 24-bit background, under a footer fleet finds the composer without.
#[test]
fn codex_a_restored_prompt_holds_the_send_and_an_empty_composer_takes_it() {
    let a = agent_of("codex", "codex-status-restored");
    held(
        &a.fleet(&["send", "--json", &a.target, "hello"]),
        "text",
        Some("fix the flaky login test"),
    );
    assert_eq!(a.received(), "", "nothing typed");

    let a = agent_of("codex", "codex-status");
    let out = a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// A recalled image prompt, its text cleared: Codex's `[Image #1]` row above the composer would
// go out with the send. Held as text, --require-empty or not.
#[test]
fn codex_an_image_attachment_holds_the_send() {
    let a = agent_of("codex", "codex-image");
    for args in [
        &["send", "--json"][..],
        &["send", "--json", "--require-empty"],
    ] {
        let mut args = args.to_vec();
        args.extend([a.target.as_str(), "hello"]);
        held(&a.fleet(&args), "text", Some("[Image #1]"));
    }
    assert_eq!(a.received(), "", "nothing typed");
}

// Codex's usage-limit menu ("Switch to … for lower credit usage?", a live capture) is not read
// as a prompt, so it is no input box. A known gap (cli.md, Sending): a plain send types there
// with a warning, and its Enter would pick the menu's first choice; --require-empty holds.
#[test]
fn codex_the_rate_limit_menu_holds_only_a_send_that_requires_an_empty_box() {
    let a = agent_of("codex", "live/codex-menu-ratelimit");
    held(
        &a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]),
        "no_input_box",
        None,
    );
    assert_eq!(a.received(), "", "nothing typed into the menu");

    let out = a.fleet(&["send", "--json", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&out)["warning"], "no_input_box", "{}", stdout(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r", "typed, Enter and all: the gap");
}

// pi: text in its editor holds the send; an empty editor takes it.
#[test]
fn pi_text_in_the_editor_holds_the_send_and_an_empty_editor_takes_it() {
    let a = agent_of("pi", "pi-text");
    held(
        &a.fleet(&["send", "--json", &a.target, "hello"]),
        "text",
        Some("now run the full suite and open a PR"),
    );
    assert_eq!(a.received(), "", "nothing typed");

    let a = agent_of("pi", "pi-empty");
    let out = a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// `/rename` typed after a restored prompt would submit both: the stopped prompt runs again. The
// rename reads the box under its lock, as send does, and holds (`held: "input"`).
#[test]
fn text_in_the_input_box_holds_a_rename() {
    let a = agent("live/claude-restored");
    let out = a.fleet(&["rename", &a.target, "probe", "--no-tmux-sync", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(
        (v["result"].as_str(), v["held"].as_str()),
        (Some("held"), Some("input")),
        "{v}"
    );
    assert!(!v["message"].as_str().unwrap().contains("essay"), "{v}");
    assert_eq!(a.received(), "", "nothing typed");
}

/// Play Claude Code's part: once `/rename <name>` and its Enter arrive, take the name.
fn take_rename(a: &Agent, name: &'static str) -> std::thread::JoinHandle<()> {
    let reg =
        a.f.env
            .path(&format!("home/.claude/sessions/{}.json", a.pid));
    let log = a.log.clone();
    std::thread::spawn(move || {
        for _ in 0..200 {
            if std::fs::read_to_string(&log)
                .is_ok_and(|r| r.ends_with(&format!("/rename {name}\r")))
            {
                let mut v: Value =
                    serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
                v["name"] = name.into();
                std::fs::write(&reg, v.to_string()).unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
}

// An empty box (its placeholder showing) holds no rename: `/rename` goes out — with
// --require-empty or without.
#[test]
fn an_empty_input_box_takes_a_rename() {
    for flags in [&[][..], &["--require-empty"]] {
        let a = agent("claude-placeholder");
        let renamer = take_rename(&a, "probe");
        let mut args = vec!["rename", &a.target, "probe", "--no-tmux-sync", "--json"];
        args.extend(flags);
        let out = a.fleet(&args);
        renamer.join().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(json(&out)["result"], "renamed");
        assert_eq!(a.received(), "/rename probe\r");
    }
}

/// `{ ok: false, result: "held", held: <reason>, message, … }`, exit 3, nothing typed.
fn rename_held(out: &std::process::Output, reason: &str) {
    assert_eq!(out.status.code(), Some(3), "{}", stderr(out));
    let v = json(out);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["result"], "held", "{v}");
    assert_eq!(v["held"], reason, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("nothing sent"),
        "{v}"
    );
}

// A screen with no input box fleet can find (here the transcript view, as an open `/usage`
// panel would be): `rename --require-empty` holds with `send`'s reason; a plain rename types
// as it always did.
#[test]
fn no_input_box_holds_a_rename_only_when_an_empty_box_is_required() {
    let a = agent("claude-transcript");
    rename_held(
        &a.fleet(&[
            "rename",
            &a.target,
            "probe",
            "--no-tmux-sync",
            "--json",
            "--require-empty",
        ]),
        "no_input_box",
    );
    assert_eq!(a.received(), "", "nothing typed");

    let renamer = take_rename(&a, "probe");
    let out = a.fleet(&["rename", &a.target, "probe", "--no-tmux-sync", "--json"]);
    renamer.join().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&out)["result"], "renamed");
    assert_eq!(a.received(), "/rename probe\r");
}

// Text in the box holds a rename with --require-empty as without.
#[test]
fn text_in_the_input_box_holds_a_rename_with_require_empty_too() {
    let a = agent("live/claude-restored");
    rename_held(
        &a.fleet(&[
            "rename",
            &a.target,
            "probe",
            "--no-tmux-sync",
            "--json",
            "--require-empty",
        ]),
        "input",
    );
    assert_eq!(a.received(), "", "nothing typed");
}

// A box fleet cannot read, through the real rename: Codex's footer drawn plain is
// indistinguishable from words typed in the composer. `--require-empty` holds the rename with
// `send`'s reason (the whole JSON report, exit 3; plain, one error line, exit 1), and
// nothing is typed.
#[test]
fn an_unreadable_input_box_holds_a_rename_that_requires_an_empty_box() {
    let a = agent_of("codex", "codex-footer-plain");
    let out = a.fleet(&[
        "rename",
        &a.target,
        "probe",
        "--no-tmux-sync",
        "--json",
        "--require-empty",
    ]);
    rename_held(&out, "unreadable");
    let v = json(&out);
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "from",
            "held",
            "host",
            "message",
            "ok",
            "pid",
            "result",
            "session_id",
            "title",
            "tmux"
        ],
        "{v}"
    );
    assert_eq!(v["pid"], a.pid, "{v}");
    assert_eq!(v["title"], "probe", "{v}");
    assert!(v["from"].as_str().is_some_and(|f| !f.is_empty()), "{v}");
    assert!(v["host"].as_str().is_some_and(|h| !h.is_empty()), "{v}");
    assert_eq!(v["tmux"], Value::Null, "{v}");
    assert_eq!(a.received(), "", "nothing typed");

    let out = a.fleet(&[
        "rename",
        &a.target,
        "probe",
        "--no-tmux-sync",
        "--require-empty",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "plain output holds no report");
    let err = stderr(&out);
    assert!(
        err.contains("no text attributes") && err.contains("nothing sent"),
        "{err}"
    );
    assert_eq!(a.received(), "", "nothing typed");
}

/// A fake Claude Code drawing its empty box (`live/claude-empty`), the terminal's cursor then
/// put `blanks` cells past where the box's text starts — as blanks typed there leave it, which
/// Claude Code draws as nothing — and hidden if `hidden`.
fn claude_with_cursor(blanks: usize, hidden: bool) -> Agent {
    let name = "live/claude-empty";
    let text = std::fs::read_to_string(screen(name)).unwrap();
    // The box's line on screen: the capture's last newline scrolled the 34-row pane a row up.
    let lines: Vec<&str> = text.split('\n').collect();
    let scroll = text.matches('\n').count().saturating_sub(33);
    let line = lines.iter().rposition(|l| l.contains('❯')).unwrap();
    let after = format!(
        "printf '\\033[{};{}H{}'",
        line - scroll + 1,
        3 + blanks,
        if hidden { "\\033[?25l" } else { "" }
    );
    agent_after(name, &after)
}

/// `fleet` with a tmux that fails every query for the cursor (`#{cursor_flag}`).
fn without_cursor(a: &Agent, args: &[&str]) -> std::process::Output {
    let wrapper = a.f.env.path("no-cursor-tmux.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in *cursor_flag*) echo 'no cursor here' >&2; exit 1;; esac\n\
             exec '{}' \"$@\"\n",
            a.f.env.tmux_wrapper().display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    a.f.env
        .cmd()
        .env("SHELL", "/bin/sh")
        .env("FLEET_TMUX", &wrapper)
        .timeout(Duration::from_secs(20))
        .args(args)
        .output()
        .unwrap()
}

// Blanks typed into Claude Code's box show as nothing; only the terminal's cursor tells them
// from an empty box. With it hidden, or its query failing, the box cannot be told empty:
// `--require-empty` holds (`unreadable`, no `input`), blanks or none, and a plain send types
// with a warning. With the cursor, as before: blanks are text, an empty box takes the send.
#[test]
fn without_the_cursor_a_box_showing_nothing_is_unreadable() {
    let required = |a: &Agent| {
        vec![
            "send".to_string(),
            "--json".into(),
            "--require-empty".into(),
            a.target.clone(),
            "hello".into(),
        ]
    };
    let run = |a: &Agent, failing: bool| {
        let args = required(a);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if failing {
            without_cursor(a, &args)
        } else {
            a.fleet(&args)
        }
    };
    for blanks in [0, 3] {
        // Hidden.
        let a = claude_with_cursor(blanks, true);
        held(&run(&a, false), "unreadable", None);
        assert_eq!(a.received(), "", "{blanks} blanks, hidden: nothing typed");
        // The query failing.
        let a = claude_with_cursor(blanks, false);
        held(&run(&a, true), "unreadable", None);
        assert_eq!(
            a.received(),
            "",
            "{blanks} blanks, no answer: nothing typed"
        );
    }
    // Shown: blanks are text, none is empty.
    let a = claude_with_cursor(3, false);
    held(&run(&a, false), "text", Some("   "));
    let a = claude_with_cursor(0, false);
    let out = run(&a, false);
    assert!(out.status.success(), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
    // Without --require-empty, typed with the warning.
    let a = claude_with_cursor(0, true);
    let out = a.fleet(&["send", "--json", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&out)["warning"], "unreadable");
    assert!(stderr(&out).contains("cursor"), "{}", stderr(&out));
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

/// A fake Claude Code drawing `claude-placeholder` (80x24) with the terminal's cursor hidden, as
/// Claude Code hides it unless its native cursor is on, and paints its own caret in reverse
/// video: on the placeholder's first character as drawn, or — `blanks` given as `(before,
/// after)` — that line cleared to `before` blanks, a painted blank, then `after` blanks.
fn claude_painted_caret(blanks: Option<(usize, usize)>) -> Agent {
    let name = "claude-placeholder";
    let text = std::fs::read_to_string(screen(name)).unwrap();
    let lines: Vec<&str> = text.split('\n').collect();
    let scroll = text.matches('\n').count().saturating_sub(23);
    let line = lines.iter().rposition(|l| l.contains('❯')).unwrap();
    let caret = match blanks {
        Some((before, after)) => format!(
            "\\033[{};3H\\033[K{}\\033[7m \\033[0m{}",
            line - scroll + 1,
            " ".repeat(before),
            " ".repeat(after)
        ),
        None => String::new(),
    };
    agent_after(name, &format!("printf '{caret}\\033[?25l'"))
}

// Claude Code's default: the terminal's cursor hidden (`#{cursor_flag}` 0), its caret painted
// in reverse video in the box. On the placeholder's first character it says the box is empty:
// `--require-empty` takes the send, no warning. So it does on a painted blank where the text
// starts, as Claude Code's box is after a turn — blanks after it or none: three blanks typed,
// then Home, look just so (an accepted gap). Painted past blanks typed there: `text`, held.
#[test]
fn with_the_cursor_hidden_the_caret_claude_code_paints_tells() {
    let a = claude_painted_caret(None);
    let out = a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["warning"], Value::Null, "{v}");
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");

    for after in [0, 3] {
        let a = claude_painted_caret(Some((0, after)));
        let out = a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]);
        assert!(out.status.success(), "{after}: {}", stderr(&out));
        assert_eq!(json(&out)["warning"], Value::Null, "{after}");
        a.wait_for_enter();
        assert_eq!(a.received(), "hello\r", "{after} blanks after the caret");
    }

    let a = claude_painted_caret(Some((3, 0)));
    held(
        &a.fleet(&["send", "--json", "--require-empty", &a.target, "hello"]),
        "text",
        Some("   "),
    );
    assert_eq!(a.received(), "", "nothing typed");
}

// The typing lock cannot be taken (its directory cannot be made): `io`, with `typed: false` —
// nothing reached the pane, so a caller may try again. `send`, `keys`, `rename` and `answer`
// alike.
#[test]
fn an_io_failure_before_typing_says_nothing_was_typed() {
    let a = agent("live/claude-empty");
    let blocker = a.f.env.path("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let t = a.target.as_str();
    for args in [
        vec!["send", "--json", t, "hello"],
        vec!["keys", "--json", t, "Escape"],
        vec!["rename", t, "probe", "--no-tmux-sync", "--json"],
        vec!["answer", t, "--choice", "1", "--json"],
    ] {
        let out =
            a.f.env
                .cmd()
                .env("SHELL", "/bin/sh")
                .env("FLEET_LOCK_DIR", blocker.join("locks"))
                .timeout(Duration::from_secs(20))
                .args(&args)
                .output()
                .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let v = json(&out);
        assert_eq!(
            (v["ok"].as_bool(), v["kind"].as_str(), v["typed"].as_bool()),
            (Some(false), Some("io"), Some(false)),
            "{args:?}: {v}"
        );
    }
    assert_eq!(a.received(), "", "nothing typed");
}

/// `fleet` with a tmux that, on every call, first tries the session's typing lock and logs
/// `held` or `free` with its arguments; the calls logged so far.
fn under_probe(f: &FakeFleet, args: &[&str]) -> (std::process::Output, Vec<(bool, String)>) {
    let probe = f.env.path("probe-tmux.sh");
    let calls = f.env.path("probe.log");
    let _ = std::fs::remove_file(&calls);
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

/// Where the calls read the input box (`capture-pane … -e`).
fn styled_reads(calls: &[(bool, String)]) -> Vec<usize> {
    (0..calls.len())
        .filter(|&i| calls[i].1.contains("capture-pane") && calls[i].1.contains("-e"))
        .collect()
}

/// Where the calls type: keys, or a paste.
fn typing(calls: &[(bool, String)]) -> Vec<usize> {
    (0..calls.len())
        .filter(|&i| {
            let c = &calls[i].1;
            c.contains("send-keys") || c.contains("load-buffer") || c.contains("paste-buffer")
        })
        .collect()
}

/// The box is read before anything is typed, and every call from that read to the last key
/// holds the session's lock: nothing in between let go of it.
fn read_then_typed_under_one_lock(calls: &[(bool, String)]) {
    let reads = styled_reads(calls);
    let typed = typing(calls);
    let (Some(&read), Some(&last)) = (reads.first(), typed.last()) else {
        panic!("a styled read and typing: {calls:?}");
    };
    assert!(read < typed[0], "read before typing: {calls:?}");
    assert!(
        calls[read..=last].iter().all(|(held, _)| *held),
        "{calls:?}"
    );
}

// The check and the send are one critical section: the input box is read (`capture-pane -e`)
// with the session's lock held, and every key of the text goes out still under it.
#[test]
fn the_box_is_read_and_the_text_typed_under_one_lock() {
    let a = agent("claude-placeholder");
    let (out, calls) = under_probe(&a.f, &["send", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    read_then_typed_under_one_lock(&calls);
    assert_eq!(a.received(), "hello\r");
}

// Text of more than one line goes out as a paste (load-buffer, paste-buffer), then Enter: the
// read and all of it under the one lock.
#[test]
fn a_multi_line_send_is_read_and_pasted_under_one_lock() {
    let a = agent("claude-placeholder");
    let (out, calls) = under_probe(&a.f, &["send", &a.target, "line one\nline two"]);
    assert!(out.status.success(), "{}", stderr(&out));
    read_then_typed_under_one_lock(&calls);
    for verb in ["load-buffer", "paste-buffer"] {
        assert!(
            calls.iter().any(|(_, c)| c.contains(verb)),
            "{verb}: {calls:?}"
        );
    }
    a.wait_for_enter();
    assert_eq!(a.received(), "line one\nline two\r");
}

// --force reads nothing, but still types every key under the lock.
#[test]
fn a_forced_send_types_under_the_lock_without_reading_the_box() {
    let a = agent("live/claude-restored");
    let (out, calls) = under_probe(&a.f, &["send", "--force", &a.target, "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(styled_reads(&calls), Vec::<usize>::new(), "{calls:?}");
    let typed = typing(&calls);
    assert!(!typed.is_empty(), "{calls:?}");
    assert!(typed.iter().all(|&i| calls[i].0), "{calls:?}");
    a.wait_for_enter();
    assert_eq!(a.received(), "hello\r");
}

// Held, the read happened under the lock too — and nothing was typed after it.
#[test]
fn a_held_send_reads_under_the_lock_and_types_nothing() {
    let a = agent("live/claude-restored");
    let (out, calls) = under_probe(&a.f, &["send", &a.target, "hello"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let reads = styled_reads(&calls);
    assert!(!reads.is_empty(), "no styled capture: {calls:?}");
    assert!(
        reads.iter().all(|&i| calls[i].0),
        "read without the lock: {calls:?}"
    );
    assert_eq!(typing(&calls), Vec::<usize>::new(), "{calls:?}");
    assert_eq!(a.received(), "");
}

// A rename's read of the box is under the lock too, not only a send's: text that lands in the
// box before the lock is taken must still be seen. Held, nothing is typed after the read.
#[test]
fn a_held_rename_reads_under_the_lock_and_types_nothing() {
    let a = agent("live/claude-restored");
    let (out, calls) = under_probe(
        &a.f,
        &["rename", &a.target, "probe", "--no-tmux-sync", "--json"],
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert_eq!(json(&out)["held"], "input");
    let reads = styled_reads(&calls);
    assert!(!reads.is_empty(), "no styled capture: {calls:?}");
    assert!(
        reads.iter().all(|&i| calls[i].0),
        "read without the lock: {calls:?}"
    );
    assert_eq!(typing(&calls), Vec::<usize>::new(), "{calls:?}");
    assert_eq!(a.received(), "");
}
