//! Read and answer a Session host's folder-trust dialog before its harness registers.
//! The host lock covers validation, every screen/key pair, outcome verification and close.
//! No screen or backend text is ever put into a failure's codes or message.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use super::{
    backend, lock,
    trust_prompt::{self, Pane, Prompt, Read},
};

/// How often the screen is read while waiting on it.
const POLL: Duration = Duration::from_millis(40);
/// A key the dialog ignored is pressed again this long after the last.
const RETRY: Duration = Duration::from_millis(400);
/// How long the prompt has, after Enter, to leave the screen.
const OUTCOME: Duration = Duration::from_secs(3);
/// How long the harness has, after a decline, to exit before the host is left open.
const CLOSE: Duration = Duration::from_secs(5);
/// At most this many presses of a key the dialog ignores.
const ATTEMPTS: usize = 4;
/// How long one tmux command may take before it counts as failed (a wedged server).
const TMUX_LIMIT: Duration = Duration::from_secs(5);
/// What `capture` has tmux print after the screen, in the same command: the pane at that
/// instant, its cursor and mode included.
const PANE_MARK: &str = "\u{1}fleet-trust-pane\t";
const PANE_FORMAT: &str = "\u{1}fleet-trust-pane\t#{cursor_x}\t#{cursor_y}\t#{pane_height}\t#{pane_in_mode}\t#{pane_current_command}";

/// `fleet trust`'s flags: `list`, else `--accept` (`accept`) or `--decline`.
pub struct Opts {
    pub session: String,
    pub pane: Option<String>,
    pub list: bool,
    pub accept: bool,
    pub prompt: Option<String>,
    pub close: bool,
    pub allow_attached: bool,
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Size {
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub detail: Option<&'static str>,
    pub pane: Option<Size>,
    pub typed: bool,
    pub steps: Vec<&'static str>,
    pub screen: Option<Box<Value>>,
}

impl Failure {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            detail: None,
            pane: None,
            typed: false,
            steps: Vec::new(),
            screen: None,
        }
    }

    /// What went wrong, in words made only of the kind, the code and the keys' count: never
    /// the screen's text, nor tmux's (it may quote a session name).
    pub fn message(&self) -> String {
        let what = match (self.kind, self.detail) {
            ("usage", _) => "fleet trust runs only on this machine: run it without -H".into(),
            ("target_not_found", _) => "no tmux session has exactly that name".into(),
            ("target_invalid", _) => "not a name a tmux session can have".into(),
            ("target_changed", _) => {
                "the tmux session no longer holds the pane --pane names".into()
            }
            ("host_unsupported", _) => {
                "the tmux session is not one window with one pane, as spawn makes it".into()
            }
            ("busy", _) => "another fleet trust is answering in this tmux session".into(),
            ("human_attached", _) => {
                "a terminal is attached to the tmux session: answer there, or pass --allow-attached"
                    .into()
            }
            ("prompt_changed", d) => format!(
                "the screen changed while answering ({}); stopped after {} key(s) — look at the terminal",
                d.unwrap_or("unreadable"),
                self.steps.len()
            ),
            ("outcome_uncertain", _) => {
                "Enter went out, but the trust prompt did not leave the screen — look at the terminal"
                    .into()
            }
            ("backend_failed", _) => "tmux failed, or the screen could not be read".into(),
            ("io", _) => "the trust lock could not be taken".into(),
            (_, Some("no_prompt")) => "no trust prompt on screen".into(),
            (_, Some("shell")) => "the harness is not running: the pane's foreground is its shell".into(),
            (_, Some("different_prompt")) => "the trust prompt on screen is not --prompt's".into(),
            (_, Some("touched")) => {
                "the trust prompt's cursor is off its preselected answer".into()
            }
            (_, Some("not_alone")) => "something else is on screen with the trust prompt".into(),
            (_, Some("clipped")) => "the trust prompt is not on screen whole".into(),
            (_, Some("layout_unsupported")) => {
                "a trust prompt in a layout fleet does not know".into()
            }
            (kind, _) => kind.into(),
        };
        if self.typed
            || matches!(
                self.kind,
                "usage"
                    | "target_invalid"
                    | "target_not_found"
                    | "outcome_uncertain"
                    | "prompt_changed"
            )
        {
            what
        } else {
            format!("{what}; nothing typed")
        }
    }

    pub fn json(&self) -> Value {
        let mut v = json!({ "ok": false, "kind": self.kind, "error": self.message(),
            "detail": self.detail, "pane": self.pane, "typed": self.typed });
        if self.typed {
            v["keys_sent"] = json!(self.steps.len());
            v["steps"] = steps_json(&self.steps);
        }
        if self.kind == "outcome_uncertain" {
            v["screen"] = self.screen.as_deref().cloned().unwrap_or(Value::Null);
        }
        v
    }
}

