//! Codex rate limits — the numbers Codex's `/status` shows (5-hour and weekly
//! windows, credits), read from Codex's own transcripts. No network call.
//!
//! **Source.** Every turn Codex appends a `token_count` event to its rollout
//! (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`, default `~/.codex`) carrying
//! `rate_limits: { limit_id, limit_name, primary, secondary, credits, plan_type, … }`
//! (`null` for logins without subscription limits, such as an API key). A window is
//! `{ used_percent, window_minutes, resets_at }` (`resets_at` in epoch seconds; older
//! Codex wrote `resets_in_seconds`). Each `limit_id` is its own bucket: `codex` (or no
//! id, from older Codex) is the main one; others are scoped limits (a model) or
//! credits-only (`premium`, no windows). The newest snapshot of each bucket counts
//! (a scoped one only within [`SLACK_MS`] of the main one, or newer), so
//! the numbers are as fresh as the last Codex turn on this machine: `observed_at` says
//! how old.
//!
//! **Finding them.** Rollouts are walked (one `stat` each) and read newest mtime first,
//! backwards in chunks, newest line first. *Assumption*: a file's mtime is at least the
//! time of its last event (Codex appends and never rewrites), so once a file's mtime is
//! older than the main snapshot found ([`SLACK_MS`] slack) no later file can hold a newer
//! one and the scan stops — usually after the first few files. Bounds per call: at most
//! [`MAX_FILES`] files and [`MAX_BYTES`] bytes, and [`MAX_LIMITLESS_FILES`] files whose
//! events record no limits at all. Nothing is cached: an uncached scan costs a few ms.
//!
//! Windows that have reset (`resets_at` passed, or `observed_at + window_minutes` when
//! there's no reset time) are reported at 0% with no reset time.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::core::usage::{Credits, Limit, ProviderUsage, severity_for};

/// Read at most this many rollouts per call.
pub const MAX_FILES: usize = 64;
/// Read at most this many bytes per call, across all rollouts.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Give up after this many rollouts whose `token_count` events all have
/// `rate_limits: null` (a login with no subscription limits), when nothing was found.
pub const MAX_LIMITLESS_FILES: usize = 8;
/// mtime vs event-timestamp slack for the early stop.
pub const SLACK_MS: i64 = 60_000;
/// Read size when walking a file backwards.
const CHUNK: u64 = 256 * 1024;
/// A line is dropped once more than this much of it is held, so lines up to about
/// `MAX_LINE + CHUNK` (~1.25 MiB) come back and longer ones are skipped (a `token_count`
/// line is a few hundred bytes).
const MAX_LINE: usize = 1024 * 1024;

/// One `rate_limits` object and when it was recorded.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    at: DateTime<Utc>,
    rate_limits: Value,
}

/// The newest snapshots of one `limit_id`.
#[derive(Debug, Default)]
struct Bucket {
    newest: Option<Snapshot>,
    /// Newest with a window.
    windowed: Option<Snapshot>,
    /// Newest *without* a window but with a `credits` object (`premium`): a windowed
    /// snapshot's credits are only the main fallback.
    credited: Option<Snapshot>,
}

fn keep_newer(slot: &mut Option<Snapshot>, s: &Snapshot) {
    if slot.as_ref().is_none_or(|old| s.at > old.at) {
        *slot = Some(s.clone());
    }
}

/// What a scan found.
#[derive(Debug, Default)]
struct Found {
    /// By `limit_id`; `""` for events without one (older Codex: the main bucket).
    buckets: BTreeMap<String, Bucket>,
    /// Rollouts whose `token_count` events had no rate limits at all.
    limitless_files: usize,
}

fn is_main(id: &str) -> bool {
    id.is_empty() || id == "codex"
}

impl Found {
    fn add(&mut self, s: Snapshot) {
        let id = s.rate_limits["limit_id"].as_str().unwrap_or("").to_string();
        let b = self.buckets.entry(id).or_default();
        if has_window(&s.rate_limits) {
            keep_newer(&mut b.windowed, &s);
        } else if s.rate_limits["credits"].is_object() {
            keep_newer(&mut b.credited, &s);
        }
        keep_newer(&mut b.newest, &s);
    }

