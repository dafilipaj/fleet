//! One fleet command at a time types into a session: an exclusive lock per session (an
//! `flock` on a file named after it), held for a whole read-screen → type → verify sequence,
//! so two `fleet answer` / `fleet send` runs cannot interleave their keys. A lock already held
//! fails fast — nothing waits for it.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::discovery::Session;
use crate::core::harness::Harness;

/// Held while it lives; dropping it (or the process ending) releases the lock.
#[derive(Debug)]
pub struct SessionLock {
    file: File,
}

impl Drop for SessionLock {
    /// Unlock explicitly rather than by closing: a child another thread is spawning holds a
    /// copy of the descriptor until its `exec`, and a `flock` lasts until its last copy closes.
    /// Unlocking releases it at once, whoever else holds a copy.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug)]
pub enum LockError {
    /// Another fleet command is typing into this session right now.
    Busy,
    Io(std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy => f.write_str("another fleet command is typing into this session"),
            LockError::Io(e) => write!(f, "cannot take the session lock: {e}"),
        }
    }
}

/// Where the lock files live: `$FLEET_LOCK_DIR`, else the runtime dir (`$XDG_RUNTIME_DIR`),
/// else the cache dir, under `fleet/locks`.
pub fn lock_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FLEET_LOCK_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("fleet")
        .join("locks")
}

/// The lock file of a session: by session id, else by pid.
pub fn lock_path(s: &Session) -> PathBuf {
    let key = match s.session_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => format!("pid-{}", s.pid),
    };
    let key: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    lock_dir().join(format!("{key}.lock"))
}

/// Take the session's lock, or say who has it.
pub fn try_lock(s: &Session) -> Result<SessionLock, LockError> {
    try_lock_at(&lock_path(s))
}

/// How long a `spawn --resume` holds a session it started whose host is unknown or idle: the
/// start's own few seconds, before its terminal runs the harness (and iTerm, which has no
/// host fleet can check). See [`Reservation::starting`].
pub const RESUME_HOLD: Duration = Duration::from_secs(30);

/// A `spawn --resume`'s claim on one harness's session id: the lock (an `flock`, like a
/// [`SessionLock`]) on `resume-<harness>-<lowercased id>.lock` in [`lock_dir`], held while
/// that one fleet checks the session is not live and starts it, so two fleets cannot both
/// pass the check. What outlives the fleet process is the stamp beside it
/// (`resume-….stamp`, see [`Stamp`]): written before anything is typed, it says when the
/// start began and, once the terminal exists, which tmux pane hosts it.
#[derive(Debug)]
pub struct Reservation {
    _lock: SessionLock,
    stamp: PathBuf,
}

/// What a resume leaves behind for later ones: when it started (ms since the epoch), and the
/// tmux pane it typed the resume into with that pane's shell pid (`None` before the terminal
/// exists, and for iTerm).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    pub at: u64,
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub shell: Option<i64>,
}

impl Stamp {
    /// The session it reserves is still starting: under [`RESUME_HOLD`] old, or its host pane
    /// still exists and runs what was typed into it (a resume waiting at the harness's trust
    /// prompt has not registered yet, however long it waits). Once the pane is gone, or its
    /// shell is back at the prompt, the stamp is stale.
    pub fn holds(&self, now: u64, host_running: impl Fn(&str, i64) -> bool) -> bool {
        if now.saturating_sub(self.at) < RESUME_HOLD.as_millis() as u64 {
            return true;
        }
        match (&self.pane, self.shell) {
            (Some(pane), Some(shell)) => host_running(pane, shell),
            _ => false,
        }
    }
}

/// The reservation file of `harness`'s session `id`. The id is the case-insensitive match
/// `id:` targets use, so it is lowercased; anything but letters, digits and `-` becomes `_`.
pub fn resume_path(harness: Harness, id: &str) -> PathBuf {
    let key: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    lock_dir().join(format!("resume-{}-{key}.lock", harness.id()))
}

/// The stamp beside the reservation `lock` file.
pub fn stamp_path(lock: &Path) -> PathBuf {
    lock.with_extension("stamp")
}

/// Claim `harness`'s session `id` for a resume; [`LockError::Busy`] while another fleet is
/// checking or starting it right now.
pub fn reserve_resume(harness: Harness, id: &str) -> Result<Reservation, LockError> {
    let path = resume_path(harness, id);
    Ok(Reservation {
        _lock: try_lock_at(&path)?,
        stamp: stamp_path(&path),
    })
}

