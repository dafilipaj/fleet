//! `fleet schedule`: one-off agent sessions started later, on this machine — at a set time, or
//! on a Productive task's start date. The `bots tick` launchd agent fires them (every minute).
//!
//! An entry is either *timed* (`at` fixed when it was added) or *task-linked* (`task` set):
//! a task-linked entry follows the task — it fires at `time` on the task's start date, and the
//! tick re-reads the task now and then (every [`TASK_REFRESH_SECS`], and right before firing),
//! so a moved start date moves the session and a closed task skips it. A task with no start
//! date yet waits until it gets one. The session's first prompt is the task's context line
//! plus the entry's prompt, as with `fleet task spawn`; the task's status is left alone.
//!
//! A timed entry whose time is more than [`CATCH_UP_SECS`] past when a tick first sees it (the
//! machine was off) is recorded as missed. A task-linked one fires late instead: the task is
//! still there to take on.
//!
//! State (`$FLEET_SCHEDULE_FILE`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/schedule.json`):
//! every pending entry, and the latest [`KEEP_DONE`] finished ones.

use std::path::PathBuf;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::core::harness::Harness;
use crate::core::tools;
use crate::error::{Error, Result};

/// A timed entry this late when a tick first sees it is missed, not run (as for bots).
pub const CATCH_UP_SECS: i64 = crate::core::jobs::CATCH_UP_SECS;
/// How often a pending task-linked entry re-reads its task.
pub const TASK_REFRESH_SECS: i64 = 3600;
/// Finished entries kept in the state file.
pub const KEEP_DONE: usize = 50;
/// When a task-linked entry fires on its start date, unless told otherwise.
pub const DEFAULT_TASK_TIME: &str = "08:00";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    #[default]
    Pending,
    Started,
    Skipped,
    Missed,
    Failed,
    Cancelled,
}

impl State {
    pub fn id(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Started => "started",
            State::Skipped => "skipped",
            State::Missed => "missed",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }
}

/// The task a task-linked entry follows, as last read.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLink {
    pub id: String,
    pub number: String,
    pub title: String,
    pub url: String,
    /// `YYYY-MM-DD`.
    pub start_date: Option<String>,
    /// ms epoch of the last read.
    pub checked_at: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    /// ms epoch it fires at; `None` for a task with no start date yet.
    pub at: Option<i64>,
    /// `HH:MM` on the start date (task-linked entries only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskLink>,
    pub dir: String,
    /// The first prompt (after the task's context line for a task-linked entry).
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub harness: Harness,
    pub gh_identity: Option<String>,
    pub created_at: i64,
    pub state: State,
    pub reason: Option<String>,
    pub fired_at: Option<i64>,
    pub tmux_session: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct File {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_SCHEDULE_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(tools::expand_tilde(&p.to_string_lossy()));
    }
    crate::core::jobs::state_dir()
        .parent()
        .map(|p| p.join("schedule.json"))
        .unwrap_or_else(|| PathBuf::from("schedule.json"))
}