    /// The main bucket's newest windowed snapshot (`codex`, or no id).
    fn main(&self) -> Option<&Snapshot> {
        self.buckets
            .iter()
            .filter(|(id, _)| is_main(id))
            .filter_map(|(_, b)| b.windowed.as_ref())
            .max_by_key(|s| s.at)
    }

    /// The main snapshot's time less [`SLACK_MS`]: scoped snapshots older than this
    /// don't count, and reading stops there once credits are known.
    fn horizon(&self) -> Option<DateTime<Utc>> {
        self.main()
            .map(|s| s.at - chrono::Duration::milliseconds(SLACK_MS))
    }

    /// The newest windowless credits (`premium`, …), from any bucket.
    fn side_credits(&self) -> Option<&Snapshot> {
        self.buckets
            .values()
            .filter_map(|b| b.credited.as_ref())
            .max_by_key(|s| s.at)
    }

    /// Scoped buckets' windowed snapshots, by id — none older than the horizon.
    fn scoped(&self) -> impl Iterator<Item = &Snapshot> {
        let horizon = self.horizon();
        self.buckets
            .iter()
            .filter(|(id, _)| !is_main(id))
            .filter_map(|(_, b)| b.windowed.as_ref())
            .filter(move |s| horizon.is_none_or(|h| s.at >= h))
    }
}

/// Codex's limits on this machine (`home` is `$CODEX_HOME`).
pub fn get(home: &Path) -> ProviderUsage {
    get_at(home, Utc::now())
}

fn get_at(home: &Path, now: DateTime<Utc>) -> ProviderUsage {
    let mut out = ProviderUsage {
        id: "codex".into(),
        label: "Codex".into(),
        status: "no_data".into(),
        plan: None,
        limits: Vec::new(),
        fetched_at: Some(now.to_rfc3339()),
        observed_at: None,
        error: None,
        credits: None,
    };
    if !home.is_dir() {
        out.status = "not_installed".into();
        out.fetched_at = None;
        out.error = Some("Codex is not installed (no Codex home directory)".into());
        return out;
    }
    let files = match rollouts(&home.join("sessions")) {
        Ok(f) => f,
        Err(e) => {
            out.status = "error".into();
            out.error = Some(e);
            return out;
        }
    };
    if files.is_empty() {
        out.error = Some("no Codex sessions yet".into());
        return out;
    }
    fill(&mut out, &scan(&files), now);
    out
}

fn read_dir(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    out.sort();
    Ok(out)
}

/// Every regular `rollout-*.jsonl` file under `sessions/YYYY/MM/DD/`, newest mtime
/// first. A missing `sessions` is no sessions; any other error reading it is an
/// error. An unreadable year, month or day directory is skipped.
fn rollouts(sessions: &Path) -> Result<Vec<(PathBuf, i64)>, String> {
    let years = match read_dir(sessions) {
        Ok(y) => y,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("can't read Codex sessions: {e}")),
    };
    let dirs = |p: &Path| read_dir(p).unwrap_or_default().into_iter();
    let mut out = Vec::new();
    for f in years
        .into_iter()
        .flat_map(|y| dirs(&y))
        .flat_map(|m| dirs(&m))
        .flat_map(|d| dirs(&d))
    {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
            continue;
        }
        // `metadata` follows symlinks; a FIFO or directory is never opened.
        let Ok(meta) = std::fs::metadata(&f) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if let (true, Some(m)) = (meta.is_file(), mtime) {
            out.push((f, m));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    Ok(out)
}

