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

/// Run a static AppleScript, passing dynamic values as argv (injection-safe). Every iTerm
/// action goes through here, so this is where `FLEET_PATH_ONLY` refuses iTerm.
fn osa(script: &str, args: &[&str]) -> Result<String> {
    crate::core::tools::deny_path_only("the iTerm backend")?;
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

/// What a tmux pane shows now (no scrollback), or `None` when tmux would not say — within
/// [`SCREEN_DEADLINE`], so a hung tmux cannot stall the discovery that asks.
pub fn tmux_screen(pane: &str) -> Option<String> {
    let mut cmd = crate::core::tools::tmux_cmd();
    cmd.args(["capture-pane", "-p", "-t", pane]);
    let out = crate::core::hosts::capture(cmd, SCREEN_DEADLINE).ok()?;
    out.ok().then_some(out.stdout)
}

/// How long one `capture-pane` for discovery may take.
pub const SCREEN_DEADLINE: std::time::Duration = std::time::Duration::from_millis(500);

pub fn peek(s: &Session) -> Result<String> {
    peek_history(s, 0)
}

/// [`peek`] plus up to `history` lines of tmux scrollback above the screen (what scrolled off
/// the top: the head of a tall prompt). iTerm gives its session's text as it has it.
pub fn peek_history(s: &Session, history: usize) -> Result<String> {
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
            let visible = tmux_capture(handle, &[])?;
            if history == 0 {
                return Ok(visible);
            }
            // The scrollback above the screen, then a mark where the screen starts: what is
            // on screen now is told apart from what scrolled (or was cleared) off it.
            let above = tmux_capture(handle, &["-S", &format!("-{history}"), "-E", "-1"])?;
            Ok(format!(
                "{above}{}\n{visible}",
                crate::core::prompt::VISIBLE_MARK
            ))
        }
        Backend::Unknown => require_handle(s).map(|_| String::new()),
    }
}

/// [`peek`] with the text's attributes, when the backend has them: `(screen, styled)`. tmux
/// keeps them as escape sequences (`capture-pane -e`, `true`); iTerm's text has none
/// (`false`). `send` reads its input box this way: faint text there is a placeholder.
pub fn peek_styled(s: &Session) -> Result<(String, bool)> {
    deny_fixture()?;
    let handle = require_handle(s)?;
    match s.backend {
        Backend::Tmux => Ok((tmux_capture(handle, &["-e"])?, true)),
        _ => peek(s).map(|text| (text, false)),
    }
}

/// One `capture-pane -p` of `handle`, with `args`. A capture tmux says failed (the pane or the
/// server gone) is an error, never an empty screen: `answer` would read that as its prompt
/// having gone, i.e. answered — and `send` as an empty input box.
fn tmux_capture(handle: &str, args: &[&str]) -> Result<String> {
    let out = crate::core::tools::tmux_cmd()
        .args(["capture-pane", "-t", handle, "-p"])
        .args(args)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    tmux_ok(out, &format!("tmux capture-pane -t {handle}"))?;
    Ok(text)
}

/// What a session's terminal shows, whole: no header, no clipping to our own width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The terminal's size, when the backend says (tmux pane, iTerm session).
    pub cols: Option<u32>,
    pub rows: Option<u32>,
    /// The last lines with content, oldest first; trailing blank lines dropped.
    pub lines: Vec<String>,
    /// The oldest line may have lost its start: the capture stopped at
    /// [`MAX_CAPTURE_ROWS`] before reaching it (tmux only).
    pub truncated: bool,
}

/// How far back into a tmux scrollback `peek --json` reaches to make its lines whole: a
/// line spanning more rows than this (a minified file `cat` in a shell pane) comes back
/// with its start cut, and `truncated` set, rather than every poll capturing the scrollback.
pub const MAX_CAPTURE_ROWS: usize = 2000;

/// The last `lines` lines of a session's terminal, unclipped. tmux joins the lines it
/// wrapped (`capture-pane -J`) and reaches into the scrollback for them — see
/// [`tmux_lines`]; iTerm gives its screen as shown. Escape sequences are stripped unless
/// `ansi` (tmux `-e`; iTerm has none to give).
pub fn screen(s: &Session, lines: usize, ansi: bool) -> Result<Screen> {
    deny_fixture()?;
    let handle = require_handle(s).map_err(|e| e.tag("backend_unsupported"))?;
    let (cols, rows, (lines, truncated)) = match s.backend {
        Backend::Iterm => {
            let out = osa(
                &format!(
                    r#"{ITERM_FIND}
on run argv
  set s to findSession(item 1 of argv)
  if s is missing value then error "session not found"
  tell application "iTerm2" to return ((columns of s) as text) & " " & ((rows of s) as text) & linefeed & (text of s)
end run"#
                ),
                &[handle],
            )
            .map_err(|e| e.tag("backend_failed"))?;
            let (size, text) = out.split_once('\n').unwrap_or((out.as_str(), ""));
            let (c, r) = parse_size(size);
            (c, r, (screen_lines(text, lines, ansi), false))
        }
        Backend::Tmux => {
            let size = tmux_query(handle, "#{pane_width} #{pane_height} #{history_size}")
                .map_err(|e| Error::tagged("backend_failed", format!("tmux: {e}")))?;
            let (c, r) = parse_size(&size);
            let history = size
                .split_whitespace()
                .nth(2)
                .and_then(|h| h.parse().ok())
                .unwrap_or(usize::MAX);
            (c, r, tmux_lines(handle, lines, history, ansi)?)
        }
        Backend::Unknown => {
            return Err(Error::tagged(
                "backend_unsupported",
                format!("session {} has no controllable backend", s.label()),
            ));
        }
    };
    Ok(Screen {
        cols,
        rows,
        lines,
        truncated,
    })
}