pub fn load() -> File {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(f: &File) -> Result<()> {
    let path = path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = f.clone();
    f.version = 1;
    // Keep every pending entry and the newest finished ones.
    let done = f
        .entries
        .iter()
        .filter(|e| e.state != State::Pending)
        .count();
    let mut drop = done.saturating_sub(KEEP_DONE);
    f.entries.retain(|e| {
        if drop > 0 && e.state != State::Pending {
            drop -= 1;
            return false;
        }
        true
    });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&f)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load, change, save — short, so an `add` and a tick that overlap lose nothing.
pub fn update<T>(f: impl FnOnce(&mut File) -> T) -> Result<T> {
    let mut file = load();
    let out = f(&mut file);
    save(&file)?;
    Ok(out)
}

/// Apply `f` to the entry `id`, if it is still there.
pub fn update_entry(id: &str, f: impl FnOnce(&mut Entry)) -> Result<()> {
    update(|file| {
        if let Some(e) = file.entries.iter_mut().find(|e| e.id == id) {
            f(e);
        }
    })
}

/// A new entry id: `s` + the creation time in base 36 (short, sortable, unique enough for
/// one person adding entries by hand).
pub fn new_id(now_ms: i64, taken: &[Entry]) -> String {
    let mut n = now_ms.max(0) as u64;
    loop {
        let id = format!("s{}", base36(n));
        if !taken.iter().any(|e| e.id == id) {
            return id;
        }
        n += 1;
    }
}

fn base36(mut n: u64) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = Vec::new();
    loop {
        s.push(D[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

// --- times ---------------------------------------------------------------------

/// `HH:MM` (or `H:MM`).
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

fn local_at(d: NaiveDate, t: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&d.and_time(t)).earliest()
}

/// When a `--at` fires, from `now`: `YYYY-MM-DD HH:MM` (or with a `T`), `YYYY-MM-DD` (08:00),
/// `HH:MM` (the next one: today, else tomorrow), `today|tomorrow|<weekday> HH:MM`, or
/// `in 90m` / `in 2h` / `in 3d` (`+2h` too).
pub fn parse_at(s: &str, now: DateTime<Local>) -> std::result::Result<DateTime<Local>, String> {
    let s = s.trim().to_lowercase();
    let bad = || {
        format!(
            "can't read time '{s}' — use 'YYYY-MM-DD HH:MM', 'HH:MM', 'tomorrow HH:MM', 'mon HH:MM' or 'in 2h'"
        )
    };
    let rel = s
        .strip_prefix("in ")
        .or_else(|| s.strip_prefix('+'))
        .map(str::trim);
    if let Some(r) = rel {
        let (num, unit) = r.split_at(r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len()));
        let n: i64 = num.parse().map_err(|_| bad())?;
        let d = match unit.trim() {
            "m" | "min" | "mins" | "minutes" => Duration::minutes(n),
            "h" | "hour" | "hours" => Duration::hours(n),
            "d" | "day" | "days" => Duration::days(n),
            _ => return Err(bad()),
        };
        return Ok(now + d);
    }
    let default_time = parse_time(DEFAULT_TASK_TIME).unwrap_or_default();
    let split = s
        .split_once(' ')
        .or_else(|| (s.len() > 10 && s.as_bytes()[10] == b't').then(|| (&s[..10], &s[11..])));
    let (day, time) = match split {
        Some((d, t)) => (Some(d.trim()), parse_time(t).ok_or_else(bad)?),
        None => match parse_time(&s) {
            Some(t) => (None, t),
            None => (Some(s.as_str()), default_time),
        },
    };
    let today = now.date_naive();
    let date = match day {
        None => {
            let t = local_at(today, time).ok_or_else(bad)?;
            return Ok(if t > now {
                t
            } else {
                local_at(today + Duration::days(1), time).ok_or_else(bad)?
            });
        }
        Some("today") => today,
        Some("tomorrow") => today + Duration::days(1),
        Some(d) => match NaiveDate::parse_from_str(d, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                let wd: chrono::Weekday = d.parse().map_err(|_| bad())?;
                let ahead = (7 + wd.num_days_from_monday() as i64
                    - today.weekday().num_days_from_monday() as i64)
                    % 7;
                let ahead = if ahead == 0 { 7 } else { ahead };
                today + Duration::days(ahead)
            }
        },
    };
    local_at(date, time).ok_or_else(bad)
}

/// When a task-linked entry fires: `time` on `start_date`.
pub fn task_at(start_date: Option<&str>, time: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(start_date?, "%Y-%m-%d").ok()?;
    local_at(d, parse_time(time)?).map(|t| t.timestamp_millis())
}

// --- ticking -------------------------------------------------------------------

/// What a tick does with one entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Not yet.
    Wait,
    /// Re-read the task (it may have moved), then decide again.
    Refresh,
    /// Start the session now.
    Fire,
    /// Too late (the machine was off): record it, run nothing.
    Missed,
}

/// Pure: decide for one entry at `now` (ms). A task-linked entry about to fire is always
/// re-read first (`fresh`: it was just read in this tick).
pub fn decide(e: &Entry, now: i64, fresh: bool) -> Action {
    if e.state != State::Pending {
        return Action::Wait;
    }
    let stale = |t: &TaskLink| !fresh && now - t.checked_at > TASK_REFRESH_SECS * 1000;
    match (&e.task, e.at) {
        (Some(_), Some(at)) if at <= now => {
            if fresh {
                Action::Fire
            } else {
                Action::Refresh
            }
        }
        (Some(t), _) if stale(t) => Action::Refresh,
        (Some(_), _) => Action::Wait,
        (None, Some(at)) if at <= now => {
            if now - at > CATCH_UP_SECS * 1000 {
                Action::Missed
            } else {
                Action::Fire
            }
        }
        (None, _) => Action::Wait,
    }
}

/// The session name a task-linked entry gets when none was given: `task-<number>`.
pub fn default_name(e: &Entry) -> Option<String> {
    e.name.clone().or_else(|| {
        e.task
            .as_ref()
            .filter(|t| !t.number.is_empty())
            .map(|t| format!("task-{}", t.number))
    })
}

