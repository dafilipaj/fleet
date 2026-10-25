//! Backend adapters. Each session is controlled through the backend it actually
//! runs in — iTerm via AppleScript keyed on the session GUID, tmux via the pane id.

use std::process::Command;

use crate::core::discovery::{Backend, Session};
use crate::core::harness::Harness;
use crate::error::{Error, Result};

/// AppleScript helper: locate an iTerm session by its `id` (the GUID that also
/// appears in ITERM_SESSION_ID).
const ITERM_FIND: &str = r#"on findSession(theId)
  tell application "iTerm2"
    repeat with w in windows
      repeat with t in tabs of w
        repeat with s in sessions of t
          if (id of s) is theId then return s
        end repeat
      end repeat
    end repeat
  end tell
  return missing value
end findSession"#;

/// Run a static AppleScript, passing dynamic values as argv (injection-safe).
fn osa(script: &str, args: &[&str]) -> Result<String> {
    let file = std::env::temp_dir().join(format!("fleet-{}.applescript", std::process::id()));
    std::fs::write(&file, script)?;
    let mut cmd = Command::new("osascript");
    cmd.arg(&file);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "osascript: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Refuse to drive a backend while a fixture is standing in for the registry.
///
/// A canned fleet's handles are fabricated: `tmux send-keys -t %99` and the iTerm
/// AppleScript would either fail loudly or, worse, hit whatever really answers to
/// that handle. `FLEET_FIXTURE` is documented as a demo/screenshot mode, so it
/// has to be inert.
fn deny_fixture() -> Result<()> {
    if crate::core::discovery::is_fixture() {
        return Err(Error::Other(
            "fixture mode: backends are inert (unset FLEET_FIXTURE)".into(),
        ));
    }
    Ok(())
}

fn require_handle(s: &Session) -> Result<&str> {
    s.handle.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "session {} has no controllable backend (backend={})",
            s.label(),
            s.backend.label()
        ))
    })
}