/// The last `n` *logical* lines of a tmux pane, each whole.
///
/// `capture-pane -S -<k>` picks physical rows and only then joins the wrapped ones (`-J`),
/// so a long line whose start lies above row `-k` would come back with its start missing.
/// So the capture widens — `k` doubling from `n` — until it holds more than `n` logical
/// lines (the first may be cut; the `n` after it cannot be) or reaches the top of the
/// scrollback (`history` rows), where nothing is cut but what tmux itself dropped — or
/// [`MAX_CAPTURE_ROWS`], where it stops and says the oldest line may be cut (`true`).
fn tmux_lines(handle: &str, n: usize, history: usize, ansi: bool) -> Result<(Vec<String>, bool)> {
    let mut back = n.clamp(1, MAX_CAPTURE_ROWS);
    loop {
        let whole = back >= history;
        let capped = !whole && back >= MAX_CAPTURE_ROWS;
        let start = if whole {
            "-".to_string()
        } else {
            format!("-{back}")
        };
        let mut cmd = crate::core::tools::tmux_cmd();
        cmd.args(["capture-pane", "-p", "-J", "-t", handle, "-S", &start]);
        if ansi {
            cmd.arg("-e");
        }
        let out = cmd.output()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        tmux_ok(out, &format!("tmux capture-pane -t {handle}"))
            .map_err(|e| e.tag("backend_failed"))?;
        let mut got = screen_lines(&text, n.saturating_add(1), ansi);
        if whole || got.len() > n || capped {
            let truncated = capped && got.len() <= n;
            let from = got.len().saturating_sub(n);
            return Ok((got.split_off(from), truncated));
        }
        back = back.saturating_mul(2).min(MAX_CAPTURE_ROWS);
    }
}

/// `"<cols> <rows>"` → both, each `None` when unreadable.
fn parse_size(s: &str) -> (Option<u32>, Option<u32>) {
    let mut it = s.split_whitespace().map(|n| n.parse().ok());
    (it.next().flatten(), it.next().flatten())
}

/// Captured text → its last `n` lines: escapes stripped unless `ansi`, trailing spaces
/// and trailing blank lines (blank once escapes are ignored) dropped. Nothing is clipped.
fn screen_lines(text: &str, n: usize, ansi: bool) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let l = l.trim_end_matches('\r');
            let l = if ansi { l.to_string() } else { strip_ansi(l) };
            l.trim_end_matches(' ').to_string()
        })
        .collect();
    while out.last().is_some_and(|l| strip_ansi(l).trim().is_empty()) {
        out.pop();
    }
    let from = out.len().saturating_sub(n);
    out.split_off(from)
}

/// Drop terminal escape sequences (CSI `ESC [ … final`, OSC `ESC ] … BEL|ESC \`,
/// `ESC ( B`-style designations, two-byte `ESC x`) and other control characters but tab.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.next() {
                Some('[') => {
                    for c in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' || (c == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // `ESC ( B` and kin: intermediates, then one final character.
                Some(c) if ('\x20'..='\x2f').contains(&c) => {
                    for c in it.by_ref() {
                        if !('\x20'..='\x2f').contains(&c) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
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

/// Type `text` into the session and submit it. Its failures carry `send --json`'s kinds: no
/// pane or iTerm session to drive is `backend_unsupported`, a tmux or iTerm call that fails is
/// `backend_failed`.
pub fn send(s: &Session, text: &str) -> Result<()> {
    deny_fixture()?;
    let handle = require_handle(s).map_err(|e| e.tag("backend_unsupported"))?;
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
            )
            .map_err(|e| e.tag("backend_failed"))?;
            Ok(())
        }
        Backend::Tmux => {
            // A non-zero `send-keys` ("can't find pane") is the pane having gone
            // away, not a spawn failure — `?` alone would swallow it and report
            // a `/rename` that never landed as success, after which the tmux
            // session gets renamed anyway.
            let typed = || -> Result<()> {
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
                send_keys(handle, &["Enter"])
            };
            typed().map_err(|e| e.tag("backend_failed"))
        }
        Backend::Unknown => Err(Error::tagged(
            "backend_unsupported",
            format!("session {} has no controllable backend", s.label()),
        )),
    }
}

/// A named key `fleet keys` may send — an allowlist, so nothing beyond what a person
/// steering a TUI presses (no `C-d`, no `C-z`, no arbitrary escape sequences).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Up,
    Down,
    Left,
    Right,
    Tab,
    /// Shift-Tab.
    BTab,
    Space,
    /// Ctrl-C.
    CtrlC,
}

impl Key {
    pub const ALL: [Key; 10] = [
        Key::Escape,
        Key::Enter,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Tab,
        Key::BTab,
        Key::Space,
        Key::CtrlC,
    ];

    /// The name `fleet keys` takes and reports — also the `tmux send-keys` key name.
    pub fn name(self) -> &'static str {
        match self {
            Key::Escape => "Escape",
            Key::Enter => "Enter",
            Key::Up => "Up",
            Key::Down => "Down",
            Key::Left => "Left",
            Key::Right => "Right",
            Key::Tab => "Tab",
            Key::BTab => "BTab",
            Key::Space => "Space",
            Key::CtrlC => "C-c",
        }
    }

    /// Exactly one of [`Key::ALL`]'s names (case-sensitive, as tmux's are).
    pub fn parse(name: &str) -> Option<Key> {
        Key::ALL.into_iter().find(|k| k.name() == name)
    }

    /// What a terminal sends for the key in normal (not application) cursor mode — what
    /// iTerm is given to type, since it has no key names.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Key::Escape => b"\x1b",
            Key::Enter => b"\r",
            Key::Up => b"\x1b[A",
            Key::Down => b"\x1b[B",
            Key::Right => b"\x1b[C",
            Key::Left => b"\x1b[D",
            Key::Tab => b"\t",
            Key::BTab => b"\x1b[Z",
            Key::Space => b" ",
            Key::CtrlC => b"\x03",
        }
    }
}

