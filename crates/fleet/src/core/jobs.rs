//! `fleet bots` (formerly `fleet jobs`): agent sessions started from a recipe — a prompt and a
//! cron schedule that spawn a session on this machine, the way a Claude Code routine runs in
//! the cloud. In code a bot is still a [`Job`]. Everything such a session spawns in turn
//! (`fleet spawn`, `fleet stack spawn`, a handoff) belongs to the same run.
//!
//! Config (`bots` in config.json, or `jobs`, their old name — per machine: a bot runs on the
//! host whose config lists it; read leniently, a bad bot is a problem and is skipped):
//!
//! ```json
//! "bots": {
//!   "triage": {
//!     "schedule": "0 7 * * 1-5",
//!     "dir": "~/Code/project",
//!     "promptFile": "~/notes/triage.md",
//!     "command": "claude --dangerously-skip-permissions",
//!     "model": "opus",
//!     "keep": "24h"
//!   }
//! }
//! ```
//!
//! `schedule` is a 5-field cron expression in local time (`*`, lists, ranges, `*/n`, names
//! `mon`/`jan`, and `@hourly` / `@daily` / `@weekly`). `prompt` (inline) or `promptFile` (read
//! at run time) is the first prompt. `command` replaces the launcher (config `claude`) for the
//! job's sessions *and* everything they spawn — it travels as `FLEET_CMD`. `harness` and
//! `model` as for `fleet spawn`. `ghIdentity` names a GitHub identity (`ghIdentities`) the
//! job's sessions act as on GitHub — see [`crate::core::gh_identity`]. `enabled: false` pauses
//! the job. `keep` (default `72h`, `"never"` to keep forever): a run's sessions are closed once
//! they are idle and the run is older than this; a session waiting for input is never closed.
//!
//! The launchd agent runs `fleet jobs tick` every minute. A tick fires a job when its latest
//! scheduled slot is newer than the last slot it saw; a slot more than [`CATCH_UP_SECS`] old
//! (the machine was off) is recorded as missed instead. A job never fires on the tick that
//! first sees it, and a run is skipped while any session of the job is still working.
//!
//! Tagging: a job session runs with `FLEET_JOB_RUN=<job>/<run>` in its environment and the
//! tmux pane option `@fleet_job_run` set to the same; `fleet list` reports it as `origin`.
//! A spawn made with `FLEET_JOB_RUN` set passes both on, so the whole tree is one run.
//!
//! State (`$FLEET_JOBS_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/jobs`):
//! `<job>.json` — the last slot seen and the latest [`KEEP_RUNS`] runs with their sessions;
//! `<job>/<run>.md` — the prompt each run started with.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::harness::Harness;
use crate::core::tools;
use crate::error::Result;

/// The env var (and, as `@fleet_job_run`, the tmux pane option) that tags a job's sessions.
pub const RUN_ENV: &str = "FLEET_JOB_RUN";
pub const PANE_OPTION: &str = "@fleet_job_run";
/// How often the launchd agent runs `fleet jobs tick`.
pub const TICK_SECS: u64 = 60;
/// A slot older than this when a tick first sees it is recorded as missed, not run.
pub const CATCH_UP_SECS: i64 = 6 * 3600;
/// Runs kept per job in its state file.
pub const KEEP_RUNS: usize = 50;
pub const DEFAULT_KEEP: &str = "72h";
/// How far back [`Cron::prev`] looks for a slot.
const LOOKBACK_MINUTES: i64 = 8 * 24 * 60;
/// How far ahead [`Cron::next`] looks.
const LOOKAHEAD_MINUTES: i64 = 366 * 24 * 60;

// --- cron ----------------------------------------------------------------------