pub fn peek(s: &Session) -> Result<String> {
    deny_fixture()?;
    let handle = require_handle(s)?;
    match s.backend {
        Backend::Iterm => osa(
            &format!(
                r#"{ITERM_FIND}
on run argv
  set s to findSession(item 1 of argv)
  if s is missing value then error "session not found"
  tell application "iTerm2" to return text of s
end run"#
            ),
            &[handle],
        ),
        Backend::Tmux => {
            let out = crate::core::tools::tmux_cmd()
                .args(["capture-pane", "-t", handle, "-p"])
                .output()?;
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Backend::Unknown => require_handle(s).map(|_| String::new()),
    }
}

/// One `tmux send-keys` at `handle`, failing when tmux says it failed.
fn send_keys(handle: &str, rest: &[&str]) -> Result<()> {
    let mut cmd = crate::core::tools::tmux_cmd();
    cmd.args(["send-keys", "-t", handle]).args(rest);
    tmux_ok(cmd.output()?, &format!("tmux send-keys -t {handle}"))
}

/// Largest piece of text one `send-keys -l` types. Claude Code takes a burst of more than
/// ~800 bytes as a paste (collapsed into `[Pasted text]`), and from one write of more than
/// ~1 KB it keeps only what follows the first 1 KB.
const TYPE_CHUNK: usize = 512;

/// Split `text` into pieces of at most `max` bytes, never inside a UTF-8 character.
fn type_chunks(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        out.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// Type `text` into `handle` literally, a [`TYPE_CHUNK`] at a time, so a long line reaches
/// the TUI whole and as typed text rather than a paste.
fn type_text(handle: &str, text: &str) -> Result<()> {
    for (i, chunk) in type_chunks(text, TYPE_CHUNK).into_iter().enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        send_keys(handle, &["-l", "--", chunk])?;
    }
    Ok(())
}

/// Paste `text` into `handle` as a bracketed paste: tmux (3.6) drops newlines typed by
/// `send-keys -l`, while a paste keeps them, so a multi-line message stays multi-line.
fn paste_text(handle: &str, text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;
    let buf = format!("fleet-send-{}", std::process::id());
    let mut load = crate::core::tools::tmux_cmd();
    load.args(["load-buffer", "-b", &buf, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = load.spawn()?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(text.as_bytes())?;
    tmux_ok(child.wait_with_output()?, "tmux load-buffer")?;
    let mut paste = crate::core::tools::tmux_cmd();
    paste.args(["paste-buffer", "-p", "-r", "-d", "-b", &buf, "-t", handle]);
    tmux_ok(paste.output()?, &format!("tmux paste-buffer -t {handle}"))
}

fn tmux_ok(out: std::process::Output, what: &str) -> Result<()> {
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let why = if why.is_empty() {
            format!("exited {}", out.status)
        } else {
            why
        };
        return Err(Error::Other(format!("{what}: {why}")));
    }
    Ok(())
}

pub fn send(s: &Session, text: &str) -> Result<()> {
    deny_fixture()?;
    let handle = require_handle(s)?;
    match s.backend {
        Backend::Iterm => {
            // Two writes: a single `write text` sends text+newline as one chunk,
            // which the bracketed-paste-aware TUI absorbs as a literal newline
            // instead of submitting. Type without newline, then a bare Enter.
            osa(
                &format!(
                    r#"{ITERM_FIND}
on run argv
  set s to findSession(item 1 of argv)
  if s is missing value then error "session not found"
  tell application "iTerm2" to tell s
    write text (item 2 of argv) without newline
    delay 0.2
    write text ""
  end tell
end run"#
                ),
                &[handle, text],
            )?;
            Ok(())
        }
        Backend::Tmux => {
            // A non-zero `send-keys` ("can't find pane") is the pane having gone
            // away, not a spawn failure — `?` alone would swallow it and report
            // a `/rename` that never landed as success, after which the tmux
            // session gets renamed anyway.
            if text.contains(['\n', '\r']) {
                paste_text(handle, text)?;
                // Let the TUI finish taking the paste, or Enter can land inside it.
                std::thread::sleep(std::time::Duration::from_millis(150));
            } else {
                type_text(handle, text)?;
                // Codex and pi take keys arriving in one burst as a paste, and an Enter
                // inside it never submits (Codex's slash-command popup also needs a beat).
                if s.harness != Harness::Claude {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
            send_keys(handle, &["Enter"])?;
            Ok(())
        }
        Backend::Unknown => require_handle(s).map(|_| ()),
    }
}

/// Bring a session's terminal to the front (its iTerm tab, or its tmux window/pane).
pub fn focus(s: &Session) -> Result<()> {
    deny_fixture()?;
    let handle = require_handle(s)?;
    match s.backend {
        Backend::Iterm => {
            osa(
                r#"on run argv
  set theId to item 1 of argv
  tell application "iTerm2"
    activate
    repeat with w in windows
      repeat with t in tabs of w
        repeat with s in sessions of t
          if (id of s) is theId then
            select w
            select t
            select s
            return
          end if
        end repeat
      end repeat
    end repeat
  end tell
  error "session not found"
end run"#,
                &[handle],
            )?;
            Ok(())
        }
        Backend::Tmux => {
            // Resolve the pane's window, select both; switch the client too if we're in tmux.
            let win = crate::core::tools::tmux_cmd()
                .args([
                    "display-message",
                    "-p",
                    "-t",
                    handle,
                    "#{session_name}:#{window_index}",
                ])
                .output()?;
            let win = String::from_utf8_lossy(&win.stdout).trim().to_string();
            if !win.is_empty() {
                crate::core::tools::tmux_cmd()
                    .args(["select-window", "-t", &win])
                    .status()?;
                if std::env::var_os("TMUX").is_some()
                    && let Some(session) = win.split(':').next()
                {
                    crate::core::tools::tmux_cmd()
                        .args(["switch-client", "-t", session])
                        .status()?;
                }
            }
            crate::core::tools::tmux_cmd()
                .args(["select-pane", "-t", handle])
                .status()?;
            Ok(())
        }
        Backend::Unknown => require_handle(s).map(|_| ()),
    }
}

// --- tmux session naming -----------------------------------------------------

/// What happened to a session's tmux name. A skip is a normal outcome — iTerm
/// sessions, shared tmux sessions and the dashboard's own session all land here
/// — so it is never an error the caller has to apologise for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxSync {
    Renamed { from: String, to: String },
    Skipped(String),
}

impl std::fmt::Display for TmuxSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TmuxSync::Renamed { from, to } => write!(f, "⧉ {from} → {to}"),
            TmuxSync::Skipped(why) => f.write_str(why),
        }
    }
}

/// Make a name tmux will store verbatim.
///
/// tmux silently rewrites `:` and `.` in a session name to `_` — renaming one to
/// `foo:bar.baz` yields `foo_bar_baz`. Doing the substitution ourselves (to `-`,
/// which reads better) means tmux's own rewrite never fires, and the collision
/// pre-check compares the string that actually lands.
pub fn sanitize_tmux_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c == ':' || c == '.' || c.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c);
        }
    }
    // A leading `-` would be parsed as a flag by `tmux rename-session`.
    let out = out.trim_start_matches('-');
    // Capped for the same reason the Claude name is: tmux will happily carry a
    // 300-character session name into every status line on the machine.
    out.chars().take(MAX_TMUX_NAME).collect()
}

/// The longest tmux session name this will produce.
pub const MAX_TMUX_NAME: usize = 64;