/// Newest mtime first, within the bounds in the module docs.
fn scan(files: &[(PathBuf, i64)]) -> Found {
    let mut found = Found::default();
    let mut budget = MAX_BYTES;
    for (path, mtime) in files.iter().take(MAX_FILES) {
        if budget == 0 {
            break;
        }
        if let Some(main) = found.main()
            && mtime + SLACK_MS < main.at.timestamp_millis()
        {
            break;
        }
        if found.buckets.is_empty() && found.limitless_files >= MAX_LIMITLESS_FILES {
            break;
        }
        let (mut events, mut limited) = (false, false);
        read_back(path, &mut budget, |line| {
            let Some((at, rl)) = token_count(line) else {
                return true;
            };
            events = true;
            if !rl.is_object() {
                return true;
            }
            limited = true;
            // Past the horizon, with credits known: nothing older can count.
            if found.horizon().is_some_and(|h| at < h) && found.side_credits().is_some() {
                return false;
            }
            found.add(Snapshot {
                at,
                rate_limits: rl,
            });
            true
        });
        if events && !limited {
            found.limitless_files += 1;
        }
    }
    found
}

/// A `token_count` event's time and `rate_limits` (maybe `null`).
fn token_count(line: &[u8]) -> Option<(DateTime<Utc>, Value)> {
    let text = std::str::from_utf8(line).ok()?;
    if !text.contains("\"token_count\"") {
        return None;
    }
    let mut v: Value = serde_json::from_str(text).ok()?; // torn or malformed: skipped
    if v["payload"]["type"].as_str() != Some("token_count") {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
        .ok()?
        .with_timezone(&Utc);
    Some((at, v["payload"]["rate_limits"].take()))
}

/// Feed `f` the file's lines, last first, reading backwards in [`CHUNK`]s and taking
/// from `budget`; stop when `f` returns false, at the start or out of budget. Empty lines
/// are skipped, and so are lines longer than about [`MAX_LINE`] + [`CHUNK`].
fn read_back(path: &Path, budget: &mut u64, mut f: impl FnMut(&[u8]) -> bool) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let Ok(mut pos) = file.metadata().map(|m| m.len()) else {
        return;
    };
    // The line that starts before `pos` and runs to the last line read, so far.
    let mut carry: Vec<u8> = Vec::new();
    let mut oversized = false;
    while pos > 0 && *budget > 0 {
        let n = CHUNK.min(pos).min(*budget);
        pos -= n;
        *budget -= n;
        let mut buf = Vec::with_capacity(n as usize);
        if file.seek(SeekFrom::Start(pos)).is_err()
            || (&mut file).take(n).read_to_end(&mut buf).is_err()
            || buf.len() as u64 != n
        {
            return;
        }
        let mut end = buf.len();
        while let Some(i) = buf[..end].iter().rposition(|&b| b == b'\n') {
            if !oversized && (i + 1 < end || !carry.is_empty()) {
                let go = if carry.is_empty() {
                    f(&buf[i + 1..end])
                } else {
                    let mut line = buf[i + 1..end].to_vec();
                    line.extend_from_slice(&carry);
                    f(&line)
                };
                if !go {
                    return;
                }
            }
            carry.clear();
            oversized = false;
            end = i;
        }
        if !oversized {
            if carry.len() + end > MAX_LINE {
                oversized = true;
                carry.clear();
            } else {
                let mut c = buf[..end].to_vec();
                c.extend_from_slice(&carry);
                carry = c;
            }
        }
    }
    if pos == 0 && !oversized && !carry.is_empty() {
        f(&carry);
    }
}

fn has_window(rl: &Value) -> bool {
    ["primary", "secondary"]
        .iter()
        .any(|k| rl[*k]["used_percent"].is_number())
}

