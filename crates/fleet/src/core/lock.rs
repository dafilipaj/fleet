//! One fleet command at a time types into a session: an exclusive lock per session (an
//! `flock` on a file named after it), held for a whole read-screen → type → verify sequence,
//! so two `fleet answer` / `fleet send` runs cannot interleave their keys. A lock already held
//! fails fast — nothing waits for it.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use crate::core::discovery::Session;

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
}