/// `desired`, or `desired-2`, `desired-3`… until it collides with nothing in
/// `existing`.
///
/// Pure on purpose: `has-session -t X` does prefix/fnmatch matching and answers
/// "yes" for a name that doesn't exist, so the only correct check is an exact
/// compare against the full list — and that list is worth being able to fake.
pub fn unique_tmux_name(desired: &str, existing: &[String]) -> String {
    let taken = |n: &str| existing.iter().any(|e| e == n);
    if !taken(desired) {
        return desired.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{desired}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{desired}-{}", std::process::id())
}

/// Reasons to leave a tmux session's name alone, or `None` to go ahead.
///
/// Pure so every guard is testable without a live server. The one-session-one-job
/// convention is what makes renaming a tmux session meaningful at all; where the
/// convention visibly doesn't hold (more than one window or pane), the name is
/// somebody's filing system and not ours to rewrite.
pub fn tmux_guard(
    current: &str,
    own: Option<&str>,
    windows: usize,
    panes: usize,
) -> Option<String> {
    if current == "fleet" {
        return Some("left `fleet` alone — the switch-client binding depends on it".into());
    }
    if own == Some(current) {
        return Some(format!(
            "left `{current}` alone — it's this dashboard's own session"
        ));
    }
    if windows > 1 || panes > 1 {
        return Some(format!(
            "left `{current}` alone — {windows} windows / {panes} panes, so it isn't one job"
        ));
    }
    None
}

/// Every live tmux session name, exactly as tmux stores it.
pub(crate) fn tmux_session_names() -> Vec<String> {
    crate::core::tools::tmux_cmd()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// One `tmux display-message -p -t <target> <format>`, or why it didn't answer.
fn tmux_query(target: &str, format: &str) -> std::result::Result<String, String> {
    let out = crate::core::tools::tmux_cmd()
        .args(["display-message", "-p", "-t", target, format])
        .output()
        .map_err(|e| format!("cannot run tmux: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if why.is_empty() {
            format!("tmux exited {}", out.status)
        } else {
            why
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err(format!("tmux gave no answer for {format}"));
    }
    Ok(text)
}

/// The tmux session a target (a pane id) belongs to, *now*.
///
/// The one on [`Session`] is a poll snapshot up to `--interval` old, while the
/// rename itself resolves live from the pane id. A pane that moved between polls
/// would otherwise be guarded against its old session name and renamed in its
/// new one — including `fleet`, which is precisely what the guard protects.
fn tmux_session_of(target: &str) -> std::result::Result<String, String> {
    tmux_query(target, "#{session_name}")
}

/// The tmux session this process is running inside, via `$TMUX_PANE`.
///
/// `Ok(None)` means "not inside tmux at all". An error is *not* that: reporting
/// a failed lookup as "not mine" would let the dashboard rename the session it
/// is drawing itself in, so the caller has to fail closed on it.
fn own_tmux_session() -> std::result::Result<Option<String>, String> {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return Ok(None);
    };
    tmux_query(&pane, "#{session_name}").map(Some)
}

/// `(windows, panes)` in the session a target belongs to. Errors rather than
/// guessing `(1, 1)`: that guess reads as "one session, one job" and waves the
/// rename through on exactly the tmux failure that should have stopped it.
pub(crate) fn tmux_shape(target: &str) -> std::result::Result<(usize, usize), String> {
    let windows: usize = tmux_query(target, "#{session_windows}")?
        .parse()
        .map_err(|_| "tmux gave an unreadable window count".to_string())?;
    let out = crate::core::tools::tmux_cmd()
        .args(["list-panes", "-s", "-t", target, "-F", "#{pane_id}"])
        .output()
        .map_err(|e| format!("cannot run tmux: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if why.is_empty() {
            format!("tmux list-panes exited {}", out.status)
        } else {
            why
        });
    }
    let panes = String::from_utf8_lossy(&out.stdout).lines().count().max(1);
    Ok((windows, panes))
}

/// Bring a session's tmux *session* name in line with its title — a slug of it,
/// see [`crate::core::title::plan_tmux`] for every rule.
///
/// The target is the pane id where we have one: it is session-name independent,
/// so nothing here can be aimed at the wrong session by a name containing `:`.
pub fn rename_tmux_session(s: &Session, title: &str) -> Result<TmuxSync> {
    use crate::core::title::{TmuxPlan, plan_tmux};
    if crate::core::discovery::is_fixture() {
        return Ok(TmuxSync::Skipped("fixture mode: tmux untouched".into()));
    }
    if s.backend != Backend::Tmux {
        return Ok(TmuxSync::Skipped(format!(
            "no tmux session to rename ({})",
            s.backend.label()
        )));
    }
    // The pane id, never a name: `-t a:b` on a session called `a:b` resolves as
    // session `a`, window `b`. Discovery fills `handle` and `tmux_session` from
    // the same pane lookup, so no handle means no tmux session either.
    let Some(target) = s.handle.as_deref().filter(|h| !h.is_empty()) else {
        return Ok(TmuxSync::Skipped("tmux session unknown".into()));
    };
    // Everything below resolves live from the pane, the guards included.
    let current = match tmux_session_of(target) {
        Ok(c) => c,
        Err(why) => return Ok(TmuxSync::Skipped(format!("tmux session unknown: {why}"))),
    };
    let (windows, panes) = match tmux_shape(target) {
        Ok(shape) => shape,
        Err(why) => {
            return Ok(TmuxSync::Skipped(format!(
                "left `{current}` alone — tmux would not say its shape: {why}"
            )));
        }
    };
    let own = match own_tmux_session() {
        Ok(own) => own,
        Err(why) => {
            return Ok(TmuxSync::Skipped(format!(
                "left `{current}` alone — cannot tell if it is this dashboard's own: {why}"
            )));
        }
    };
    let to = match plan_tmux(
        title,
        &current,
        own.as_deref(),
        windows,
        panes,
        &tmux_session_names(),
    ) {
        TmuxPlan::Skip(why) => return Ok(TmuxSync::Skipped(why)),
        TmuxPlan::Rename { to, .. } => to,
    };
    let out = crate::core::tools::tmux_cmd()
        .args(["rename-session", "-t", target, &to])
        .output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(Error::Other(format!("tmux rename-session: {why}")));
    }
    Ok(TmuxSync::Renamed { from: current, to })
}