fn fill(out: &mut ProviderUsage, found: &Found, now: DateTime<Utc>) {
    let main = found.main();
    let plan_of = |s: &Snapshot| s.rate_limits["plan_type"].as_str().map(plan_name);
    out.plan = main.and_then(plan_of).or_else(|| {
        found
            .buckets
            .values()
            .filter_map(|b| b.newest.as_ref())
            .filter(|s| plan_of(s).is_some())
            .max_by_key(|s| s.at)
            .and_then(plan_of)
    });
    out.credits = found
        .side_credits()
        .or(main)
        .and_then(|s| credits(&s.rate_limits["credits"]));
    if let Some(m) = main {
        out.limits.extend(limits(m, None, now));
    }
    for s in found.scoped() {
        let rl = &s.rate_limits;
        let id = rl["limit_id"].as_str().unwrap_or("");
        let name = rl["limit_name"].as_str().unwrap_or(id).to_string();
        out.limits.extend(limits(s, Some(name), now));
    }
    if let Some(top) = out
        .limits
        .iter_mut()
        .filter(|l| l.model.is_none() && l.percent > 0.0)
        .max_by(|a, b| a.percent.total_cmp(&b.percent))
    {
        top.active = true;
    }
    let observed = main.or_else(|| found.scoped().max_by_key(|s| s.at));
    match observed {
        Some(s) => {
            out.status = "ok".into();
            out.observed_at = Some(s.at.to_rfc3339());
        }
        None => {
            out.status = "no_data".into();
            out.error = Some(if !found.buckets.is_empty() {
                "Codex has recorded credits but no rate-limit windows yet".into()
            } else if found.limitless_files > 0 {
                "Codex records no rate limits for this login (an API key?)".into()
            } else {
                "no Codex rate limits recorded yet".into()
            });
        }
    }
}

/// `plus` → `Plus`.
fn plan_name(p: &str) -> String {
    let mut c = p.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn credits(c: &Value) -> Option<Credits> {
    c.is_object().then(|| Credits {
        has_credits: c["has_credits"].as_bool().unwrap_or(false),
        unlimited: c["unlimited"].as_bool().unwrap_or(false),
        balance: match &c["balance"] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        },
    })
}

/// A snapshot's `primary` and `secondary` windows; `model` names a scoped bucket.
fn limits(s: &Snapshot, model: Option<String>, now: DateTime<Utc>) -> Vec<Limit> {
    let rl = &s.rate_limits;
    let observed = s.at.timestamp();
    [("primary", false), ("secondary", true)]
        .iter()
        .filter_map(|(key, long)| {
            let w = &rl[*key];
            let used = w["used_percent"].as_f64()?;
            let minutes = w["window_minutes"].as_i64().filter(|m| *m > 0);
            let resets = w["resets_at"]
                .as_i64()
                .or_else(|| w["resets_in_seconds"].as_i64().map(|x| observed + x));
            let reset = match (resets, minutes) {
                (Some(r), _) => r <= now.timestamp(),
                (None, Some(m)) => observed + m * 60 <= now.timestamp(),
                (None, None) => false,
            };
            let percent = if reset { 0.0 } else { used };
            let weekly = minutes.map_or(*long, |m| m > 24 * 60);
            let (group, kind) = match (weekly, model.is_some()) {
                (false, false) => ("session", "session"),
                (false, true) => ("session", "session_scoped"),
                (true, false) => ("weekly", "weekly_all"),
                (true, true) => ("weekly", "weekly_scoped"),
            };
            let name = window_name(minutes, weekly);
            Some(Limit {
                kind: kind.into(),
                group: group.into(),
                label: match &model {
                    Some(m) => format!("{name} · {m}"),
                    None => name,
                },
                model: model.clone(),
                percent,
                severity: severity_for(percent).into(),
                resets_at: resets
                    .filter(|_| !reset)
                    .and_then(|r| Utc.timestamp_opt(r, 0).single())
                    .map(|t| t.to_rfc3339()),
                active: false,
            })
        })
        .collect()
}

