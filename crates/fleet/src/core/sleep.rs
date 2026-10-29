//! `fleet sleep`: close a session but keep it **dormant**. Its agent process ends and its
//! terminal goes (the tmux window, or the tmux session when that was its only window; an
//! iTerm tab is left to the caller), and the snapshot keeps it in the dormant list instead of
//! the recently closed one — marked `slept`, so only `fleet restore <target>` (or `enter`,
//! or the web UI) brings it back, never `restore --all` / `restore.onBoot`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::core::discovery::{self, Backend, Session};
use crate::core::snapshot::{self, DormantView};
use crate::error::{Error, Result};

/// SIGTERM → SIGKILL grace period.
const GRACE: Duration = Duration::from_secs(4);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slept {
    pub session_id: String,
    pub title: String,
    /// `terminated` | `killed` | `gone`
    pub process: &'static str,
    /// `tmux-session-killed` | `tmux-window-killed` | `tmux-pane-gone` | `left`
    pub terminal: &'static str,
    /// The dormant entry it is now (`target` is what `fleet restore` takes); `None` on a dry run.
    pub dormant: Option<DormantView>,
    pub dry_run: bool,
}

fn signal(pid: i64, sig: &str) -> bool {
    Command::new("kill")
        .args([sig, &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn alive(pid: i64) -> bool {
    signal(pid, "-0")
}

/// SIGTERM, wait up to [`GRACE`], then SIGKILL.
fn terminate(pid: i64) -> &'static str {
    if !alive(pid) || !signal(pid, "-TERM") {
        return "gone";
    }
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if !alive(pid) {
            return "terminated";
        }
    }
    signal(pid, "-KILL");
    std::thread::sleep(Duration::from_millis(200));
    "killed"
}

/// Put `s` to sleep: record it, end it, take its tmux window/session down, and move it into
/// the dormant list.
pub fn sleep(s: &Session) -> Result<Slept> {
    let session_id = s
        .session_id
        .clone()
        .filter(|id| !id.starts_with("pid-"))
        .ok_or_else(|| {
            Error::Other(format!(
                "{} has no session id yet — nothing to resume it from",
                s.headline()
            ))
        })?;
    if discovery::is_fixture() {
        return Err(Error::Other(
            "fixture mode: sleep is inert (unset FLEET_FIXTURE)".into(),
        ));
    }
    let title = s.headline();
    if crate::core::hosts::dry_run() {
        println!("kill -TERM {}", s.pid);
        return Ok(Slept {
            session_id,
            title,
            process: "skipped",
            terminal: "left",
            dormant: None,
            dry_run: true,
        });
    }
    // What a restore needs (cwd, flags, tmux layout) must be on record before it ends.
    snapshot::refresh()?;
    // Resolved before the process dies: a pane whose own command was the agent goes with it.
    let home = match (s.backend, s.handle.as_deref()) {
        (Backend::Tmux, Some(pane)) => crate::core::tmux::pane_home(pane),
        _ => None,
    };
    let process = terminate(s.pid);
    let terminal = match (s.backend, &home) {
        (Backend::Tmux, Some(h)) => crate::core::tmux::close_home(h)?,
        (Backend::Tmux, None) => "tmux-pane-gone",
        _ => "left",
    };
    // Recorded again with it gone: it lingers as just ended, and from there goes dormant.
    let rows = discovery::discover_checked().map_err(Error::Other)?;
    if rows
        .iter()
        .any(|r| r.session_id.as_deref() == Some(&session_id))
    {
        return Err(Error::Other(format!("{title} is still running")));
    }
    snapshot::refresh()?;
    let dormant = snapshot::put_to_sleep(&session_id)?;
    Ok(Slept {
        session_id,
        title,
        process,
        terminal,
        dormant: Some(dormant),
        dry_run: false,
    })
}