/// A parsed 5-field cron expression (minute hour day-of-month month day-of-week).
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: Vec<bool>,
    hour: Vec<bool>,
    dom: Vec<bool>,
    month: Vec<bool>,
    dow: Vec<bool>,
    /// Classic cron: when both day fields are restricted, a day matching *either* counts.
    dom_any: bool,
    dow_any: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    pub fn parse(expr: &str) -> std::result::Result<Cron, String> {
        let expr = expr.trim();
        let expanded = match expr {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            other => other,
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!(
                "`{expr}`: a schedule has 5 fields (minute hour day month weekday), got {}",
                f.len()
            ));
        }
        let mut dow = field(f[4], 0, 7, &DAYS, "weekday")?;
        if dow[7] {
            dow[0] = true; // 7 is Sunday too
        }
        dow.truncate(7);
        Ok(Cron {
            minute: field(f[0], 0, 59, &[], "minute")?,
            hour: field(f[1], 0, 23, &[], "hour")?,
            dom: field(f[2], 1, 31, &[], "day")?,
            month: field(f[3], 1, 12, &MONTHS, "month")?,
            dow,
            dom_any: f[2] == "*",
            dow_any: f[4] == "*",
        })
    }

    pub fn matches<T: Datelike + Timelike>(&self, t: &T) -> bool {
        if !self.minute[t.minute() as usize]
            || !self.hour[t.hour() as usize]
            || !self.month[t.month() as usize]
        {
            return false;
        }
        let dom = self.dom[t.day() as usize];
        let dow = self.dow[t.weekday().num_days_from_sunday() as usize];
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }

    /// The latest slot at or before `now` (within about a week).
    pub fn prev(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = floor_minute(now);
        (0..LOOKBACK_MINUTES)
            .map(|m| start - Duration::minutes(m))
            .find(|t| self.matches(t))
    }

    /// The first slot after `now` (within a year).
    pub fn next(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = floor_minute(now);
        (1..LOOKAHEAD_MINUTES)
            .map(|m| start + Duration::minutes(m))
            .find(|t| self.matches(t))
    }
}

impl Cron {
    /// The schedule in plain English: "Every day at 07:00", "Weekdays at 07:00 and 13:00",
    /// "Every 15 minutes, 09:00–17:59, on Mon and Fri".
    pub fn describe(&self) -> String {
        let minutes = members(&self.minute, 0);
        let hours = members(&self.hour, 0);
        let all_hours = hours.len() == 24;
        let time = if minutes.len() == 60 {
            if all_hours {
                "every minute".to_string()
            } else {
                format!("every minute, {}", hour_span(&hours))
            }
        } else if all_hours {
            match (minutes.as_slice(), step_of(&minutes, 60)) {
                ([0], _) => "every hour".into(),
                ([m], _) => format!("every hour at :{m:02}"),
                (_, Some((0, st))) => format!("every {st} minutes"),
                _ => format!(
                    "every hour at {}",
                    list(
                        &minutes
                            .iter()
                            .map(|m| format!(":{m:02}"))
                            .collect::<Vec<_>>()
                    )
                ),
            }
        } else if minutes.len() * hours.len() <= 4 {
            let times: Vec<String> = hours
                .iter()
                .flat_map(|h| minutes.iter().map(move |m| format!("{h:02}:{m:02}")))
                .collect();
            format!("at {}", list(&times))
        } else {
            let at = match (minutes.as_slice(), step_of(&minutes, 60)) {
                ([m], _) => format!("every hour at :{m:02}"),
                (_, Some((0, st))) => format!("every {st} minutes"),
                _ => format!(
                    "at {} past the hour",
                    list(
                        &minutes
                            .iter()
                            .map(|m| format!(":{m:02}"))
                            .collect::<Vec<_>>()
                    )
                ),
            };
            format!("{at}, {}", hour_span(&hours))
        };
        let dows = members(&self.dow, 0);
        let doms = members(&self.dom, 1);
        let weekdays = || match dows.as_slice() {
            [1, 2, 3, 4, 5] => "weekdays".to_string(),
            [0, 6] => "weekends".to_string(),
            d => list(
                &d.iter()
                    .map(|&i| DAY_NAMES[i as usize].to_string())
                    .collect::<Vec<_>>(),
            ),
        };
        let month_days = || {
            format!(
                "the {} of the month",
                list(&doms.iter().map(|d| ordinal(*d)).collect::<Vec<_>>())
            )
        };
        let day = match (self.dom_any, self.dow_any) {
            (true, true) => None,
            (true, false) => Some(weekdays()),
            (false, true) => Some(month_days()),
            (false, false) => Some(format!("{} or on {}", month_days(), weekdays())),
        };
        let months = members(&self.month, 1);
        let month = (months.len() < 12).then(|| {
            format!(
                "in {}",
                list(
                    &months
                        .iter()
                        .map(|&m| MONTH_NAMES[m as usize - 1].to_string())
                        .collect::<Vec<_>>()
                )
            )
        });
        let mut out = match day {
            None => {
                let every_day = !time.starts_with("every");
                if every_day {
                    format!("Every day {time}")
                } else {
                    capitalize(&time)
                }
            }
            Some(d) if d == "weekdays" || d == "weekends" => format!("{} {time}", capitalize(&d)),
            Some(d) => format!("{}, on {d}", capitalize(&time)),
        };
        if let Some(m) = month {
            out.push_str(", ");
            out.push_str(&m);
        }
        out
    }
}