/// Gap between two keys. One burst would let a TUI read `Escape` + `Up` as Alt-Up; the web
/// server's answer keys use the same beat.
const KEY_GAP: std::time::Duration = std::time::Duration::from_millis(80);

/// Press `keys` in the session's terminal, one at a time, [`KEY_GAP`] apart. Not atomic:
/// on a failure the keys before it stay pressed and the rest are not sent (iTerm types them
/// in one AppleScript run, which stops the same way at the key that fails).
pub fn keys(s: &Session, keys: &[Key]) -> Result<()> {
    deny_fixture()?;
    let handle = require_handle(s).map_err(|e| e.tag("backend_unsupported"))?;
    match s.backend {
        Backend::Iterm => {
            // Each key as its byte values ("27,91,65"), typed with `character id` so no
            // control character has to survive argv.
            let codes: Vec<String> = keys
                .iter()
                .map(|k| {
                    k.bytes()
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect();
            let mut args = vec![handle];
            args.extend(codes.iter().map(String::as_str));
            osa(
                &format!(
                    r#"{ITERM_FIND}
on run argv
  set s to findSession(item 1 of argv)
  if s is missing value then error "session not found — 0 of " & ((count of argv) - 1) & " keys pressed before it"
  set AppleScript's text item delimiters to ","
  set total to (count of argv) - 1
  repeat with i from 2 to count of argv
    set seq to ""
    repeat with c in text items of (item i of argv)
      set seq to seq & (character id (c as integer))
    end repeat
    try
      tell application "iTerm2" to tell s to write text seq without newline
    on error errMsg
      error errMsg & " — " & (i - 2) & " of " & total & " keys pressed before it"
    end try
    if i < (count of argv) then delay {gap}
  end repeat
end run"#,
                    gap = KEY_GAP.as_secs_f32()
                ),
                &args,
            )
            .map_err(|e| e.tag("backend_failed"))?;
            Ok(())
        }
        Backend::Tmux => {
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    std::thread::sleep(KEY_GAP);
                }
                // Stop at the first failure: the keys before it are already pressed (nothing can
                // take them back), the ones after it are not.
                send_keys(handle, &[k.name()]).map_err(|e| {
                    Error::tagged(
                        "backend_failed",
                        format!("{e} — {i} of {} keys pressed before it", keys.len()),
                    )
                })?;
            }
            Ok(())
        }
        Backend::Unknown => Err(Error::tagged(
            "backend_unsupported",
            format!("session {} has no controllable backend", s.label()),
        )),
    }
}

const ITERM_WRITE: &str = r#"on run argv
  set s to findSession(item 1 of argv)
  if s is missing value then error "session not found"
  tell application "iTerm2" to tell s to write text (item 2 of argv) without newline
end run"#;

/// Press one key in the session, nothing else: a named [`Key`] (`Enter`, `Down`, `Escape`, …)
/// through [`keys`], or a single letter or digit (a prompt's shortcut), typed as itself.
pub fn press(s: &Session, key: &str) -> Result<()> {
    match Key::parse(key) {
        Some(k) => keys(s, &[k]),
        None if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()) => {
            type_literal(s, key)
        }
        None => Err(Error::Other(format!("no such key: {key}"))),
    }
}