/// The entry `want` names: an exact id, else a unique prefix of one among the pending.
pub fn find<'a>(entries: &'a [Entry], want: &str) -> Result<&'a Entry> {
    let want = want.trim();
    if let Some(e) = entries.iter().find(|e| e.id == want) {
        return Ok(e);
    }
    let hits: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.state == State::Pending && e.id.starts_with(want))
        .collect();
    match hits.as_slice() {
        [e] => Ok(e),
        [] => Err(Error::Other(format!("no scheduled session '{want}'"))),
        many => Err(Error::Other(format!(
            "'{want}' matches several scheduled sessions: {}",
            many.iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn reads_the_ways_to_say_when() {
        // Tue 27.10.2026 10:00
        let now = at(2026, 10, 27, 10, 0);
        assert_eq!(
            parse_at("2026-10-29 07:30", now),
            Ok(at(2026, 10, 29, 7, 30))
        );
        assert_eq!(
            parse_at("2026-10-29T07:30", now),
            Ok(at(2026, 10, 29, 7, 30))
        );
        assert_eq!(parse_at("2026-10-29", now), Ok(at(2026, 10, 29, 8, 0)));
        assert_eq!(parse_at("11:00", now), Ok(at(2026, 10, 27, 11, 0)));
        assert_eq!(parse_at("09:00", now), Ok(at(2026, 10, 28, 9, 0)));
        assert_eq!(parse_at("tomorrow 7:00", now), Ok(at(2026, 10, 28, 7, 0)));
        assert_eq!(parse_at("mon 09:00", now), Ok(at(2026, 11, 2, 9, 0)));
        assert_eq!(parse_at("tue 09:00", now), Ok(at(2026, 11, 3, 9, 0)));
        assert_eq!(parse_at("in 2h", now), Ok(at(2026, 10, 27, 12, 0)));
        assert_eq!(parse_at("+90m", now), Ok(at(2026, 10, 27, 11, 30)));
        assert!(parse_at("soonish", now).is_err());
        assert!(parse_at("tomorrow 25:00", now).is_err());
    }

    fn timed(at: i64) -> Entry {
        Entry {
            id: "s1".into(),
            at: Some(at),
            ..Default::default()
        }
    }

    fn linked(at: Option<i64>, checked_at: i64) -> Entry {
        Entry {
            task: Some(TaskLink {
                id: "9".into(),
                checked_at,
                ..Default::default()
            }),
            at,
            ..timed(0)
        }
    }

    #[test]
    fn timed_entries_fire_once_due_and_are_missed_when_long_past() {
        let h = 3600 * 1000;
        assert_eq!(decide(&timed(10 * h), 9 * h, false), Action::Wait);
        assert_eq!(decide(&timed(10 * h), 10 * h, false), Action::Fire);
        assert_eq!(decide(&timed(10 * h), 15 * h, false), Action::Fire);
        assert_eq!(decide(&timed(10 * h), 17 * h, false), Action::Missed);
        let done = Entry {
            state: State::Started,
            ..timed(0)
        };
        assert_eq!(decide(&done, 10 * h, false), Action::Wait);
    }

    #[test]
    fn task_entries_reread_the_task_before_firing_and_hourly() {
        let h = 3600 * 1000;
        // Due: always re-read first, then fire — however late.
        assert_eq!(decide(&linked(Some(h), 0), 50 * h, false), Action::Refresh);
        assert_eq!(decide(&linked(Some(h), 50 * h), 50 * h, true), Action::Fire);
        // Not due: re-read when the last read is over an hour old.
        assert_eq!(
            decide(&linked(Some(90 * h), 49 * h), 50 * h, false),
            Action::Wait
        );
        assert_eq!(
            decide(&linked(Some(90 * h), 48 * h), 50 * h, false),
            Action::Refresh
        );
        // No start date: wait for one, re-reading hourly.
        assert_eq!(decide(&linked(None, 49 * h), 50 * h, false), Action::Wait);
        assert_eq!(
            decide(&linked(None, 48 * h), 50 * h, false),
            Action::Refresh
        );
        assert_eq!(decide(&linked(None, 50 * h), 50 * h, true), Action::Wait);
    }

    #[test]
    fn task_time_lands_on_the_start_date() {
        assert_eq!(
            task_at(Some("2026-10-29"), "08:30"),
            Some(at(2026, 10, 29, 8, 30).timestamp_millis())
        );
        assert_eq!(task_at(None, "08:30"), None);
        assert_eq!(task_at(Some("2026-10-29"), "8h"), None);
    }

    #[test]
    fn ids_do_not_collide() {
        let a = new_id(1_000, &[]);
        let taken = vec![Entry {
            id: a.clone(),
            ..Default::default()
        }];
        assert_ne!(new_id(1_000, &taken), a);
    }

    #[test]
    fn find_takes_an_id_or_a_unique_pending_prefix() {
        let e = |id: &str| Entry {
            id: id.into(),
            ..Default::default()
        };
        let all = vec![e("sabc"), e("sabd"), e("sx")];
        assert_eq!(find(&all, "sx").unwrap().id, "sx");
        assert_eq!(find(&all, "sabc").unwrap().id, "sabc");
        assert!(find(&all, "sab").is_err());
        assert!(find(&all, "zz").is_err());
    }
}