const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn members(set: &[bool], min: u32) -> Vec<u32> {
    (min..set.len() as u32)
        .filter(|&i| set[i as usize])
        .collect()
}

/// `(start, step)` when `v` is `start, start+step, …` up to the field's end (`size` values).
fn step_of(v: &[u32], size: u32) -> Option<(u32, u32)> {
    let (&a, &b) = (v.first()?, v.get(1)?);
    let st = b - a;
    let ok = v.windows(2).all(|w| w[1] - w[0] == st) && v.last()? + st >= size;
    ok.then_some((a, st))
}

/// "09:00–17:59" for a run of hours, else "at hours 9, 13 and 17".
fn hour_span(hours: &[u32]) -> String {
    let contiguous = hours.windows(2).all(|w| w[1] == w[0] + 1);
    match (hours.first(), hours.last()) {
        (Some(a), Some(b)) if contiguous => format!("{a:02}:00–{b:02}:59"),
        _ => format!(
            "during hours {}",
            list(&hours.iter().map(|h| format!("{h:02}")).collect::<Vec<_>>())
        ),
    }
}

fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn floor_minute(t: DateTime<Local>) -> DateTime<Local> {
    let ts = t.timestamp() - t.timestamp().rem_euclid(60);
    Local.timestamp_opt(ts, 0).single().unwrap_or(t)
}

/// One cron field → a membership table indexed by value (`0..=max`).
fn field(
    src: &str,
    min: u32,
    max: u32,
    names: &[&str],
    what: &str,
) -> std::result::Result<Vec<bool>, String> {
    let mut set = vec![false; max as usize + 1];
    let value = |s: &str| -> std::result::Result<u32, String> {
        let lower = s.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == lower) {
            // Month names count from 1, weekday names from 0.
            return Ok(i as u32 + if names.len() == 12 { 1 } else { 0 });
        }
        let n: u32 = s
            .parse()
            .map_err(|_| format!("{what} `{s}` is not a number"))?;
        if n < min || n > max {
            return Err(format!("{what} {n} is outside {min}-{max}"));
        }
        Ok(n)
    };
    for part in src.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("{what} step `{s}` must be a positive number"))?;
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            // `5/10` means 5, 15, 25 … like `5-max/10`.
            (v, if part.contains('/') { max } else { v })
        };
        if lo > hi {
            return Err(format!("{what} range `{range}` runs backwards"));
        }
        for v in (lo..=hi).step_by(step as usize) {
            set[v as usize] = true;
        }
    }
    Ok(set)
}

// --- config --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum PromptSrc {
    None,
    Inline(String),
    /// A `~/…` or absolute path, read when the job runs.
    File(String),
}