/// Type one line of text into the session literally, without submitting it.
pub fn type_literal(s: &Session, text: &str) -> Result<()> {
    deny_fixture()?;
    let handle = require_handle(s)?;
    match s.backend {
        Backend::Tmux => type_text(handle, text),
        Backend::Iterm => osa(&format!("{ITERM_FIND}\n{ITERM_WRITE}"), &[handle, text]).map(|_| ()),
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

/// pi's commands, which it runs when one is its first argument (`pi update`).
const PI_COMMANDS: [&str; 6] = ["install", "remove", "uninstall", "update", "list", "config"];

/// pi has no `--`: to it an argument that starts with `-` is a flag, `@…` a file to attach,
/// and a bare `update` or `install` its own command. A leading space keeps any of those the
/// first message; the model never minds it.
fn pi_message(p: &str) -> std::borrow::Cow<'_, str> {
    if p.starts_with(['-', '@']) || PI_COMMANDS.contains(&p) {
        format!(" {p}").into()
    } else {
        p.into()
    }
}

fn run_command(prompt: Prompt, name: Option<&str>, launcher: &str, harness: Harness) -> String {
    let bare = with_name(launcher, name, harness);
    // `--` ends the options, so the prompt is never read as a flag or a subcommand:
    // `codex '--help'` printed help, `codex 'resume'` opened the resume picker.
    let launcher = if harness.ends_options_at_double_dash() {
        format!("{bare} --")
    } else {
        bare.clone()
    };
    match (prompt, harness) {
        (Prompt::None | Prompt::Inline(""), _) => bare,
        (Prompt::Inline(p), Harness::Pi) => format!("{launcher} {}", shq_typed(&pi_message(p))),
        (Prompt::Inline(p), _) => format!("{launcher} {}", shq_typed(p)),
        // `$(cat f)` would drop the file's trailing newlines; a `.` after them keeps them, and
        // `${…%.}` takes it off again. The file's first line is unknown here, so pi always gets
        // the leading space.
        (Prompt::File(path), Harness::Pi) => format!(
            "{PROMPT_VAR}=\"$(cat {}; printf .)\" && {launcher} \" ${{{PROMPT_VAR}%.}}\"",
            shq(path)
        ),
        (Prompt::File(path), _) => format!(
            "{PROMPT_VAR}=\"$(cat {}; printf .)\" && {launcher} \"${{{PROMPT_VAR}%.}}\"",
            shq(path)
        ),
    }
}

/// The shell variable a prompt file is read into (see [`run_command`]).
const PROMPT_VAR: &str = "fleet_prompt";

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

/// Open a new iTerm tab (a window with `window`, or when none is open) and run `line` in it.
/// Returns the new session's id (the handle `list` reports for it), when iTerm gave one.
pub fn iterm_run(line: &str, window: bool) -> Result<Option<String>> {
    deny_fixture()?;
    // A fresh tab's shell is still sourcing the login profile (which on
    // this stack loads secrets), so a single `write text` — text plus its
    // implicit newline in one chunk — can land before the prompt is ready
    // and never submit. Type without a newline, let the shell settle, then
    // send a bare Enter, the same reliable two-write dance `send` uses.
    let out = osa(
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
    -- Held from here on: a click elsewhere during the delays must not redirect the typing.
    set s to current session of current tab of current window
    try
      tell s
        delay 0.5
        write text theCmd without newline
        delay 0.3
        write text ""
      end tell
    on error msg
      -- The tab exists: say which one, so the caller can find and close it.
      return (id of s) & tab & "error" & tab & msg
    end try
    return id of s
  end tell
end run"#,
        &[line, if window { "1" } else { "0" }],
    )
    .map_err(|e| e.tag("backend_failed"))?;
    match parse_iterm_result(&out) {
        (id, None) => Ok(id),
        (id, Some(why)) => Err(Error::tagged(
            "backend_failed",
            format!("iTerm: typing into the new tab failed: {why}"),
        )
        .with_partial_handle(id)),
    }
}

/// The AppleScript's answer: `<session id>`, or `<id>\terror\t<message>` when the tab
/// opened but typing into it failed.
fn parse_iterm_result(out: &str) -> (Option<String>, Option<String>) {
    let out = out.trim_end_matches(['\n', '\r']);
    let mut parts = out.splitn(3, '\t');
    let id = parts
        .next()
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .map(String::from);
    match (parts.next(), parts.next()) {
        (Some("error"), why) => (id, Some(why.unwrap_or("").trim().to_string())),
        _ => (id, None),
    }
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

/// Where a spawn landed. `Display` is the one-line human description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawned {
    pub backend: Backend,
    pub desc: String,
    /// The tmux session it runs in (`None` for iTerm).
    pub tmux_session: Option<String>,
    /// Its tmux window, `session:index`.
    pub tmux_window: Option<String>,
    /// The control handle `list` reports for it: tmux pane id (`%12`) or iTerm session id.
    pub handle: Option<String>,
    /// tmux: a new session was created for it (false: a window in an existing one).
    pub created_session: bool,
    /// The shell line typed into the new terminal.
    pub command: String,
    /// The file the first prompt was read from, when it went through one.
    pub prompt_file: Option<String>,
}

impl std::fmt::Display for Spawned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.desc)
    }
}

/// Spawn a fresh session and say where it landed. Spawned from inside a job run
/// (`FLEET_JOB_RUN` set), the new session joins that run.
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
) -> Result<Spawned> {
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
}

/// The backend a spawn really uses. A session tagged for a job run must land in tmux: the tag
/// lives in the tmux session's environment and the pane's `@fleet_job_run` option, and the
/// run records the session there.
pub fn effective_backend(backend: Backend, tagged: bool) -> Backend {
    match (backend, tagged) {
        (Backend::Iterm, true) => Backend::Tmux,
        (b, _) => b,
    }
}

/// [`effective_backend`] for a spawn from this process (tagged when `FLEET_JOB_RUN` is set):
/// what [`spawn`] will use, for a dry run to report.
pub fn spawn_backend(backend: Backend) -> Backend {
    effective_backend(backend, crate::core::jobs::Tag::from_env().is_some())
}

/// [`spawn`] with an explicit job tag (`None` = untagged); see [`effective_backend`].
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
) -> Result<Spawned> {
    spawn_hooked(
        backend,
        dir,
        prompt,
        name,
        tmux_session,
        window,
        launcher,
        harness,
        tag,
        &mut |_| Ok(()),
    )
}

