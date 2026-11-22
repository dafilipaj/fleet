//! Host load, the CLI side (docs/architecture.md → Host load). The web server samples this
//! machine's memory pressure and CPU load and writes its verdict to `<state>/fleet/load.json`
//! (`web/lib/load.mjs`); while it says `hot` or `critical`, new sessions wait:
//!
//! - a spawn you start (`spawn`, `handoff`, `pr` / `task` / `stack spawn`, `restore`, `bots run`)
//!   is refused with a clear message unless `--ignore-load` (env `FLEET_IGNORE_LOAD=1`);
//! - the automatic ones (`schedule tick`, `jobs tick`) skip the tick and fire once the host has
//!   recovered — a due entry stays due, a bot's slot is caught up (`jobs::CATCH_UP_SECS`).
//!
//! Fail-open: no file, a file that does not parse, or one older than `STALE_MS` (the server is
//! off, or `resources.enabled` is not set — it removes the file then) means not busy.

use std::path::PathBuf;

use serde::Deserialize;

use crate::core::tools;
use crate::error::{Error, Result};

/// Must match `STALE_MS` in `web/lib/load.mjs`.
pub const STALE_MS: i64 = 90 * 1000;

/// The words the web server and the UI recognise (`BUSY_MARK` in `web/lib/load.mjs`).
pub const BUSY_MARK: &str = "is under heavy load";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadState {
    pub state: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub updated_at: i64,
}

impl LoadState {
    pub fn busy(&self) -> bool {
        self.state == "hot" || self.state == "critical"
    }
}

static IGNORE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `--ignore-load`: start sessions on a busy host anyway (this process).
pub fn set_ignore() {
    IGNORE.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn ignored() -> bool {
    IGNORE.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("FLEET_IGNORE_LOAD").is_ok_and(|v| v == "1" || v == "true")
}

pub fn state_path() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_LOAD_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(tools::expand_tilde(&p.to_string_lossy()));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"));
    base.join("fleet").join("load.json")
}

/// The state in `text` if it is fresh at `now_ms` and says busy.
pub fn busy_in(text: &str, now_ms: i64) -> Option<LoadState> {
    let st: LoadState = serde_json::from_str(text).ok()?;
    if now_ms - st.updated_at > STALE_MS || st.updated_at > now_ms + STALE_MS {
        return None;
    }
    st.busy().then_some(st)
}

/// This machine's load verdict, when it is busy now.
pub fn busy() -> Option<LoadState> {
    let text = std::fs::read_to_string(state_path()).ok()?;
    busy_in(&text, chrono::Local::now().timestamp_millis())
}

pub fn message(host: &str, st: &LoadState) -> String {
    let why = if st.reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", st.reasons.join(", "))
    };
    format!("{host} {BUSY_MARK}{why} — new sessions wait until it recovers")
}

/// `Err` while this machine is busy, unless `--ignore-load`: every command that starts an
/// agent calls it (through `spawn_tagged`, and `restore`).
pub fn ensure_not_busy() -> Result<()> {
    if ignored() {
        return Ok(());
    }
    match busy() {
        None => Ok(()),
        Some(st) => Err(Error::Other(format!(
            "{}; add --ignore-load to start it anyway",
            message(&crate::core::config::get().self_name(), &st)
        ))),
    }
}

/// For the ticks: why they hold this round, or `None`. `--ignore-load` does not apply (a tick
/// is never started by hand with it in mind).
pub fn hold_reason() -> Option<String> {
    busy().map(|st| message(&crate::core::config::get().self_name(), &st))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_only_when_fresh_and_hot() {
        let at = 1_000_000;
        let hot = format!(r#"{{"state":"hot","reasons":["load 2.5× cores"],"updatedAt":{at}}}"#);
        assert!(busy_in(&hot, at + 1000).is_some());
        assert!(busy_in(&hot, at + STALE_MS + 1).is_none(), "stale");
        let ok = format!(r#"{{"state":"ok","reasons":[],"updatedAt":{at}}}"#);
        assert!(busy_in(&ok, at).is_none());
        let crit = format!(r#"{{"state":"critical","updatedAt":{at}}}"#);
        assert!(busy_in(&crit, at).is_some());
        assert!(busy_in("not json", at).is_none());
        assert!(busy_in(r#"{"state":"hot"}"#, at).is_none(), "no timestamp");
    }

    #[test]
    fn message_names_the_reasons() {
        let st = LoadState {
            state: "hot".into(),
            reasons: vec!["memory pressure warn".into(), "load 2.1× cores".into()],
            updated_at: 0,
        };
        assert_eq!(
            message("workstation", &st),
            "workstation is under heavy load (memory pressure warn, load 2.1× cores) — new sessions wait until it recovers"
        );
    }
}