/// One configured job.
#[derive(Debug, Clone)]
pub struct Job {
    pub name: String,
    pub schedule: String,
    pub cron: Cron,
    pub dir: String,
    pub prompt: PromptSrc,
    pub harness: Harness,
    pub model: Option<String>,
    /// Launcher for the job's sessions and their descendants (`FLEET_CMD`).
    pub command: Option<String>,
    /// The GitHub identity its sessions act as (`ghIdentity`, see [`crate::core::gh_identity`]).
    pub gh_identity: Option<String>,
    pub enabled: bool,
    /// Seconds after a run starts that its idle sessions are closed; `None` = never.
    pub keep: Option<i64>,
    /// The config section it came from: `bots`, or the legacy `jobs`.
    pub section: &'static str,
}

/// The `bots` config section (plus the legacy `jobs` one), parsed.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub jobs: Vec<Job>,
    pub problems: Vec<String>,
}

impl Settings {
    /// One section (`bots` or `jobs`) of the config.
    pub fn from_value(v: &Value, section: &'static str) -> Settings {
        let mut s = Settings::default();
        let map = match v {
            Value::Null => return s,
            Value::Object(m) => m,
            other => {
                s.problems.push(format!(
                    "{section} must be an object of name → bot, not {other}"
                ));
                return s;
            }
        };
        // `fleet config set bots.<name> null` removes a bot.
        for (name, raw) in map.iter().filter(|(_, v)| !v.is_null()) {
            match job_from_value(name, raw) {
                Ok(j) => s.jobs.push(Job { section, ..j }),
                Err(e) => s.problems.push(format!("{section}.{name}: {e}")),
            }
        }
        s
    }

    /// The whole config: `bots`, then `jobs` — a name in both is the `bots` one.
    pub fn from_config(raw: &Value) -> Settings {
        let mut s = Settings::from_value(&raw["bots"], "bots");
        let legacy = Settings::from_value(&raw["jobs"], "jobs");
        s.problems.extend(legacy.problems);
        for j in legacy.jobs {
            if s.get(&j.name).is_some() {
                s.problems.push(format!(
                    "jobs.{}: also in bots — the bots one is used",
                    j.name
                ));
            } else {
                s.jobs.push(j);
            }
        }
        s
    }

    pub fn load() -> Settings {
        Settings::from_config(&crate::core::config::load().raw)
    }

    pub fn get(&self, name: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.name == name)
    }
}

/// A job name is a file name and part of a tmux session name: keep it tame.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn job_from_value(name: &str, v: &Value) -> std::result::Result<Job, String> {
    if !valid_name(name) {
        return Err("a job name is 1-40 of [A-Za-z0-9_-]".into());
    }
    let o = v.as_object().ok_or("a job is an object")?;
    let text = |key: &str| -> std::result::Result<Option<String>, String> {
        match o.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
            Some(other) => Err(format!("{key} must be a string, not {other}")),
        }
    };
    let schedule = text("schedule")?.ok_or("schedule is required")?;
    let cron = Cron::parse(&schedule)?;
    let dir = text("dir")?.ok_or("dir is required")?;
    let prompt = match (o.get("prompt"), text("promptFile")?) {
        (Some(Value::String(p)), None) if !p.trim().is_empty() => PromptSrc::Inline(p.clone()),
        (None | Some(Value::Null), Some(f)) => PromptSrc::File(f),
        (None | Some(Value::Null), None) => PromptSrc::None,
        (Some(Value::String(p)), None) if p.trim().is_empty() => PromptSrc::None,
        (Some(Value::String(_)), Some(_)) => {
            return Err("set prompt or promptFile, not both".into());
        }
        (Some(other), _) => return Err(format!("prompt must be a string, not {other}")),
    };
    let harness = match text("harness")? {
        None => Harness::Claude,
        Some(h) => Harness::parse(&h).ok_or_else(|| format!("unknown harness `{h}`"))?,
    };
    let enabled = match o.get("enabled") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => return Err(format!("enabled must be true or false, not {other}")),
    };
    let gh_identity = text("ghIdentity")?;
    if let Some(g) = &gh_identity
        && !crate::core::gh_identity::valid_name(g)
    {
        return Err(format!("ghIdentity `{g}` is not a valid name"));
    }
    let keep = match text("keep")?.as_deref().unwrap_or(DEFAULT_KEEP) {
        "never" => None,
        k => Some(crate::core::tmux::parse_span(k).map_err(|e| format!("keep: {e}"))?),
    };
    Ok(Job {
        name: name.to_string(),
        schedule,
        cron,
        dir,
        prompt,
        harness,
        model: text("model")?,
        command: text("command")?,
        gh_identity,
        enabled,
        keep,
        section: "bots",
    })
}