type R<T> = Result<T, Failure>;

fn steps_json(steps: &[&str]) -> Value {
    json!(steps.iter().map(|k| json!({"key": k})).collect::<Vec<_>>())
}

fn query(args: &[&str]) -> R<String> {
    let out = backend::tmux_within(args, TMUX_LIMIT).map_err(|_| Failure::new("backend_failed"))?;
    if !out.status.success() {
        return Err(Failure::new("backend_failed"));
    }
    String::from_utf8(out.stdout).map_err(|_| Failure::new("backend_failed"))
}

#[derive(Debug)]
struct Host {
    id: String,
    handle: String,
    size: Size,
    command: String,
    pid: i64,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '.' | ':'))
}

/// An exact session snapshot, before any capture. A linked window is unsupported too:
/// another session would provide another way for a person to type into this pane.
fn host(name: &str, pane: Option<&str>) -> R<Host> {
    let out = backend::tmux_within(&["list-panes", "-s", "-t", &format!("={name}"), "-F",
        "#{session_id}\t#{session_name}\t#{session_windows}\t#{window_panes}\t#{window_linked}\t#{pane_id}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}\t#{pane_pid}"], TMUX_LIMIT)
        .map_err(|_| Failure::new("backend_failed"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let gone = err.starts_with("can't find session")
            || err.starts_with("no server running on ")
            || (err.starts_with("error connecting to ") && err.contains("No such file"));
        return Err(Failure::new(if gone {
            "target_not_found"
        } else {
            "backend_failed"
        }));
    }
    let text = String::from_utf8(out.stdout).map_err(|_| Failure::new("backend_failed"))?;
    let rows: Vec<_> = text
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .collect();
    if rows.is_empty() {
        return Err(Failure::new("target_not_found"));
    }
    if rows.iter().any(|r| r.len() != 10 || r[1] != name) {
        return Err(Failure::new("backend_failed"));
    }
    if rows.len() != 1 || rows[0][2..5] != ["1", "1", "0"] {
        return Err(Failure::new("host_unsupported"));
    }
    let row = &rows[0];
    if pane.is_some_and(|p| p != row[5]) {
        return Err(Failure::new("target_changed"));
    }
    let parse = || {
        Some(Host {
            id: row[0].into(),
            handle: row[5].into(),
            size: Size {
                cols: row[6].parse().ok()?,
                rows: row[7].parse().ok()?,
            },
            command: row[8].into(),
            pid: row[9].parse().ok()?,
        })
    };
    parse()
        .filter(|h| h.size.cols > 0 && h.size.rows > 0 && h.pid > 0)
        .ok_or_else(|| Failure::new("backend_failed"))
}

/// The visible screen, and in the same tmux command (so at the same instant) the pane's
/// foreground, cursor, height and mode: a screen read live is never paired with a later cursor.
fn capture(h: &Host) -> R<Read> {
    let text = query(&[
        "capture-pane",
        "-p",
        "-e",
        "-J",
        "-t",
        &h.handle,
        ";",
        "display-message",
        "-p",
        "-t",
        &h.handle,
        PANE_FORMAT,
    ])?;
    let bad = || Failure::new("backend_failed");
    let (screen, pane) = text.rsplit_once(PANE_MARK).ok_or_else(bad)?;
    let fields: Vec<_> = pane.trim_end_matches('\n').splitn(5, '\t').collect();
    let [x, y, rows, mode, command] = fields[..] else {
        return Err(bad());
    };
    let number = |s: &str| s.parse::<usize>().map_err(|_| bad());
    let pane = Pane {
        command,
        cursor: (number(x)?, number(y)?),
        rows: number(rows)?,
        in_mode: mode != "0",
    };
    Ok(trust_prompt::read(screen, &pane))
}

fn list_json(o: &Opts, h: &Host, read: &Read) -> Value {
    let mut v = json!({ "ok": true, "tmux_session": o.session, "handle": h.handle,
        "prompt": read.json(), "answerable": read.detail.is_none(), "detail": read.detail,
        "pane": h.size });
    if o.dry_run {
        v["dry_run"] = json!(true);
    }
    v
}

fn refusal(detail: &'static str) -> Failure {
    let mut f = Failure::new(if matches!(detail, "clipped" | "layout_unsupported") {
        "prompt_incomplete"
    } else {
        "prompt_not_on_screen"
    });
    f.detail = Some(detail);
    f
}

fn bound_host(o: &Opts, original: &Host) -> R<Host> {
    let now = host(&o.session, Some(&original.handle))?;
    if now.id != original.id {
        return Err(Failure::new("target_changed"));
    }
    Ok(now)
}

/// Just before a key: bind the host again, then read the screen. A moved cursor is allowed
/// only while it is the cursor movement this command itself is verifying.
fn checked(o: &Opts, original: &Host, id: &str) -> R<(Host, Read)> {
    let now = bound_host(o, original)?;
    let read = capture(&now)?;
    if let Some(detail) = read.detail.filter(|d| *d != "touched") {
        return Err(refusal(detail));
    }
    if read.prompt.as_ref().is_none_or(|p| p.id != id) {
        return Err(refusal("different_prompt"));
    }
    Ok((now, read))
}

/// Unless `--allow-attached`: no terminal client on the tmux session, where a person could be
/// typing.
fn unattended(o: &Opts) -> R<()> {
    if o.allow_attached {
        return Ok(());
    }
    let clients = query(&[
        "list-clients",
        "-t",
        &format!("={}", o.session),
        "-F",
        "#{client_tty}",
    ])?;
    if clients.trim().is_empty() {
        Ok(())
    } else {
        Err(Failure::new("human_attached"))
    }
}

/// One key, the attached-client check made again right before it: a person who attached since
/// the last key gets the terminal back.
fn press(o: &Opts, h: &Host, key: &'static str, steps: &mut Vec<&'static str>) -> R<()> {
    unattended(o)?;
    // A failed send may have reached the pane. Count it, as answer/send do for `typed`.
    steps.push(key);
    query(&["send-keys", "-t", &h.handle, key]).map(|_| ())
}

fn changed(mut f: Failure) -> Failure {
    if matches!(
        f.kind,
        "target_changed" | "target_not_found" | "host_unsupported"
    ) {
        f.detail = Some("different_prompt");
    }
    if f.kind != "backend_failed" {
        f.kind = "prompt_changed";
    }
    f
}

fn after_keys(f: Failure, steps: &[&str]) -> Failure {
    if steps.is_empty() { f } else { changed(f) }
}

fn navigate(o: &Opts, h: &Host, p: &Prompt, chosen: usize, steps: &mut Vec<&'static str>) -> R<()> {
    if p.selected == chosen {
        return Ok(());
    }
    let key = if chosen > p.selected { "Down" } else { "Up" };
    for _ in 0..ATTEMPTS {
        let (now, read) = checked(o, h, &p.id).map_err(|f| after_keys(f, steps))?;
        let current = read.prompt.as_ref().unwrap();
        if steps.is_empty() && current.selected != p.selected {
            return Err(refusal("touched"));
        }
        if current.selected == chosen {
            return Ok(());
        }
        if current.selected != p.selected {
            return Err(changed(refusal("touched")));
        }
        press(o, &now, key, steps)?;
        std::thread::sleep(RETRY);
        let (_, read) = checked(o, h, &p.id).map_err(changed)?;
        if read.prompt.as_ref().unwrap().selected == chosen {
            return Ok(());
        }
    }
    Err(changed(refusal("touched")))
}

/// Enter, on the chosen answer, then wait for the prompt to leave the screen. An Enter the
/// dialog ignored is pressed again only while it is the first key: once a Down or Up has
/// landed, the dialog listens, and an unchanged screen means Claude Code has not redrawn yet
/// — a second Enter could land on whatever it draws next (another dialog's default answer).
///
/// What counts as the prompt gone depends on the answer. A decline exits Claude Code, which
/// leaves the dialog's text on the screen above the shell: no live prompt is enough. An accept
/// keeps it running and clears the dialog: the shell back means the harness exited without
/// trusting the folder, and the text still there is no proof either.
fn enter(o: &Opts, h: &Host, p: &Prompt, chosen: usize, steps: &mut Vec<&'static str>) -> R<()> {
    let attempts = if steps.is_empty() { ATTEMPTS } else { 1 };
    let mut sent = 0;
    let mut next_retry = Instant::now();
    let deadline = Instant::now() + OUTCOME;
    let last_screen = loop {
        let now = match bound_host(o, h) {
            Ok(now) => now,
            Err(f) if sent == 0 => return Err(after_keys(f, steps)),
            Err(_) => break None,
        };
        let read = match capture(&now) {
            Ok(read) => read,
            Err(f) if sent == 0 => return Err(f),
            Err(_) => break None,
        };
        let same = read
            .prompt
            .as_ref()
            .is_some_and(|current| current.id == p.id && current.selected == chosen)
            && read.detail.is_none_or(|d| d == "touched");
        if sent == 0 && !same {
            let different = read
                .prompt
                .as_ref()
                .is_some_and(|current| current.id != p.id);
            return Err(after_keys(
                refusal(if different {
                    "different_prompt"
                } else {
                    read.detail.unwrap_or("different_prompt")
                }),
                steps,
            ));
        }
        if sent > 0 && read.prompt.is_none() && read.incomplete.is_none() {
            match read.detail {
                Some("no_prompt") if !o.accept || !read.stale => return Ok(()),
                Some("shell") if !o.accept => return Ok(()),
                Some("shell") => break Some(list_json(o, &now, &read)),
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            break Some(list_json(o, &now, &read));
        }
        if same && sent < attempts && Instant::now() >= next_retry {
            press(o, &now, "Enter", steps)?;
            sent += 1;
            next_retry = Instant::now() + RETRY;
        }
        std::thread::sleep(POLL);
    };
    let mut f = Failure::new("outcome_uncertain");
    f.screen = last_screen.map(Box::new);
    Err(f)
}

/// The foreground is back to a shell AND it has no child, including a background job.
/// A failed process query proves nothing and cannot authorize a close.
fn exited(h: &Host) -> bool {
    if !trust_prompt::is_shell(&h.command) {
        return false;
    }
    let Ok(out) = std::process::Command::new("ps")
        .args(["-Ao", "ppid="])
        .output()
    else {
        return false;
    };
    out.status.success()
        && String::from_utf8_lossy(&out.stdout)
            .lines()
            .all(|l| l.trim().parse::<i64>().is_ok_and(|pid| pid != h.pid))
}

/// C5: once the harness has exited, kill the tmux session, `None`; else why it is left as it
/// is. A host that can no longer be bound to the one read (renamed away, replaced, split,
/// gone) is `pane_changed`.
fn close(o: &Opts, h: &Host) -> R<Option<&'static str>> {
    let deadline = Instant::now() + CLOSE;
    loop {
        let now = match bound_host(o, h) {
            Ok(now) => now,
            Err(f) if f.kind == "backend_failed" => return Err(f),
            Err(_) => return Ok(Some("pane_changed")),
        };
        if exited(&now) {
            // One tmux command checks the session (by its id: never reused), its one window
            // and its one pane, and kills it: nothing can change between the check and the kill.
            return match super::tmux::kill_if_alone_within(&h.id, &h.handle, TMUX_LIMIT) {
                Ok(super::tmux::KillAlone::Killed(_)) => Ok(None),
                Ok(_) => Ok(Some("pane_changed")),
                Err(_) => Err(Failure::new("backend_failed")),
            };
        }
        if Instant::now() >= deadline {
            return Ok(Some("still_running"));
        }
        std::thread::sleep(POLL);
    }
}

/// Names may contain slashes and percent signs in tmux. Encode those injectively so a host
/// cannot escape the lock directory or collide with another host. Ordinary spawn names use
/// exactly `trust-<name>.lock`.
fn lock_path(name: &str) -> std::path::PathBuf {
    let key = name.replace('%', "%25").replace('/', "%2f");
    lock::lock_dir().join(format!("trust-{key}.lock"))
}

pub fn run(o: &Opts) -> R<Value> {
    if !valid_name(&o.session) {
        return Err(Failure::new("target_invalid"));
    }
    let _held = if o.list {
        None
    } else {
        Some(lock::try_lock_at(&lock_path(&o.session)).map_err(|e| {
            Failure::new(match e {
                lock::LockError::Busy => "busy",
                lock::LockError::Io(_) => "io",
            })
        })?)
    };
    let h = host(&o.session, o.pane.as_deref())?;
    let mut steps = Vec::new();
    let result = (|| {
        if !o.list {
            unattended(o)?;
        }
        let read = capture(&h)?;
        if o.list {
            return Ok(list_json(o, &h, &read));
        }
        if let Some(detail) = read.detail {
            return Err(refusal(detail));
        }
        let p = read.prompt.ok_or_else(|| refusal("no_prompt"))?;
        if o.prompt.as_deref() != Some(&p.id) {
            return Err(refusal("different_prompt"));
        }
        let chosen = p.choices.iter().position(|c| c.accept == o.accept).unwrap();
        let mut v = json!({ "ok": true, "tmux_session": o.session, "handle": h.handle,
            "harness": "claude", "answered": !o.dry_run,
            "answer": if o.accept {"accept"} else {"decline"}, "id": p.id, "closed": null });
        if o.dry_run {
            if p.selected != chosen {
                steps.push(if chosen > p.selected { "Down" } else { "Up" });
            }
            steps.push("Enter");
            v["dry_run"] = json!(true);
        } else {
            navigate(o, &h, &p, chosen, &mut steps)?;
            enter(o, &h, &p, chosen, &mut steps)?;
            if o.close {
                let why = close(o, &h)?;
                v["closed"] = json!(why.is_none());
                if let Some(why) = why {
                    v["close_detail"] = json!(why);
                }
            }
        }
        v["steps"] = steps_json(&steps);
        Ok(v)
    })();
    result.map_err(|mut f: Failure| {
        f.pane = Some(h.size);
        f.typed = !steps.is_empty() && !o.dry_run;
        f.steps = steps;
        f
    })
}