/// Shell-quote a value for a single-quoted context.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quote text that gets *typed* into a shell. A raw newline inside '…' does not survive
/// being typed (zsh drops it), so multi-line text goes as ANSI-C `$'…'` with escapes.
fn shq_typed(s: &str) -> String {
    if !s.contains(['\n', '\r', '\t']) {
        return shq(s);
    }
    let mut out = String::from("$'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// How the first prompt reaches the spawned session.
#[derive(Clone, Copy)]
pub enum Prompt<'a> {
    /// Start Claude with no prompt.
    None,
    /// A short one-liner, quoted straight onto the command line.
    Inline(&'a str),
    /// A file whose contents become the prompt. The command line only carries the
    /// path — the shell expands it at launch, so a multi-line brief of any shape
    /// survives AppleScript/send-keys untouched.
    File(&'a str),
}

/// `claude -n <name>` (and `pi -n`) names the session up front, so it lands in the
/// fleet with a name you chose instead of the cwd-derived default. Codex has no such
/// flag: its session is named by the tmux session only.
fn with_name(launcher: &str, name: Option<&str>, harness: Harness) -> String {
    match name {
        Some(n) if harness.takes_name() => format!("{launcher} -n {}", shq(n)),
        _ => launcher.to_string(),
    }
}

fn run_command(prompt: Prompt, name: Option<&str>, launcher: &str, harness: Harness) -> String {
    let launcher = with_name(launcher, name, harness);
    match prompt {
        Prompt::None => launcher,
        Prompt::Inline("") => launcher,
        Prompt::Inline(p) => format!("{launcher} {}", shq_typed(p)),
        Prompt::File(path) => format!("{launcher} \"$(cat {})\"", shq(path)),
    }
}

/// The shell line a spawn types: `cd <dir> && <launcher> [-n name] [prompt]`.
pub fn launch_command(
    dir: &str,
    prompt: Prompt,
    name: Option<&str>,
    launcher: &str,
    harness: Harness,
) -> String {
    format!(
        "cd {} && {}",
        shq(dir),
        run_command(prompt, name, launcher, harness)
    )
}

/// The shell line that attaches to a host's tmux session: `fleet -H <host> enter <session>`.
pub fn attach_command(host: &str, tmux_session: &str) -> String {
    format!("fleet -H {} enter -- {}", shq(host), shq(tmux_session))
}

/// The host's tmux, not one session: `fleet -H <host> new` attaches to (or creates) `main`.
pub fn host_shell_command(host: &str) -> String {
    format!("fleet -H {} new", shq(host))
}

/// Open a new iTerm tab (a window with `window`, or when none is open) and run `line` in it.
pub fn iterm_run(line: &str, window: bool) -> Result<()> {
    deny_fixture()?;
    // A fresh tab's shell is still sourcing the login profile (which on
    // this stack loads secrets), so a single `write text` — text plus its
    // implicit newline in one chunk — can land before the prompt is ready
    // and never submit. Type without a newline, let the shell settle, then
    // send a bare Enter, the same reliable two-write dance `send` uses.
    osa(
        r#"on run argv
  set theCmd to item 1 of argv
  set makeWindow to (item 2 of argv is "1")
  tell application "iTerm2"
    activate
    if makeWindow or (count of windows) is 0 then
      create window with default profile
    else
      tell current window to create tab with default profile
    end if
    tell (current session of current tab of current window)
      delay 0.5
      write text theCmd without newline
      delay 0.3
      write text ""
    end tell
  end tell
end run"#,
        &[line, if window { "1" } else { "0" }],
    )?;
    Ok(())
}

/// Longest launch line typed into a shell; a longer prompt goes through a file
/// (`web/lib/spawn.mjs` uses the same limit).
const TYPED_LINE_MAX: usize = 900;

/// Store a prompt in `~/.claude/fleet-prompts/` for a spawn to read at launch.
fn write_prompt_file(text: &str) -> Result<String> {
    let dir = crate::core::discovery::claude_home().join("fleet-prompts");
    std::fs::create_dir_all(&dir)?;
    let now = chrono::Local::now();
    let path = dir.join(format!(
        "{}-{}.md",
        now.format("%Y%m%d-%H%M%S"),
        now.timestamp_subsec_nanos() % 1_000_000
    ));
    std::fs::write(&path, text)?;
    Ok(path.display().to_string())
}

/// Spawn a fresh session and return a human description of where it landed. Spawned from
/// inside a job run (`FLEET_JOB_RUN` set), the new session joins that run.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    backend: Backend,
    dir: &str,
    prompt: Prompt,
    name: Option<&str>,
    tmux_session: Option<&str>,
    window: bool,
    launcher: &str,
    harness: Harness,
) -> Result<String> {
    let tag = crate::core::jobs::Tag::from_env();
    spawn_tagged(
        backend,
        dir,
        prompt,
        name,
        tmux_session,
        window,
        launcher,
        harness,
        tag.as_ref(),
    )
    .map(|(desc, _)| desc)
}

/// [`spawn`] with an explicit job tag (`None` = untagged). Returns the description and the
/// tmux session it landed in (`None` for iTerm). A tagged session must land in tmux: the tag
/// lives in the tmux session's environment and the pane's `@fleet_job_run` option, and the
/// run records the session there.
#[allow(clippy::too_many_arguments)]
pub fn spawn_tagged(
    backend: Backend,
    dir: &str,
    prompt: Prompt,
    name: Option<&str>,
    tmux_session: Option<&str>,
    window: bool,
    launcher: &str,
    harness: Harness,
    tag: Option<&crate::core::jobs::Tag>,
) -> Result<(String, Option<String>)> {
    let backend = match (backend, tag) {
        (Backend::Iterm, Some(_)) => Backend::Tmux,
        (b, _) => b,
    };
    // A long prompt typed onto the command line is slow and fragile (the web spawn has the
    // same limit): it goes through a file the shell expands at launch instead.
    let long_file = match prompt {
        Prompt::Inline(p)
            if launch_command(dir, prompt, name, launcher, harness).len() > TYPED_LINE_MAX =>
        {
            Some(write_prompt_file(p)?)
        }
        _ => None,
    };
    let prompt = match &long_file {
        Some(f) => Prompt::File(f),
        None => prompt,
    };
    let line = launch_command(dir, prompt, name, launcher, harness);
    match backend {
        Backend::Iterm => {
            iterm_run(&line, window)?;
            Ok((
                format!("spawned iTerm {}", if window { "window" } else { "tab" }),
                None,
            ))
        }
        Backend::Tmux => {
            let (session, exists) =
                plan_tmux_target(tmux_session, name, dir, &tmux_session_names())?;
            // A fresh session's first window is the one we want: opening another
            // next to it would leave an idle shell behind.
            let mut args = tmux_spawn_args(&session, dir, exists);
            if let Some(t) = tag {
                args.extend(t.env().into_iter().flat_map(|kv| ["-e".to_string(), kv]));
            }
            let out = crate::core::tools::tmux_cmd().args(&args).output()?;
            if !out.status.success() {
                return Err(Error::Other(format!(
                    "tmux: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            let win = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Some(id) = crate::core::gh_identity::in_launcher(launcher) {
                crate::core::gh_identity::tag_pane(&win, id);
            }
            if let Some(t) = tag {
                tag_pane(&win, t, &session, exists, name);
            }
            // Window already opened in `dir`; just run the launcher in it.
            let run = run_command(prompt, name, launcher, harness);
            crate::core::tools::tmux_cmd()
                .args(["send-keys", "-t", &win, "-l", &run])
                .status()?;
            crate::core::tools::tmux_cmd()
                .args(["send-keys", "-t", &win, "Enter"])
                .status()?;
            let desc = if exists {
                format!("spawned tmux window {win}  (attach: tmux attach -t {session})")
            } else {
                format!("spawned tmux session {session}  (attach: tmux attach -t {session})")
            };
            Ok((desc, Some(session)))
        }
        Backend::Unknown => Err(Error::Other("unknown backend".into())),
    }
}

/// Mark the new pane as part of the job run and note it in the run's record. Best effort:
/// the environment already carries the tag, so a failure here only costs the `origin` label.
fn tag_pane(
    win: &str,
    tag: &crate::core::jobs::Tag,
    session: &str,
    exists: bool,
    name: Option<&str>,
) {
    let _ = crate::core::tools::tmux_cmd()
        .args([
            "set-option",
            "-p",
            "-t",
            win,
            crate::core::jobs::PANE_OPTION,
            &tag.run,
        ])
        .status();
    // A window in an existing session is not ours to close later: only a session spawned
    // for the job is recorded.
    if exists {
        return;
    }
    // The spawner's own tmux session, when it is a job session spawning a child.
    let parent = std::env::var("TMUX_PANE")
        .ok()
        .filter(|_| std::env::var_os(crate::core::jobs::RUN_ENV).is_some())
        .and_then(|p| tmux_query(&p, "#{session_name}").ok());
    crate::core::jobs::record_session(
        tag,
        crate::core::jobs::RunSession {
            tmux_session: session.to_string(),
            name: name.map(String::from),
            parent,
            at: chrono::Local::now().timestamp_millis(),
            closed_at: None,
        },
    );
}

/// Where a tmux spawn lands, and whether that session already exists.
///
/// One tmux session per job, the same convention as `fleet tmux new` and the
/// web spawn: the session is named after `name` (or the directory's basename),
/// sanitised like `fleet tmux new`. An explicit name that is already taken is an
/// error — the same answer the web spawn gives (409) — while a name derived from
/// the directory is made unique (`app`, `app-2`, …), since the caller never
/// chose it. `--tmux-session <s>` opts out: a new window in `s` (created if
/// missing). Pure — `existing` is the live session list, compared exactly.
pub fn plan_tmux_target(
    tmux_session: Option<&str>,
    name: Option<&str>,
    dir: &str,
    existing: &[String],
) -> Result<(String, bool)> {
    if let Some(s) = tmux_session {
        return Ok((s.to_string(), existing.iter().any(|e| e == s)));
    }
    if let Some(n) = name {
        let n = crate::core::tmux::sanitize_name(n);
        if n.is_empty() {
            return Err(Error::Other(
                "--name is empty after sanitising for tmux".into(),
            ));
        }
        if existing.contains(&n) {
            return Err(Error::Other(format!(
                "tmux session `{n}` already exists — pick another --name, or pass \
                 --tmux-session {n} to open a window in it"
            )));
        }
        return Ok((n, false));
    }
    let base = std::path::Path::new(dir.trim_end_matches('/'))
        .file_name()
        .map(|b| crate::core::tmux::sanitize_name(&b.to_string_lossy()))
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "claude".into());
    Ok((unique_tmux_name(&base, existing), false))
}

/// Environment for a spawn's shell: oh-my-zsh's "Would you like to update? [Y/n]" reads one
/// key, eating the first character of the launch line typed into it (`laude: command not found`).
pub const SPAWN_SHELL_ENV: &str = "DISABLE_AUTO_UPDATE=true";

/// The tmux command that opens the spawn's window in `dir`, printing its target:
/// a new window in an existing `session` (matched exactly), or the session itself.
fn tmux_spawn_args(session: &str, dir: &str, exists: bool) -> Vec<String> {
    let mut a: Vec<String> = if exists {
        vec!["new-window".into(), "-t".into(), format!("={session}:")]
    } else {
        vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            session.into(),
        ]
    };
    a.extend(
        [
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
            "-c",
            dir,
            "-e",
            SPAWN_SHELL_ENV,
        ]
        .map(String::from),
    );
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_command_quotes_both_words() {
        assert_eq!(
            attach_command("workstation", "api"),
            "fleet -H 'workstation' enter -- 'api'"
        );
        assert_eq!(
            attach_command("laptop", "it's"),
            r"fleet -H 'laptop' enter -- 'it'\''s'"
        );
    }

    #[test]
    fn host_shell_command_quotes_the_host() {
        assert_eq!(
            host_shell_command("workstation"),
            "fleet -H 'workstation' new"
        );
    }

    #[test]
    fn tmux_spawn_targets_the_session_exactly() {
        assert_eq!(
            tmux_spawn_args("fleet", "/tmp", true)[..3],
            ["new-window", "-t", "=fleet:"]
        );
        let fresh = tmux_spawn_args("fleet", "/tmp", false);
        assert_eq!(fresh[..4], ["new-session", "-d", "-s", "fleet"]);
        assert!(fresh.ends_with(&[
            "-c".to_string(),
            "/tmp".to_string(),
            "-e".to_string(),
            SPAWN_SHELL_ENV.to_string()
        ]));
        assert!(fresh.contains(&"-P".to_string()));
    }

    #[test]
    fn spawn_gets_its_own_tmux_session() {
        let live = vec!["app".to_string(), "api".to_string(), "fleet".to_string()];
        let plan = |t, n, d| plan_tmux_target(t, n, d, &live);
        // Derived from the dir basename, uniquified.
        assert_eq!(plan(None, None, "/x/app").unwrap(), ("app-2".into(), false));
        assert_eq!(
            plan(None, None, "/x/my.repo/").unwrap(),
            ("my-repo".into(), false)
        );
        assert_eq!(plan(None, None, "/").unwrap(), ("claude".into(), false));
        // Explicit name: sanitised; taken is an error pointing at --tmux-session.
        assert_eq!(
            plan(None, Some("new job"), "/x/app").unwrap(),
            ("new-job".into(), false)
        );
        let err = plan(None, Some("api"), "/x").unwrap_err().to_string();
        assert!(err.contains("already exists") && err.contains("--tmux-session api"));
        assert!(plan(None, Some("::"), "/x").is_err());
        // --tmux-session targets an existing session (or creates it).
        assert_eq!(
            plan(Some("fleet"), Some("api"), "/x").unwrap(),
            ("fleet".into(), true)
        );
        assert_eq!(
            plan(Some("nope"), None, "/x").unwrap(),
            ("nope".into(), false)
        );
    }

    #[test]
    fn quoting_and_command() {
        assert_eq!(shq("a b"), "'a b'");
        assert_eq!(shq("it's"), r"'it'\''s'");
        assert_eq!(
            launch_command("/tmp", Prompt::None, None, "claude", Harness::Claude),
            "cd '/tmp' && claude"
        );
        assert_eq!(
            launch_command("/tmp", Prompt::Inline(""), None, "claude", Harness::Claude),
            "cd '/tmp' && claude"
        );
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::Inline("hi there"),
                None,
                "claude",
                Harness::Claude
            ),
            "cd '/tmp' && claude 'hi there'"
        );
        // A custom launcher (e.g. the user's `cc` wrapper) replaces `claude`.
        assert_eq!(
            launch_command("/tmp", Prompt::Inline("hi"), None, "cc", Harness::Claude),
            "cd '/tmp' && cc 'hi'"
        );
    }

    #[test]
    fn multi_line_prompt_is_typed_as_one_ansi_c_line() {
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::Inline("a\nit's \\ b"),
                None,
                "claude",
                Harness::Claude
            ),
            "cd '/tmp' && claude $'a\\nit\\'s \\\\ b'"
        );
    }

    #[test]
    fn session_name_goes_to_the_launcher_before_the_prompt() {
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::Inline("go"),
                Some("auth fix"),
                "claude",
                Harness::Claude
            ),
            "cd '/tmp' && claude -n 'auth fix' 'go'"
        );
        // Flags the user already configured stay ahead of ours.
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::None,
                Some("probe"),
                "claude --resume",
                Harness::Claude
            ),
            "cd '/tmp' && claude --resume -n 'probe'"
        );
    }

    // --- tmux session naming -------------------------------------------------

    // tmux rewrites `:` and `.` to `_` behind your back, so a collision check
    // against the pre-rewrite string compares a name that never lands.
    #[test]
    fn tmux_names_lose_the_characters_tmux_would_rewrite() {
        assert_eq!(sanitize_tmux_name("foo:bar.baz"), "foo-bar-baz");
        assert_eq!(sanitize_tmux_name("cache warmup"), "cache-warmup");
        assert_eq!(sanitize_tmux_name("a: . b"), "a-b");
        assert_eq!(sanitize_tmux_name("cache-warmup"), "cache-warmup");
        // A leading `-` reads as a flag to `tmux rename-session`.
        assert_eq!(sanitize_tmux_name(" leading"), "leading");
        // A typed name of any length still has to fit a status line.
        let long = sanitize_tmux_name(&"a".repeat(500));
        assert_eq!(long.chars().count(), MAX_TMUX_NAME);
    }

    // Both guards used to answer "go ahead" when tmux itself failed to answer —
    // `(1, 1)` and `None` are indistinguishable from "one job, not mine".
    #[test]
    fn the_guards_fail_closed_when_tmux_does_not_answer() {
        // A pane id that cannot exist; with tmux absent the command fails too.
        assert!(tmux_query("%999999", "#{session_name}").is_err());
        assert!(tmux_session_of("%999999").is_err());
        assert!(tmux_shape("%999999").is_err());
    }

    // …and end to end: an unresolvable pane is skipped, never renamed on a
    // stale name carried over from the last poll.
    #[test]
    fn an_unresolvable_pane_is_skipped_rather_than_renamed_blind() {
        let s = Session {
            backend: Backend::Tmux,
            handle: Some("%999999".into()),
            // Deliberately stale and deliberately dangerous: the snapshot says
            // one thing, the live pane would say another.
            tmux_session: Some("some-old-name".into()),
            ..Default::default()
        };
        let why = match rename_tmux_session(&s, "cache-warmup").unwrap() {
            TmuxSync::Skipped(why) => why,
            TmuxSync::Renamed { to, .. } => panic!("renamed blind: {to}"),
        };
        assert!(why.contains("tmux session unknown"), "{why}");

        // No pane id at all is the same answer — discovery fills `handle` and
        // `tmux_session` from one lookup, so there is no third case.
        let s = Session {
            backend: Backend::Tmux,
            tmux_session: Some("some-old-name".into()),
            ..Default::default()
        };
        assert!(matches!(
            rename_tmux_session(&s, "x").unwrap(),
            TmuxSync::Skipped(_)
        ));
    }

    // `.status()?` propagated only spawn failure, so "can't find pane" was
    // reported as a `/rename` that landed — and the tmux session got renamed to
    // match a name the session never received.
    #[test]
    fn send_keys_reports_what_tmux_says_went_wrong() {
        let e = send_keys("%999999", &["-l", "hi"]).unwrap_err();
        assert!(e.to_string().contains("send-keys"), "{e}");
    }

    #[test]
    fn long_text_is_typed_in_pieces_that_keep_characters_whole() {
        assert!(type_chunks("", 4).is_empty());
        assert_eq!(type_chunks("abcd", 4), ["abcd"]);
        assert_eq!(type_chunks("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        // "é" is two bytes: a cut at byte 4 would split it, so it moves before it.
        assert_eq!(type_chunks("abcé-fg", 4), ["abc", "é-f", "g"]);
        let long = "tok-0123456789 ".repeat(150);
        let pieces = type_chunks(&long, TYPE_CHUNK);
        assert!(pieces.iter().all(|p| p.len() <= TYPE_CHUNK));
        assert_eq!(pieces.concat(), long);
    }

    #[test]
    fn colliding_names_get_a_numeric_suffix() {
        let existing: Vec<String> = ["app", "cache-warmup", "cache-warmup-2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(unique_tmux_name("docs-refresh", &existing), "docs-refresh");
        assert_eq!(
            unique_tmux_name("cache-warmup", &existing),
            "cache-warmup-3"
        );
        assert_eq!(unique_tmux_name("app", &existing), "app-2");
        // Nothing taken at all is a straight pass-through.
        assert_eq!(unique_tmux_name("app", &[]), "app");
    }

    #[test]
    fn the_guards_refuse_the_sessions_that_must_not_be_renamed() {
        // `bind f switch-client -t fleet` depends on this name.
        assert!(tmux_guard("fleet", None, 1, 1).is_some());
        // The dashboard's own session — renaming it out from under the TUI.
        assert!(tmux_guard("app", Some("app"), 1, 1).is_some());
        // More than one window or pane: one-session-one-job doesn't hold here.
        assert!(tmux_guard("app", None, 3, 1).is_some());
        assert!(tmux_guard("app", None, 1, 4).is_some());
        // The ordinary case goes ahead.
        assert!(tmux_guard("app", Some("other"), 1, 1).is_none());
    }

    #[test]
    fn non_tmux_sessions_are_a_friendly_no_op() {
        let s = Session {
            backend: Backend::Iterm,
            tab: Some("app".into()),
            ..Default::default()
        };
        assert!(matches!(
            rename_tmux_session(&s, "x").unwrap(),
            TmuxSync::Skipped(_)
        ));
        let s = Session {
            backend: Backend::Unknown,
            ..Default::default()
        };
        assert!(matches!(
            rename_tmux_session(&s, "x").unwrap(),
            TmuxSync::Skipped(_)
        ));
    }

    #[test]
    fn codex_takes_no_name_flag_pi_does() {
        assert_eq!(
            launch_command(
                "/w",
                Prompt::Inline("go"),
                Some("x"),
                "codex --model 'gpt-6'",
                Harness::Codex
            ),
            "cd '/w' && codex --model 'gpt-6' 'go'"
        );
        assert_eq!(
            launch_command("/w", Prompt::Inline("go"), Some("x"), "pi", Harness::Pi),
            "cd '/w' && pi -n 'x' 'go'"
        );
    }

    #[test]
    fn prompt_file_expands_in_the_spawned_shell() {
        // The brief's text must never reach the command line — only its path does.
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::File("/tmp/a b.md"),
                None,
                "claude",
                Harness::Claude
            ),
            r#"cd '/tmp' && claude "$(cat '/tmp/a b.md')""#
        );
    }
}