// --- tags ----------------------------------------------------------------------

/// What a job session carries into its environment, and passes on to what it spawns.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    /// `<job>/<run>`.
    pub run: String,
    /// The launcher (`FLEET_CMD`), when the job sets one.
    pub command: Option<String>,
}

impl Tag {
    /// The tag of the job run this process belongs to, if any.
    pub fn from_env() -> Option<Tag> {
        let run = std::env::var(RUN_ENV)
            .ok()
            .filter(|r| split_run(r).is_some())?;
        Some(Tag {
            run,
            command: std::env::var("FLEET_CMD")
                .ok()
                .filter(|c| !c.trim().is_empty()),
        })
    }

    /// `KEY=value` pairs for the new session's environment.
    pub fn env(&self) -> Vec<String> {
        let mut e = vec![format!("{RUN_ENV}={}", self.run)];
        if let Some(c) = &self.command {
            e.push(format!("FLEET_CMD={c}"));
        }
        e
    }
}

/// `<job>/<run>` → (job, run).
pub fn split_run(tag: &str) -> Option<(&str, &str)> {
    let (job, run) = tag.split_once('/')?;
    (valid_name(job) && !run.is_empty()).then_some((job, run))
}

/// `origin` on a `fleet list` row: the job run a session belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub job: String,
    pub run: String,
}

impl Origin {
    pub fn parse(tag: &str) -> Option<Origin> {
        split_run(tag.trim()).map(|(job, run)| Origin {
            job: job.into(),
            run: run.into(),
        })
    }
}

// --- state ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JobState {
    pub version: u32,
    /// Milliseconds of the last schedule slot a tick saw (fired, skipped or missed).
    pub last_slot: Option<i64>,
    /// Newest last.
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    pub id: String,
    /// `schedule` | `manual`.
    pub trigger: String,
    pub started_at: i64,
    /// The schedule slot it ran for (scheduled runs).
    pub slot: Option<i64>,
    /// `spawned` | `skipped` | `missed` | `failed`.
    pub outcome: String,
    pub reason: Option<String>,
    /// The run's first prompt, as it was sent.
    pub prompt_file: Option<String>,
    pub sessions: Vec<RunSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RunSession {
    /// The tmux session it got (created for it, so closing it closes only this session).
    pub tmux_session: String,
    pub name: Option<String>,
    /// The tmux session of the job session that spawned it; `None` for the run's first.
    pub parent: Option<String>,
    pub at: i64,
    /// When `jobs tick` closed it (or found it gone).
    pub closed_at: Option<i64>,
}

pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_JOBS_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(tools::expand_tilde(&p.to_string_lossy()));
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"));
    base.join("fleet").join("jobs")
}

pub fn state_path(job: &str) -> PathBuf {
    state_dir().join(format!("{job}.json"))
}

pub fn prompt_path(job: &str, run: &str) -> PathBuf {
    state_dir().join(job).join(format!("{run}.md"))
}

pub fn load_state(job: &str) -> JobState {
    load_state_at(&state_path(job))
}