impl Reservation {
    /// An earlier resume of this session is still starting it (see [`Stamp::holds`]).
    pub fn starting(&self) -> bool {
        stamp_holds(&self.stamp)
    }

    /// Record, before anything is typed, that a start begins now. Later resumes see it as
    /// starting even when this process dies before it can say more.
    pub fn claim(&self) -> std::io::Result<()> {
        self.write(&Stamp {
            at: now_ms(),
            pane: None,
            shell: None,
        })
    }

    /// Tie the start to the tmux pane it is about to be typed into, before it is typed.
    pub fn host(&self, pane: &str) -> std::io::Result<()> {
        self.write(&Stamp {
            at: now_ms(),
            pane: Some(pane.to_string()),
            shell: crate::core::tmux::pane_pid(pane),
        })
    }

    /// Nothing started after all: later resumes are free at once.
    pub fn release(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.stamp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Write the whole stamp or nothing (a new file renamed over the old one), so a process
    /// killed mid-write never leaves a half stamp that reads as none. The lock stays on the
    /// `.lock` file's own inode.
    fn write(&self, stamp: &Stamp) -> std::io::Result<()> {
        let tmp = self
            .stamp
            .with_extension(format!("stamp.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(stamp)?)?;
        std::fs::rename(&tmp, &self.stamp).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

/// [`Reservation::starting`] without claiming anything (a dry run looks, and writes nothing).
pub fn resume_starting(harness: Harness, id: &str) -> bool {
    stamp_holds(&stamp_path(&resume_path(harness, id)))
}

/// The stamp at `path`, when there is a readable one.
pub fn read_stamp(path: &Path) -> Option<Stamp> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn stamp_holds(path: &Path) -> bool {
    read_stamp(path).is_some_and(|s| s.holds(now_ms(), crate::core::tmux::pane_running))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Take the lock in the file at `path`, creating it (and its directory) if needed.
fn try_lock_at(path: &Path) -> Result<SessionLock, LockError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(LockError::Io)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(LockError::Io)?;
    match file.try_lock() {
        Ok(()) => Ok(SessionLock { file }),
        Err(TryLockError::WouldBlock) => Err(LockError::Busy),
        Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_makes_the_next_one_busy_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks").join("abc.lock");
        let held = try_lock_at(&path).unwrap();
        assert!(matches!(try_lock_at(&path), Err(LockError::Busy)));
        // Released the moment it is dropped, even while a child another test thread is
        // spawning still holds a copy of its descriptor.
        drop(held);
        drop(try_lock_at(&path).unwrap());
        // The session's key is sanitised into one file name (no path separators, no dots).
        let s = Session {
            session_id: Some("abc/../x".into()),
            ..Session::default()
        };
        assert_eq!(lock_path(&s).file_name().unwrap(), "abc____x.lock");
        let s = Session {
            pid: 42,
            ..Session::default()
        };
        assert_eq!(lock_path(&s).file_name().unwrap(), "pid-42.lock");
    }

    #[test]
    fn a_stamp_holds_for_the_hold_or_while_its_host_runs_the_resume() {
        let hold = RESUME_HOLD.as_millis() as u64;
        let bare = Stamp {
            at: 1_000_000,
            pane: None,
            shell: None,
        };
        let never = |_: &str, _: i64| -> bool { panic!("no host to check") };
        assert!(bare.holds(bare.at + hold - 1, never));
        assert!(!bare.holds(bare.at + hold, never));
        let hosted = Stamp {
            pane: Some("%3".into()),
            shell: Some(42),
            ..bare
        };
        assert!(hosted.holds(hosted.at + hold * 100, |p, s| p == "%3" && s == 42));
        assert!(!hosted.holds(hosted.at + hold, |_, _| false));
    }

    #[test]
    fn a_resume_reservation_is_per_harness_and_case_blind() {
        let a = resume_path(Harness::Claude, "AbC-1");
        assert_eq!(a.file_name().unwrap(), "resume-claude-abc-1.lock");
        assert_eq!(a, resume_path(Harness::Claude, "abc-1"));
        assert_ne!(a, resume_path(Harness::Codex, "abc-1"));
        assert_eq!(
            resume_path(Harness::Codex, "a/../b").file_name().unwrap(),
            "resume-codex-a____b.lock"
        );
    }
}