/// [`spawn_tagged`] that calls `before_typing` once a tmux terminal exists and before anything
/// is typed into it (iTerm creates the tab and runs the line in one step: never called). Its
/// failure types nothing and is the spawn's, with the terminal as the `partial`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_hooked(
    backend: Backend,
    dir: &str,
    prompt: Prompt,
    name: Option<&str>,
    tmux_session: Option<&str>,
    window: bool,
    launcher: &str,
    harness: Harness,
    tag: Option<&crate::core::jobs::Tag>,
    before_typing: &mut dyn FnMut(&Spawned) -> Result<()>,
) -> Result<Spawned> {
    let backend = effective_backend(backend, tag.is_some());
    // Before a prompt file is written for it.
    refuse_path_only(backend)?;
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
    let prompt_file = match prompt {
        Prompt::File(f) => Some(f.to_string()),
        _ => None,
    };
    match backend {
        Backend::Iterm => {
            let spawned = |handle| Spawned {
                backend,
                desc: format!("spawned iTerm {}", if window { "window" } else { "tab" }),
                tmux_session: None,
                tmux_window: None,
                handle,
                created_session: false,
                command: line.clone(),
                prompt_file: prompt_file.clone(),
            };
            match iterm_run(&line, window) {
                Ok(handle) => Ok(spawned(handle)),
                Err(Error::Partial { source, partial }) => Err(Error::Partial {
                    source,
                    partial: Box::new(spawned(partial.handle)),
                }),
                Err(e) => Err(e),
            }
        }
        Backend::Tmux => {
            let (session, exists) =
                plan_tmux_target(tmux_session, name, dir, &tmux_session_names())?;
            // A fresh session's first window is the one we want: opening another
            // next to it would leave an idle shell behind.
            let mut env = crate::core::tools::pane_env();
            env.extend(tag.map(|t| t.env()).unwrap_or_default());
            let sh = crate::core::tools::pane_sh()?;
            let args = tmux_spawn_args(&session, dir, exists, &env, &sh);
            let mut cmd = crate::core::tools::tmux_cmd();
            // When this starts the tmux server, its global environment is ours: keep the
            // agent's variables out of it, or every later pane on the server inherits them.
            for (k, _) in std::env::vars_os() {
                if k.to_str().is_some_and(is_agent_env) {
                    cmd.env_remove(k);
                }
            }
            let out = cmd
                .args(&args)
                .output()
                .map_err(|e| Error::tagged("backend_failed", format!("cannot run tmux: {e}")))?;
            if !out.status.success() {
                return Err(Error::tagged(
                    "backend_failed",
                    format!("tmux: {}", String::from_utf8_lossy(&out.stderr).trim()),
                ));
            }
            let (pane, win) = parse_spawn_target(&String::from_utf8_lossy(&out.stdout));
            if let Some(t) = tag {
                tag_pane(&pane, t, &session, exists, name);
            }
            let desc = if exists {
                format!("spawned tmux window {win}  (attach: tmux attach -t {session})")
            } else {
                format!("spawned tmux session {session}  (attach: tmux attach -t {session})")
            };
            let spawned = Spawned {
                backend,
                desc,
                tmux_session: Some(session),
                tmux_window: Some(win),
                handle: Some(pane.clone()),
                created_session: !exists,
                command: line,
                prompt_file,
            };
            // Window already opened in `dir`; just run the launcher in it. From here on the
            // terminal exists: a failure says where it is, so the caller can clean it up.
            if let Err(e) = before_typing(&spawned) {
                return Err(Error::Partial {
                    source: Box::new(e),
                    partial: Box::new(spawned),
                });
            }
            let run = run_command(prompt, name, launcher, harness);
            let typed =
                send_keys(&pane, &["-l", "--", &run]).and_then(|()| send_keys(&pane, &["Enter"]));
            match typed {
                Ok(()) => Ok(spawned),
                Err(e) => Err(Error::Partial {
                    source: Box::new(e.tag("backend_failed")),
                    partial: Box::new(spawned),
                }),
            }
        }
        Backend::Unknown => Err(Error::Other("unknown backend".into())),
    }
}

/// What [`tmux_spawn_args`] prints: `<pane id> <session>:<window index>`. The pane id leads
/// because a session name (`--tmux-session`) can hold a space; it never does.
fn parse_spawn_target(out: &str) -> (String, String) {
    let out = out.trim();
    match out.split_once(' ') {
        Some((pane, win)) => (pane.to_string(), win.to_string()),
        None => (out.to_string(), out.to_string()),
    }
}