pub fn load_state_at(path: &Path) -> JobState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_state(job: &str, st: &JobState) -> Result<()> {
    let path = state_path(job);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut st = st.clone();
    st.version = 1;
    let drop = st.runs.len().saturating_sub(KEEP_RUNS);
    st.runs.drain(..drop);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&st)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Note a session a job run spawned (the run's first, or one a job session spawned).
/// Never fails the spawn: a run that can't be recorded is still tagged.
pub fn record_session(tag: &Tag, s: RunSession) {
    let Some((job, run)) = split_run(&tag.run) else {
        return;
    };
    let mut st = load_state(job);
    match st.runs.iter_mut().rev().find(|r| r.id == run) {
        Some(r) => r.sessions.push(s),
        // A run this host has no record of (spawned here from another host's job).
        None => st.runs.push(Run {
            id: run.into(),
            trigger: "remote".into(),
            started_at: s.at,
            outcome: "spawned".into(),
            sessions: vec![s],
            ..Default::default()
        }),
    }
    if let Err(e) = save_state(job, &st) {
        crate::core::hosts::debug(&format!("jobs: could not record a session of {job}: {e}"));
    }
}

// --- scheduling ----------------------------------------------------------------

/// What a tick does with a job.
#[derive(Debug, Clone, PartialEq)]
pub enum Due {
    /// Nothing new since the last slot it saw.
    Nothing,
    /// First sighting (or paused): remember where we are, run nothing.
    Arm,
    /// Run for this slot.
    Fire(i64),
    /// The slot is too old (the machine was off): record it, run nothing.
    Missed(i64),
}

/// Pure: decide for one job at `now`, and the `last_slot` to store afterwards.
pub fn due(cron: &Cron, enabled: bool, last_slot: Option<i64>, now: DateTime<Local>) -> (Due, i64) {
    let now_ms = now.timestamp_millis();
    let slot = cron.prev(now).map(|t| t.timestamp_millis());
    let Some(last) = last_slot.filter(|_| enabled) else {
        // Arm at `now`, not at the slot: a slot that comes after this tick fires.
        return (Due::Arm, last_slot.map_or(now_ms, |l| l.max(now_ms)));
    };
    match slot {
        Some(s) if s > last => {
            if now_ms - s > CATCH_UP_SECS * 1000 {
                (Due::Missed(s), s)
            } else {
                (Due::Fire(s), s)
            }
        }
        _ => (Due::Nothing, last),
    }
}

/// A run id for a start at `t`: `YYYYMMDD-HHMM` for a slot, plus seconds for a manual run.
pub fn run_id(t: DateTime<Local>, manual: bool) -> String {
    if manual {
        t.format("%Y%m%d-%H%M%S").to_string()
    } else {
        t.format("%Y%m%d-%H%M").to_string()
    }
}

/// The name a run's first session gets: `<job>-<run id without the year>`
/// (`triage-1013-0700`).
pub fn session_name(job: &str, run: &str) -> String {
    format!("{job}-{}", run.get(4..).unwrap_or(run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn cron_fields() {
        let c = Cron::parse("0 7 * * 1-5").unwrap();
        assert!(c.matches(&at(2026, 10, 14, 7, 0))); // Wednesday
        assert!(!c.matches(&at(2026, 10, 18, 7, 0))); // Sunday
        assert!(!c.matches(&at(2026, 10, 14, 7, 1)));
        let c = Cron::parse("*/15 9-17 * * mon,fri").unwrap();
        assert!(c.matches(&at(2026, 10, 12, 9, 45))); // Monday
        assert!(!c.matches(&at(2026, 10, 13, 9, 45))); // Tuesday
        let c = Cron::parse("30 6 1 jan *").unwrap();
        assert!(c.matches(&at(2027, 1, 1, 6, 30)));
        assert!(
            Cron::parse("0 0 * * 7")
                .unwrap()
                .matches(&at(2026, 10, 18, 0, 0))
        );
        assert_eq!(
            Cron::parse("@daily").unwrap(),
            Cron::parse("0 0 * * *").unwrap()
        );
        // Both day fields restricted: either matches.
        let c = Cron::parse("0 0 13 * fri").unwrap();
        assert!(c.matches(&at(2026, 10, 13, 0, 0))); // the 13th, a Tuesday
        assert!(c.matches(&at(2026, 10, 16, 0, 0))); // a Friday
        assert!(!c.matches(&at(2026, 10, 15, 0, 0)));
        let c = Cron::parse("5/20 * * * *").unwrap();
        assert!(c.matches(&at(2026, 10, 13, 1, 45)));
        assert!(!c.matches(&at(2026, 10, 13, 1, 50)));
    }

    #[test]
    fn cron_in_plain_english() {
        let d = |e: &str| Cron::parse(e).unwrap().describe();
        assert_eq!(d("0 7 * * *"), "Every day at 07:00");
        assert_eq!(d("0 7 * * 1-5"), "Weekdays at 07:00");
        assert_eq!(d("30 6 * * sat,sun"), "Weekends at 06:30");
        assert_eq!(d("0 7,13 * * *"), "Every day at 07:00 and 13:00");
        assert_eq!(d("0 9 * * mon,fri"), "At 09:00, on Mon and Fri");
        assert_eq!(d("* * * * *"), "Every minute");
        assert_eq!(d("@hourly"), "Every hour");
        assert_eq!(d("15 * * * *"), "Every hour at :15");
        assert_eq!(d("*/15 * * * *"), "Every 15 minutes");
        assert_eq!(
            d("*/15 9-17 * * 1-5"),
            "Weekdays every 15 minutes, 09:00–17:59"
        );
        assert_eq!(d("0 0 1 * *"), "At 00:00, on the 1st of the month");
        assert_eq!(
            d("0 8 1,15 jan,jul *"),
            "At 08:00, on the 1st and 15th of the month, in Jan and Jul"
        );
        assert_eq!(
            d("0 0 13 * fri"),
            "At 00:00, on the 13th of the month or on Fri"
        );
    }

    #[test]
    fn cron_rejects_garbage() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* * * * mon-sun-x",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cron_prev_and_next() {
        let c = Cron::parse("0 7 * * *").unwrap();
        assert_eq!(
            c.prev(at(2026, 10, 13, 9, 12)),
            Some(at(2026, 10, 13, 7, 0))
        );
        assert_eq!(c.prev(at(2026, 10, 13, 7, 0)), Some(at(2026, 10, 13, 7, 0)));
        assert_eq!(
            c.prev(at(2026, 10, 13, 6, 59)),
            Some(at(2026, 10, 12, 7, 0))
        );
        assert_eq!(c.next(at(2026, 10, 13, 7, 0)), Some(at(2026, 10, 14, 7, 0)));
        assert_eq!(
            c.next(at(2026, 10, 13, 6, 59)),
            Some(at(2026, 10, 13, 7, 0))
        );
    }

    #[test]
    fn due_arms_then_fires_once_per_slot() {
        let c = Cron::parse("0 7 * * *").unwrap();
        let t0 = at(2026, 10, 13, 6, 50);
        let (d, last) = due(&c, true, None, t0);
        assert_eq!(d, Due::Arm);
        assert_eq!(last, t0.timestamp_millis());
        // Same day, before the slot: nothing.
        assert_eq!(
            due(&c, true, Some(last), at(2026, 10, 13, 6, 59)).0,
            Due::Nothing
        );
        let slot = at(2026, 10, 13, 7, 0).timestamp_millis();
        let (d, last) = due(&c, true, Some(last), at(2026, 10, 13, 7, 0));
        assert_eq!(d, Due::Fire(slot));
        assert_eq!(
            due(&c, true, Some(last), at(2026, 10, 13, 7, 1)).0,
            Due::Nothing
        );
        // A job added after today's slot waits for tomorrow's.
        let (_, armed) = due(&c, true, None, at(2026, 10, 13, 8, 0));
        assert_eq!(
            due(&c, true, Some(armed), at(2026, 10, 13, 8, 1)).0,
            Due::Nothing
        );
    }

    #[test]
    fn due_catches_up_a_little_and_skips_the_rest() {
        let c = Cron::parse("0 7 * * *").unwrap();
        let yesterday = at(2026, 10, 12, 7, 0).timestamp_millis();
        let slot = at(2026, 10, 13, 7, 0).timestamp_millis();
        assert_eq!(
            due(&c, true, Some(yesterday), at(2026, 10, 13, 9, 0)).0,
            Due::Fire(slot)
        );
        assert_eq!(
            due(&c, true, Some(yesterday), at(2026, 10, 13, 14, 0)).0,
            Due::Missed(slot)
        );
    }

    #[test]
    fn due_paused_keeps_arming() {
        let c = Cron::parse("0 7 * * *").unwrap();
        let yesterday = at(2026, 10, 12, 7, 0).timestamp_millis();
        let now = at(2026, 10, 13, 7, 30);
        let (d, last) = due(&c, false, Some(yesterday), now);
        assert_eq!(d, Due::Arm);
        // Re-enabled right after: today's slot is behind it, so it does not fire.
        assert_eq!(
            due(&c, true, Some(last), at(2026, 10, 13, 7, 31)).0,
            Due::Nothing
        );
    }

    #[test]
    fn settings_parse_leniently() {
        let s = Settings::from_value(
            &json!({
            "triage": {
                "schedule": "0 7 * * 1-5",
                "dir": "~/Code/project",
                "promptFile": "~/notes/triage.md",
                "command": "claude --dangerously-skip-permissions",
                "model": "opus"
            },
            "paused": { "schedule": "@hourly", "dir": "/tmp", "prompt": "hi", "enabled": false, "keep": "never" },
            "bad cron": { "schedule": "0 7 * *", "dir": "/tmp" },
            "nodir": { "schedule": "@daily" },
            "both": { "schedule": "@daily", "dir": "/tmp", "prompt": "a", "promptFile": "b" },
            "removed": null
            }),
            "bots",
        );
        assert_eq!(s.jobs.len(), 2, "{:?}", s.problems);
        assert_eq!(s.problems.len(), 3);
        let t = s.get("triage").unwrap();
        assert_eq!(t.prompt, PromptSrc::File("~/notes/triage.md".into()));
        assert_eq!(t.keep, Some(259_200));
        assert!(t.enabled);
        assert_eq!(t.model.as_deref(), Some("opus"));
        let p = s.get("paused").unwrap();
        assert!(!p.enabled);
        assert_eq!(p.keep, None);
        assert_eq!(p.prompt, PromptSrc::Inline("hi".into()));
        assert!(
            Settings::from_value(&Value::Null, "bots")
                .problems
                .is_empty()
        );
        assert_eq!(Settings::from_value(&json!([1]), "bots").problems.len(), 1);
    }

    #[test]
    fn settings_read_bots_and_legacy_jobs() {
        let bot = json!({ "schedule": "@daily", "dir": "/tmp" });
        let s = Settings::from_config(&json!({
            "bots": { "triage": bot, "both": bot },
            "jobs": { "old": bot, "both": bot }
        }));
        let sections: Vec<_> = ["triage", "old", "both"]
            .iter()
            .map(|n| s.get(n).unwrap().section)
            .collect();
        assert_eq!(sections, ["bots", "jobs", "bots"]);
        assert_eq!(s.jobs.len(), 3);
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
    }

    #[test]
    fn tags_round_trip() {
        let t = Tag {
            run: "triage/20261013-0700".into(),
            command: Some("claude --dangerously-skip-permissions".into()),
        };
        assert_eq!(
            t.env(),
            [
                "FLEET_JOB_RUN=triage/20261013-0700",
                "FLEET_CMD=claude --dangerously-skip-permissions"
            ]
        );
        assert_eq!(
            Origin::parse("triage/20261013-0700"),
            Some(Origin {
                job: "triage".into(),
                run: "20261013-0700".into()
            })
        );
        assert_eq!(Origin::parse(""), None);
        assert_eq!(Origin::parse("no-run/"), None);
        assert_eq!(Origin::parse("bad name/x"), None);
    }

    #[test]
    fn names_and_ids() {
        let t = at(2026, 10, 13, 7, 0);
        assert_eq!(run_id(t, false), "20261013-0700");
        assert_eq!(run_id(t, true), "20261013-070000");
        assert_eq!(
            session_name("triage", &run_id(t, false)),
            "triage-1013-0700"
        );
        assert_eq!(
            session_name("triage", &run_id(t, true)),
            "triage-1013-070000"
        );
    }
}