/// `5h limit`, `Weekly limit`, `30m limit`, `2d limit`.
fn window_name(minutes: Option<i64>, weekly: bool) -> String {
    match minutes {
        Some(10080) => "Weekly limit".into(),
        Some(m) if m % 1440 == 0 => format!("{}d limit", m / 1440),
        Some(m) if m % 60 == 0 => format!("{}h limit", m / 60),
        Some(m) => format!("{m}m limit"),
        None if weekly => "Weekly limit".into(),
        None => "Session limit".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    const FIXTURE: &str = include_str!("../../../../testdata/codex/rollout-usage.jsonl");

    /// 2026-10-16T11:00:00Z (epoch 1_792_148_400) — before the fixture's resets
    /// (12:00 and a week on).
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T11:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Write a rollout under `home/sessions/2026/10/<day>/` with the given mtime.
    fn rollout(home: &Path, day: &str, name: &str, text: &str, mtime_secs: u64) -> PathBuf {
        let dir = home.join("sessions/2026/10").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("rollout-{name}.jsonl"));
        std::fs::write(&p, text).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs);
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
        p
    }

    fn token_count_line(ts: &str, rl: Value) -> String {
        serde_json::json!({
            "timestamp": ts, "type": "event_msg",
            "payload": {"type": "token_count", "info": null, "rate_limits": rl}
        })
        .to_string()
            + "\n"
    }

    fn windows(id: &str, primary: f64, secondary: f64) -> Value {
        serde_json::json!({
            "limit_id": id, "limit_name": null,
            "primary": {"used_percent": primary, "window_minutes": 300, "resets_at": 1792152000},
            "secondary": {"used_percent": secondary, "window_minutes": 10080, "resets_at": 1792756800},
            "credits": {"has_credits": false, "unlimited": false, "balance": "0"},
            "plan_type": "plus"
        })
    }

    fn premium() -> Value {
        serde_json::json!({
            "limit_id": "premium", "primary": null, "secondary": null,
            "credits": {"has_credits": true, "unlimited": false, "balance": "12.5"},
            "plan_type": null
        })
    }

    fn summary(u: &ProviderUsage) -> Vec<(String, String, f64, String, bool)> {
        u.limits
            .iter()
            .map(|l| {
                (
                    l.kind.clone(),
                    l.label.clone(),
                    l.percent,
                    l.severity.clone(),
                    l.active,
                )
            })
            .collect()
    }

    fn row(
        kind: &str,
        label: &str,
        pct: f64,
        sev: &str,
        active: bool,
    ) -> (String, String, f64, String, bool) {
        (kind.into(), label.into(), pct, sev.into(), active)
    }

    #[test]
    fn reads_the_fixture() {
        let d = tempfile::tempdir().unwrap();
        rollout(d.path(), "16", "fixture", FIXTURE, 1_792_146_700);
        let u = get_at(d.path(), now());
        assert_eq!(u.status, "ok");
        assert_eq!(u.plan.as_deref(), Some("Plus"));
        assert_eq!(u.observed_at.as_deref(), Some("2026-10-16T10:30:00+00:00"));
        assert_eq!(
            summary(&u),
            [
                row("session", "5h limit", 50.0, "normal", true),
                row("weekly_all", "Weekly limit", 2.0, "normal", false),
            ]
        );
        assert_eq!(
            u.limits[0].resets_at.as_deref(),
            Some("2026-10-16T12:00:00+00:00")
        );
        // Credits from the newer `premium` snapshot, not the main one.
        assert_eq!(u.credits.unwrap().balance.as_deref(), Some("12.5"));
    }

    #[test]
    fn severity_bands_and_scoped_limits() {
        let d = tempfile::tempdir().unwrap();
        let mut scoped = windows("codex_other", 96.0, 71.0);
        scoped["limit_name"] = "GPT-Mini".into();
        // The scoped id is written after the main one, interleaved, and must not replace it.
        let text = token_count_line("2026-10-16T10:00:00Z", windows("codex", 40.0, 5.0))
            + &token_count_line("2026-10-16T10:01:00Z", scoped.clone())
            + &token_count_line("2026-10-16T10:02:00Z", windows("codex", 60.0, 6.0))
            + &token_count_line("2026-10-16T10:03:00Z", scoped);
        rollout(d.path(), "16", "a", &text, 1_792_144_980);
        let u = get_at(d.path(), now());
        assert_eq!(
            summary(&u),
            [
                // `active` is the fullest main window: a scoped one only stops its model.
                row("session", "5h limit", 60.0, "normal", true),
                row("weekly_all", "Weekly limit", 6.0, "normal", false),
                row(
                    "session_scoped",
                    "5h limit · GPT-Mini",
                    96.0,
                    "critical",
                    false
                ),
                row(
                    "weekly_scoped",
                    "Weekly limit · GPT-Mini",
                    71.0,
                    "warning",
                    false
                ),
            ]
        );
        assert_eq!(u.limits[2].model.as_deref(), Some("GPT-Mini"));
        // observed_at is the main snapshot's.
        assert_eq!(u.observed_at.as_deref(), Some("2026-10-16T10:02:00+00:00"));
    }

    #[test]
    fn all_six_orders_of_premium_scoped_and_main_agree() {
        let mut scoped = windows("codex_other", 30.0, 3.0);
        scoped["limit_name"] = "GPT-Mini".into(); // carries credits false / "0" too
        let kinds = [premium(), scoped, windows("codex", 50.0, 2.0)];
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let mut seen = Vec::new();
        for order in orders {
            let d = tempfile::tempdir().unwrap();
            let text: String = order
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let ts = format!("2026-10-16T10:00:{:02}Z", i * 20);
                    token_count_line(&ts, kinds[*k].clone())
                })
                .collect();
            rollout(d.path(), "16", "a", &text, 1_792_144_900);
            let u = get_at(d.path(), now());
            let c = u.credits.clone().unwrap();
            assert_eq!(
                (c.has_credits, c.balance.as_deref()),
                (true, Some("12.5")),
                "{order:?}"
            );
            assert_eq!(u.limits.len(), 4, "{order:?}");
            seen.push((u.limits, u.credits));
        }
        assert!(seen.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn a_scoped_snapshot_older_than_the_main_one_does_not_count() {
        let d = tempfile::tempdir().unwrap();
        let mut scoped = windows("codex_other", 30.0, 3.0);
        scoped["limit_name"] = "GPT-Mini".into();
        let text = token_count_line("2026-10-16T09:00:00Z", scoped)
            + &token_count_line("2026-10-16T10:00:00Z", windows("codex", 50.0, 2.0));
        rollout(d.path(), "16", "a", &text, 1_792_144_800);
        let u = get_at(d.path(), now());
        assert_eq!(u.limits.len(), 2);
        assert!(u.limits.iter().all(|l| l.model.is_none()));
    }

    #[test]
    fn credits_do_not_depend_on_event_order() {
        for (name, text) in [
            (
                "premium-then-codex",
                token_count_line("2026-10-16T10:00:00Z", premium())
                    + &token_count_line("2026-10-16T10:01:00Z", windows("codex", 50.0, 2.0)),
            ),
            (
                "codex-then-premium",
                token_count_line("2026-10-16T10:00:00Z", windows("codex", 50.0, 2.0))
                    + &token_count_line("2026-10-16T10:01:00Z", premium()),
            ),
        ] {
            let d = tempfile::tempdir().unwrap();
            rollout(d.path(), "16", name, &text, 1_792_144_900);
            let u = get_at(d.path(), now());
            assert_eq!(u.status, "ok", "{name}");
            assert_eq!(u.limits[0].percent, 50.0, "{name}");
            let c = u.credits.unwrap();
            assert_eq!(
                (c.has_credits, c.balance.as_deref()),
                (true, Some("12.5")),
                "{name}"
            );
        }
        // With no side bucket, credits come from the main snapshot.
        let d = tempfile::tempdir().unwrap();
        let text = token_count_line("2026-10-16T10:00:00Z", windows("codex", 50.0, 2.0));
        rollout(d.path(), "16", "a", &text, 1_792_144_900);
        let c = get_at(d.path(), now()).credits.unwrap();
        assert_eq!((c.has_credits, c.balance.as_deref()), (false, Some("0")));
    }

    #[test]
    fn older_codex_without_a_limit_id_is_the_main_bucket() {
        let d = tempfile::tempdir().unwrap();
        let mut rl = windows("x", 30.0, 3.0);
        rl.as_object_mut().unwrap().remove("limit_id");
        rl["primary"] = serde_json::json!({"used_percent": 30.0, "window_minutes": 300, "resets_in_seconds": 600});
        rollout(
            d.path(),
            "16",
            "a",
            &token_count_line("2026-10-16T10:55:00Z", rl),
            1_792_148_100,
        );
        let u = get_at(d.path(), now());
        assert_eq!(u.limits[0].kind, "session");
        assert_eq!(
            u.limits[0].resets_at.as_deref(),
            Some("2026-10-16T11:05:00+00:00")
        );
    }

    #[test]
    fn resets_apply_on_every_call() {
        let d = tempfile::tempdir().unwrap();
        let mut rl = windows("codex", 100.0, 40.0);
        // No reset time: expires at observed_at + window_minutes (10:00 + 5h = 15:00).
        rl["primary"].as_object_mut().unwrap().remove("resets_at");
        rollout(
            d.path(),
            "16",
            "a",
            &token_count_line("2026-10-16T10:00:00Z", rl),
            1_792_144_800,
        );
        let at = |h: i64| get_at(d.path(), now() + chrono::Duration::hours(h));
        let u = at(0);
        assert_eq!(
            (u.limits[0].percent, u.limits[0].severity.as_str()),
            (100.0, "critical")
        );
        let u = at(4); // 15:00: the 5-hour window has reset, the weekly one hasn't.
        assert_eq!(
            (u.limits[0].percent, u.limits[0].resets_at.as_deref()),
            (0.0, None)
        );
        assert_eq!(u.limits[0].severity, "normal");
        assert!(u.limits[1].active && u.limits[1].percent == 40.0);
        let u = at(24 * 8); // A week on, both have.
        assert!(u.limits.iter().all(|l| l.percent == 0.0 && !l.active));
        assert_eq!(u.status, "ok");
    }

    #[test]
    fn no_home_no_sessions_only_credits_no_limits() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("codex");
        assert_eq!(get_at(&home, now()).status, "not_installed");

        std::fs::create_dir_all(&home).unwrap();
        let u = get_at(&home, now());
        assert_eq!(
            (u.status.as_str(), u.error.as_deref()),
            ("no_data", Some("no Codex sessions yet"))
        );

        let text = token_count_line("2026-10-16T10:00:00Z", premium()) + "{not json\n";
        rollout(&home, "16", "a", &text, 1_792_144_800);
        let u = get_at(&home, now());
        assert_eq!(u.status, "no_data");
        assert!(u.limits.is_empty());
        assert!(u.error.unwrap().contains("credits"));
        assert!(u.credits.unwrap().has_credits);

        let d = tempfile::tempdir().unwrap();
        let text = token_count_line("2026-10-16T10:00:00Z", Value::Null);
        rollout(d.path(), "16", "a", &text, 1_792_144_800);
        let u = get_at(d.path(), now());
        assert!(u.error.unwrap().contains("no rate limits for this login"));
    }

    #[test]
    fn stops_after_files_with_no_limits() {
        let d = tempfile::tempdir().unwrap();
        // Oldest: a real window, behind more limitless files than the scan will read.
        rollout(
            d.path(),
            "15",
            "old",
            &token_count_line("2026-10-15T10:00:00Z", windows("codex", 9.0, 1.0)),
            1_792_058_400,
        );
        for i in 0..MAX_LIMITLESS_FILES as u64 {
            let text = token_count_line("2026-10-16T10:00:00Z", Value::Null);
            rollout(d.path(), "16", &format!("k{i}"), &text, 1_792_144_800 + i);
        }
        assert_eq!(get_at(d.path(), now()).status, "no_data");
    }

    #[test]
    fn newest_event_across_files_and_early_stop() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        // A session started yesterday but written last (a resumed or long one).
        rollout(
            home,
            "15",
            "old-day",
            &token_count_line("2026-10-16T10:40:00Z", windows("codex", 60.0, 3.0)),
            1_792_147_200,
        );
        // Newest mtime: only credits.
        rollout(
            home,
            "16",
            "credits",
            &token_count_line("2026-10-16T10:50:00Z", premium()),
            1_792_147_800,
        );
        // Within the slack (mtime 30 s before the best event): read, and its event is newer.
        rollout(
            home,
            "16",
            "slack",
            &token_count_line("2026-10-16T10:41:00Z", windows("codex", 61.0, 3.0)),
            1_792_147_170,
        );
        // Past the slack: never read, though its (inconsistent) event is newer still.
        rollout(
            home,
            "01",
            "ancient",
            &token_count_line("2026-10-16T10:45:00Z", windows("codex", 99.0, 99.0)),
            1_792_147_100,
        );

        let files = rollouts(&home.join("sessions")).unwrap();
        assert_eq!(files.len(), 4);
        let u = get_at(home, now());
        assert_eq!((u.status.as_str(), u.limits[0].percent), ("ok", 61.0));
        assert_eq!(u.observed_at.as_deref(), Some("2026-10-16T10:41:00+00:00"));
        assert!(u.credits.unwrap().has_credits);
    }

    #[test]
    fn reads_back_past_big_lines() {
        let d = tempfile::tempdir().unwrap();
        let big = |n: usize| {
            format!(
                "{}\n",
                serde_json::json!({"type": "response_item", "payload": {"text": "x".repeat(n)}})
            )
        };
        // 5 MiB in one line, then 5 lines of 1 MiB, after the event.
        let mut text =
            token_count_line("2026-10-16T10:00:00Z", windows("codex", 33.0, 1.0)) + &big(5 << 20);
        for _ in 0..5 {
            text += &big(1 << 20);
        }
        rollout(d.path(), "16", "long", &text, 1_792_144_800);
        rollout(
            d.path(),
            "15",
            "older",
            &token_count_line("2026-10-15T10:00:00Z", windows("codex", 1.0, 1.0)),
            1_792_058_400,
        );
        assert_eq!(get_at(d.path(), now()).limits[0].percent, 33.0);
    }

    #[test]
    fn the_byte_budget_bounds_a_scan() {
        let d = tempfile::tempdir().unwrap();
        let text = token_count_line("2026-10-16T10:00:00Z", windows("codex", 33.0, 1.0))
            + &"y".repeat((MAX_BYTES + 1024) as usize)
            + "\n";
        let p = rollout(d.path(), "16", "huge", &text, 1_792_144_800);
        let mut budget = MAX_BYTES;
        let mut lines = 0;
        read_back(&p, &mut budget, |_| {
            lines += 1;
            true
        });
        assert_eq!((budget, lines), (0, 0));
        assert_eq!(get_at(d.path(), now()).status, "no_data");
    }

    #[test]
    fn read_back_yields_every_line_last_first() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        let lines: Vec<String> = (0..2000)
            .map(|i| format!("{i}:{}", "z".repeat(i % 700)))
            .collect();
        std::fs::write(&p, lines.join("\n")).unwrap();
        let mut got = Vec::new();
        let mut budget = MAX_BYTES;
        read_back(&p, &mut budget, |l| {
            got.push(String::from_utf8(l.to_vec()).unwrap());
            true
        });
        got.reverse();
        assert_eq!(got, lines);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_sessions_dir_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let d = tempfile::tempdir().unwrap();
        rollout(
            d.path(),
            "16",
            "a",
            &token_count_line("2026-10-16T10:00:00Z", windows("codex", 5.0, 1.0)),
            1_792_144_800,
        );
        let sessions = d.path().join("sessions");
        let day = sessions.join("2026/10/16");
        // An unreadable day directory is skipped…
        std::fs::set_permissions(&day, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _g1 = Restore(day.clone());
        if std::fs::read_dir(&day).is_ok() {
            return; // running as root: permissions don't apply
        }
        assert_eq!(
            get_at(d.path(), now()).error.as_deref(),
            Some("no Codex sessions yet")
        );
        // …an unreadable sessions directory is an error.
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _g2 = Restore(sessions);
        let u = get_at(d.path(), now());
        assert_eq!(u.status, "error");
        assert!(u.error.unwrap().contains("can't read Codex sessions"));
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_never_opened() {
        let d = tempfile::tempdir().unwrap();
        rollout(
            d.path(),
            "16",
            "a",
            &token_count_line("2026-10-16T10:00:00Z", windows("codex", 5.0, 1.0)),
            1_792_144_800,
        );
        let fifo = d.path().join("sessions/2026/10/16/rollout-fifo.jsonl");
        let ok = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        assert_eq!(rollouts(&d.path().join("sessions")).unwrap().len(), 1);
        assert_eq!(get_at(d.path(), now()).status, "ok");
    }
}