/// Mark the new pane as part of the job run and note it in the run's record. Best effort:
/// the environment already carries the tag, so a failure here only costs the `origin` label.
fn tag_pane(
    pane: &str,
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
            pane,
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
            return Err(Error::tagged(
                "name_invalid",
                "--name is empty after sanitising for tmux",
            ));
        }
        if existing.contains(&n) {
            return Err(Error::tagged(
                "name_taken",
                format!(
                    "tmux session `{n}` already exists — pick another --name, or pass \
                     --tmux-session {n} to open a window in it"
                ),
            ));
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

/// What the agent that ran `fleet spawn` puts in its tools' environment to describe itself
/// (found by diffing an agent's tool environment against a fresh login shell's, and Codex's
/// sandboxed one). A session spawned with them would act as part of the spawner: a nested
/// `claude` takes its session id, messaging socket and effort; a `codex` thinks it runs
/// sandboxed inside another one's thread; `TRACEPARENT` joins the parent's trace.
///
/// Only these exact names: configuration a user sets on purpose — `CLAUDE_CODE_OAUTH_TOKEN`,
/// `CLAUDE_CODE_USE_BEDROCK` / `_VERTEX`, `OTEL_*` exporter endpoints and headers, … — is left
/// alone (Claude Code does not pass its `OTEL_*` on to tools at all).
pub const AGENT_ENV: [&str; 23] = [
    // Claude Code
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    // The parent tool call's W3C trace context
    "TRACEPARENT",
    "TRACESTATE",
    // Codex
    "CODEX_THREAD_ID",
    "CODEX_CI",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_MANAGED_BY_NPM",
    "CODEX_MANAGED_BY_BUN",
    "CODEX_MANAGED_BY_PNPM",
    "CODEX_MANAGED_BY_VITE_PLUS",
    "CODEX_MANAGED_PACKAGE_ROOT",
];

pub fn is_agent_env(name: &str) -> bool {
    AGENT_ENV.contains(&name)
}

/// The spawned pane's first process (`$1`: the tmux binary, `$2`: the `sh` running it). It
/// drops every [`AGENT_ENV`] variable, wherever it came from — fleet's own environment, the
/// tmux server's global environment (a server started from inside an agent keeps that agent's
/// variables for good), the session's — and becomes what tmux itself would have started: the
/// session's effective `default-command` when one is set (read here, where the server is up
/// and its config loaded), else `$SHELL -l` (`$2 -l` when it is empty). The agent is typed
/// into a login shell either way, so the profile runs and may set any of them again on purpose.
/// Only shell builtins: no `env`, so a `PATH` without one (`FLEET_PATH_ONLY`) still works.
fn spawn_shell_script() -> String {
    format!(
        r#"unset {}
dc=$("$1" show-options -Aqv default-command 2>/dev/null)
sh=${{SHELL:-$2}}
if [ -n "$dc" ]; then exec "$sh" -c "$dc"; fi
exec "$sh" -l"#,
        AGENT_ENV.join(" ")
    )
}

/// Under `FLEET_PATH_ONLY`, what a spawn landing on `backend` (the one it really uses, see
/// [`spawn_backend`]) could never do, refused (kind `usage`) before anything is written for
/// it: iTerm at all, or a tmux pane with no `sh` on `PATH` to start it.
pub fn refuse_path_only(backend: Backend) -> Result<()> {
    match backend {
        Backend::Iterm => crate::core::tools::deny_path_only("the iTerm backend"),
        Backend::Tmux => crate::core::tools::pane_sh().map(drop),
        Backend::Unknown => Ok(()),
    }
}

/// The tmux command that opens the spawn's window in `dir`, printing its pane id and
/// window (see [`parse_spawn_target`]): a new window in an existing `session` (matched
/// exactly), or the session itself. `env` is `-e` `KEY=value`s on top of
/// [`SPAWN_SHELL_ENV`] (the job tag, and under `FLEET_PATH_ONLY` the
/// [`pane_env`](crate::core::tools::pane_env)); the pane is `sh` (see
/// [`pane_sh`](crate::core::tools::pane_sh)) running [`spawn_shell_script`].
fn tmux_spawn_args(
    session: &str,
    dir: &str,
    exists: bool,
    env: &[String],
    sh: &str,
) -> Vec<String> {
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
            "#{pane_id} #{session_name}:#{window_index}",
            "-c",
            dir,
            "-e",
            SPAWN_SHELL_ENV,
        ]
        .map(String::from),
    );
    a.extend(env.iter().flat_map(|kv| ["-e".to_string(), kv.clone()]));
    // More than one argument: tmux runs it as given, no `sh -c` around it.
    a.extend([
        sh.into(),
        "-c".into(),
        spawn_shell_script(),
        "fleet-spawn".into(),
        crate::core::tools::tmux().into(),
        sh.into(),
    ]);
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
    fn tmux_spawn_targets_the_session_exactly() {
        assert_eq!(
            tmux_spawn_args("fleet", "/tmp", true, &[], "/bin/sh")[..3],
            ["new-window", "-t", "=fleet:"]
        );
        let fresh = tmux_spawn_args("fleet", "/tmp", false, &["A=1".into()], "/bin/sh");
        assert_eq!(fresh[..4], ["new-session", "-d", "-s", "fleet"]);
        let at = fresh.iter().position(|a| a == "-c").unwrap();
        assert_eq!(
            fresh[at..at + 6],
            ["-c", "/tmp", "-e", SPAWN_SHELL_ENV, "-e", "A=1"]
        );
        assert!(fresh.contains(&"-P".to_string()));
    }

    // Under FLEET_PATH_ONLY the pane starts with fleet's PATH and the switch, set by tmux
    // `-e` — on a new session and on a window in an existing one — not the server's, and
    // is the `sh` found on that PATH.
    #[test]
    fn a_path_only_spawn_hands_the_pane_its_path() {
        let env = [
            "PATH=/t/bin:/usr/bin:/bin".to_string(),
            "FLEET_PATH_ONLY=1".into(),
        ];
        for exists in [false, true] {
            let a = tmux_spawn_args("s", "/tmp", exists, &env, "/t/bin/sh");
            let at = a.iter().position(|a| a == "-c").unwrap();
            assert_eq!(
                a[at..at + 8],
                [
                    "-c",
                    "/tmp",
                    "-e",
                    SPAWN_SHELL_ENV,
                    "-e",
                    "PATH=/t/bin:/usr/bin:/bin",
                    "-e",
                    "FLEET_PATH_ONLY=1"
                ],
                "{a:?}"
            );
            // Still the scrubbing script that ends in a login shell, run by that `sh`, which
            // it also gets as the shell to fall back on.
            assert_eq!(a[at + 8..at + 10], ["/t/bin/sh", "-c"]);
            assert_eq!(a.last().unwrap(), "/t/bin/sh");
        }
    }

    #[test]
    fn a_job_run_moves_an_iterm_spawn_to_tmux() {
        assert_eq!(effective_backend(Backend::Iterm, true), Backend::Tmux);
        assert_eq!(effective_backend(Backend::Iterm, false), Backend::Iterm);
        assert_eq!(effective_backend(Backend::Tmux, true), Backend::Tmux);
    }

    #[test]
    fn the_iterm_answer_names_the_tab_even_when_typing_failed() {
        assert_eq!(
            parse_iterm_result("ABC-123\n"),
            (Some("ABC-123".into()), None)
        );
        assert_eq!(
            parse_iterm_result("ABC-123\terror\tsession is gone\n"),
            (Some("ABC-123".into()), Some("session is gone".into()))
        );
        assert_eq!(parse_iterm_result(""), (None, None));
    }

    #[test]
    fn the_pane_starts_as_a_scrubbing_login_shell() {
        let a = tmux_spawn_args("s", "/tmp", false, &[], "/bin/sh");
        assert_eq!(
            a[a.len() - 6..],
            [
                "/bin/sh",
                "-c",
                &spawn_shell_script(),
                "fleet-spawn",
                crate::core::tools::tmux(),
                "/bin/sh"
            ]
        );
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "TRACEPARENT",
        ] {
            assert!(is_agent_env(name), "{name}");
        }
        // Configuration stays: auth, provider, telemetry exporters.
        for name in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_RESOURCE_ATTRIBUTES",
            "PATH",
            "FLEET_JOB_RUN",
        ] {
            assert!(!is_agent_env(name), "{name}");
        }
    }

    // The script itself, run by a real `sh`: the agent's markers are gone, configuration stays,
    // and it ends in a login shell — or in tmux's `default-command`, asked of tmux at run time.
    // It runs nothing from PATH (here an empty dir: no `env`), as under FLEET_PATH_ONLY.
    #[test]
    fn the_spawn_script_drops_the_markers_and_execs_what_tmux_would() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = |name: &str, body: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let empty = dir.path().join("empty-bin");
        std::fs::create_dir_all(&empty).unwrap();
        // `$SHELL` is where fleet's own PATH stops mattering: it may run what it likes.
        let shell = exe("shell", "#!/bin/sh\necho \"args:$*\"\n/usr/bin/env\n");
        let run = |tmux: &std::path::Path| {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &spawn_shell_script(), "fleet-spawn"])
                .arg(tmux)
                .arg("/bin/sh")
                .env("PATH", &empty)
                .env("SHELL", &shell)
                .env("CLAUDECODE", "1")
                .env("CLAUDE_CODE_ENTRYPOINT", "cli")
                .env("CODEX_THREAD_ID", "t-1")
                .env("TRACEPARENT", "00-a-b-01")
                .env("CLAUDE_CODE_OAUTH_TOKEN", "kept")
                .env("CLAUDE_CODE_USE_BEDROCK", "1")
                .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector")
                .env("KEEP_ME", "multi\nCLAUDECODE=line")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let no_default = exe("tmux-none", "#!/bin/sh\nexit 0\n");
        let out = run(&no_default);
        assert!(out.starts_with("args:-l\n"), "{out}");
        for gone in [
            "CLAUDECODE=1",
            "CLAUDE_CODE_ENTRYPOINT=",
            "CODEX_THREAD_ID=",
            "TRACEPARENT=",
        ] {
            assert!(!out.contains(gone), "{gone} survived:\n{out}");
        }
        for kept in [
            "CLAUDE_CODE_OAUTH_TOKEN=kept",
            "CLAUDE_CODE_USE_BEDROCK=1",
            "OTEL_EXPORTER_OTLP_ENDPOINT=http://collector",
            "KEEP_ME=multi\nCLAUDECODE=line",
        ] {
            assert!(out.contains(kept), "{kept} lost:\n{out}");
        }
        let with_default = exe("tmux-dc", "#!/bin/sh\necho 'echo custom'\n");
        let out = run(&with_default);
        assert!(out.starts_with("args:-c echo custom\n"), "{out}");
        assert!(!out.contains("CLAUDECODE=1"), "{out}");

        // An empty `$SHELL`: the `sh` it was handed (`$2`), as a login shell. (Unset, some
        // `sh`s fill it in themselves: bash takes the account's login shell.)
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &spawn_shell_script(), "fleet-spawn"])
            .arg(&no_default)
            .arg(&shell)
            .env("PATH", &empty)
            .env("SHELL", "")
            .env("CLAUDECODE", "1")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&out.stdout);
        assert!(out.starts_with("args:-l\n"), "{out}");
        assert!(!out.contains("CLAUDECODE=1"), "{out}");
    }

    #[test]
    fn the_spawn_target_is_read_as_pane_then_window() {
        assert_eq!(
            parse_spawn_target("%12 api:3\n"),
            ("%12".to_string(), "api:3".to_string())
        );
        // A `--tmux-session` name may hold a space; the pane id never does.
        assert_eq!(
            parse_spawn_target("%7 my work:0"),
            ("%7".to_string(), "my work:0".to_string())
        );
    }

    #[test]
    fn spawn_failures_are_tagged_for_json_callers() {
        let live = vec!["api".to_string()];
        let e = plan_tmux_target(None, Some("api"), "/x", &live).unwrap_err();
        assert_eq!(e.kind(), "name_taken");
        let e = plan_tmux_target(None, Some("::"), "/x", &live).unwrap_err();
        assert_eq!(e.kind(), "name_invalid");
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
            "cd '/tmp' && claude -- 'hi there'"
        );
        // A custom launcher (e.g. the user's `cc` wrapper) replaces `claude`.
        assert_eq!(
            launch_command("/tmp", Prompt::Inline("hi"), None, "cc", Harness::Claude),
            "cd '/tmp' && cc -- 'hi'"
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
            "cd '/tmp' && claude -- $'a\\nit\\'s \\\\ b'"
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
            "cd '/tmp' && claude -n 'auth fix' -- 'go'"
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

    // A failed capture read as an empty screen: to `answer`, its prompt gone — answered.
    #[test]
    fn a_capture_tmux_says_failed_is_an_error_not_an_empty_screen() {
        let s = Session {
            backend: Backend::Tmux,
            handle: Some("%999999".into()),
            ..Default::default()
        };
        for history in [0, 1000] {
            let e = peek_history(&s, history).unwrap_err();
            assert!(e.to_string().contains("capture-pane"), "{e}");
        }
    }

    #[test]
    fn keys_are_an_exact_allowlist_of_tmux_names() {
        for k in Key::ALL {
            assert_eq!(Key::parse(k.name()), Some(k), "{k:?} round-trips");
            assert!(!k.bytes().is_empty());
        }
        assert_eq!(Key::parse("C-c"), Some(Key::CtrlC));
        assert_eq!(Key::parse("BTab"), Some(Key::BTab));
        // Not on the list, or not spelled as tmux spells it.
        for bad in ["C-d", "C-z", "escape", "ESC", "Esc", "Home", "", "Enter "] {
            assert_eq!(Key::parse(bad), None, "{bad:?}");
        }
        // What iTerm types: a terminal's own sequences, in normal cursor mode.
        assert_eq!(Key::Escape.bytes(), b"\x1b");
        assert_eq!(Key::Up.bytes(), b"\x1b[A");
        assert_eq!(Key::BTab.bytes(), b"\x1b[Z");
        assert_eq!(Key::CtrlC.bytes(), b"\x03");
        assert_eq!(Key::Enter.bytes(), b"\r");
    }

    #[test]
    fn escapes_are_stripped_and_text_kept() {
        assert_eq!(strip_ansi("\x1b[1;31mred\x1b[0m plain"), "red plain");
        assert_eq!(strip_ansi("\x1b]0;title\x07after"), "after");
        assert_eq!(
            strip_ansi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        assert_eq!(strip_ansi("a\tb\x1b(Bc\x08"), "a\tbc");
        assert_eq!(strip_ansi("ünïcode ✓"), "ünïcode ✓");
    }

    #[test]
    fn screen_lines_trim_the_tail_and_never_clip() {
        let long = "x".repeat(300);
        let text = format!("one  \n\x1b[32mtwo\x1b[0m\n{long}\n\n   \n\x1b[0m\n");
        assert_eq!(
            screen_lines(&text, 40, false),
            ["one", "two", long.as_str()]
        );
        assert_eq!(screen_lines(&text, 1, false), [long.as_str()]);
        // With escapes kept, a line holding only escapes still counts as blank.
        assert_eq!(
            screen_lines(&text, 40, true),
            ["one", "\x1b[32mtwo\x1b[0m", long.as_str()]
        );
        assert!(screen_lines("\n\n", 40, false).is_empty());
        assert_eq!(parse_size("80 24"), (Some(80), Some(24)));
        assert_eq!(parse_size("junk"), (None, None));
    }

    // `send --json` documents `backend_unsupported` and `backend_failed`: its failures once
    // came back untagged (`other`), which the web server turned into a 502.
    #[test]
    fn send_failures_carry_the_backend_kinds() {
        let none = Session {
            pid: 1,
            ..Default::default()
        };
        assert_eq!(send(&none, "hi").unwrap_err().kind(), "backend_unsupported");
        let unknown = Session {
            pid: 1,
            handle: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(
            send(&unknown, "hi").unwrap_err().kind(),
            "backend_unsupported"
        );
        for text in ["hi", "two\nlines"] {
            let gone = Session {
                backend: Backend::Tmux,
                handle: Some("%999999".into()),
                ..Default::default()
            };
            let e = send(&gone, text).unwrap_err();
            assert_eq!(e.kind(), "backend_failed", "{e}");
        }
    }

    #[test]
    fn keys_refuse_a_session_without_a_backend() {
        let s = Session {
            pid: 1,
            ..Default::default()
        };
        let e = keys(&s, &[Key::Escape]).unwrap_err();
        assert_eq!(e.kind(), "backend_unsupported", "{e}");
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
            "cd '/w' && codex --model 'gpt-6' -- 'go'"
        );
        assert_eq!(
            launch_command("/w", Prompt::Inline("go"), Some("x"), "pi", Harness::Pi),
            "cd '/w' && pi -n 'x' 'go'"
        );
    }

    // `codex '--help'` showed Codex's help, `codex 'resume'` its resume picker: the prompt
    // was read as a flag or a subcommand. Behind `--` it is always the prompt.
    #[test]
    fn the_prompt_follows_a_double_dash_where_the_cli_takes_one() {
        let line = |p, h, l| launch_command("/w", Prompt::Inline(p), None, l, h);
        assert_eq!(
            line("--help", Harness::Codex, "codex"),
            "cd '/w' && codex -- '--help'"
        );
        assert_eq!(
            line("resume", Harness::Codex, "codex"),
            "cd '/w' && codex -- 'resume'"
        );
        assert_eq!(
            line("-p yes", Harness::Claude, "claude"),
            "cd '/w' && claude -- '-p yes'"
        );
        // No prompt, no `--`: nothing for it to protect.
        assert_eq!(
            launch_command("/w", Prompt::None, Some("n"), "claude", Harness::Claude),
            "cd '/w' && claude -n 'n'"
        );
        assert_eq!(
            launch_command("/w", Prompt::Inline(""), None, "codex", Harness::Codex),
            "cd '/w' && codex"
        );
    }

    // pi's parser has no `--` (it would take `--` as a flag and the prompt as its value), so a
    // prompt pi would misread gets a leading space instead.
    #[test]
    fn pi_prompts_it_would_misread_get_a_leading_space() {
        let line = |p| launch_command("/w", Prompt::Inline(p), None, "pi", Harness::Pi);
        assert_eq!(line("fix it"), "cd '/w' && pi 'fix it'");
        assert_eq!(line("--help"), "cd '/w' && pi ' --help'");
        assert_eq!(line("@notes.md read"), "cd '/w' && pi ' @notes.md read'");
        assert_eq!(line("update"), "cd '/w' && pi ' update'");
        assert_eq!(line("update the docs"), "cd '/w' && pi 'update the docs'");
        assert_eq!(
            launch_command("/w", Prompt::File("/f.md"), None, "pi", Harness::Pi),
            r#"cd '/w' && fleet_prompt="$(cat '/f.md'; printf .)" && pi " ${fleet_prompt%.}""#
        );
    }

    #[test]
    fn prompt_file_expands_in_the_spawned_shell() {
        // Run by a real shell: the file's text arrives byte for byte, trailing newlines too.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("p.md");
        let text = "it's \"x\" $HOME `y` \\ é\n\nlast line\n\n";
        std::fs::write(&f, text).unwrap();
        let line = run_command(
            Prompt::File(&f.display().to_string()),
            None,
            "printf '%s|'",
            Harness::Claude,
        );
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &line])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("--|{text}|")
        );
        // The brief's text must never reach the command line — only its path does.
        assert_eq!(
            launch_command(
                "/tmp",
                Prompt::File("/tmp/a b.md"),
                None,
                "claude",
                Harness::Claude
            ),
            r#"cd '/tmp' && fleet_prompt="$(cat '/tmp/a b.md'; printf .)" && claude -- "${fleet_prompt%.}""#
        );
    }
}
