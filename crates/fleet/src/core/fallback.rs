//! Subscription fallback (docs/architecture.md → Subscription fallback). A machine has at most
//! two subscriptions: Claude (the `claude` harness, or pi on an `anthropic/…` model) and the
//! ChatGPT plan (Codex, and pi on its other models). Every spawn names a primary harness; when
//! that harness's subscription is at its limit, the spawn starts on the configured fallback
//! harness instead — the other subscription.
//!
//! Opt-in: config `usage.enabled`. Settings:
//!
//! ```json
//! "usage": {
//!   "enabled": true,
//!   "fallback": { "claude": "codex", "codex": "claude", "pi": "claude" },
//!   "limits": { "session": 90, "weekly": 95, "paceMargin": 15 }
//! }
//! ```
//!
//! A subscription is **exhausted** when its current session is at `session` %, a weekly limit
//! at `weekly` % (a per-model weekly one only when the session's model is that model), or —
//! for bots and scheduled sessions only ([`Ctx::Bot`]) — when its weekly use runs ahead of the
//! week by more than `paceMargin` points (60 % used with 40 % of the week gone, margin 15).
//!
//! What a spawn gets ([`decide`]):
//! - primary not exhausted → the primary;
//! - primary exhausted, fallback usable → the fallback (its default model), the session marked;
//! - both exhausted (or no usable fallback): a spawn you start (CLI, the web's spawn dialog)
//!   starts on the primary with a warning; bots, schedules, Slack and GitHub spawns **hold** —
//!   the same way as on a host under heavy load ([`USAGE_MARK`] in the message).
//!
//! Fail-open: a usage read that fails, or one older than [`STALE_SECS`], counts as not
//! exhausted. `--no-fallback` (env `FLEET_NO_FALLBACK=1`) keeps the primary; an override
//! (`fleet usage override <minutes>`, Settings → Usage) turns the whole check off for a while.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::core::harness::{self, Harness};
use crate::core::tools;
use crate::core::usage::{self, Usage};
use crate::error::{Error, Result};

/// The words the web server and the UI recognise as "held for usage" (`USAGE_MARK` in
/// `web/lib/usage.mjs`).
pub const USAGE_MARK: &str = "is at its usage limit";
/// A usage read older than this is not trusted (counts as unknown, i.e. not exhausted).
pub const STALE_SECS: i64 = 30 * 60;
/// The longest an override lasts.
pub const MAX_OVERRIDE_MINUTES: i64 = 1440;
const WEEK_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// Who starts the session: decides whether pace counts and whether a spawn may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// You, by hand (CLI, the web's spawn dialog): never held, never paced.
    Manual,
    /// Slack and GitHub spawns: held when both are exhausted, not paced.
    Auto,
    /// Bots and scheduled sessions: held, and paced.
    Bot,
}

impl Ctx {
    pub fn parse(s: &str) -> Option<Ctx> {
        match s {
            "manual" => Some(Ctx::Manual),
            "auto" => Some(Ctx::Auto),
            "bot" => Some(Ctx::Bot),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub fallback: HashMap<Harness, Harness>,
    pub session: f64,
    pub weekly: f64,
    pub pace_margin: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            fallback: HashMap::from([
                (Harness::Claude, Harness::Codex),
                (Harness::Codex, Harness::Claude),
                (Harness::Pi, Harness::Claude),
            ]),
            session: 90.0,
            weekly: 95.0,
            pace_margin: 15.0,
        }
    }
}

impl Settings {
    /// From config `usage` (raw JSON, read leniently: a bad value keeps its default).
    pub fn from_value(v: &Value) -> Settings {
        let mut s = Settings {
            enabled: v.get("enabled").and_then(Value::as_bool) == Some(true),
            ..Settings::default()
        };
        if let Some(m) = v.get("fallback").and_then(Value::as_object) {
            for (from, to) in m {
                let Some(from) = Harness::parse(from) else {
                    continue;
                };
                match to.as_str().and_then(Harness::parse) {
                    Some(to) if to != from => {
                        s.fallback.insert(from, to);
                    }
                    // `null` / "" / "none": no fallback for this harness.
                    _ if to.is_null()
                        || to.as_str().is_some_and(|t| t.is_empty() || t == "none") =>
                    {
                        s.fallback.remove(&from);
                    }
                    _ => {}
                }
            }
        }
        let pct = |key: &str, def: f64| {
            v.get("limits")
                .and_then(|l| l.get(key))
                .and_then(Value::as_f64)
                .filter(|x| (1.0..=100.0).contains(x))
                .unwrap_or(def)
        };
        s.session = pct("session", s.session);
        s.weekly = pct("weekly", s.weekly);
        s.pace_margin = v
            .get("limits")
            .and_then(|l| l.get("paceMargin"))
            .and_then(Value::as_f64)
            .filter(|x| (0.0..=100.0).contains(x))
            .unwrap_or(s.pace_margin);
        s
    }

    pub fn load() -> Settings {
        Settings::from_value(&crate::core::config::get().usage)
    }
}

/// `claude` or `openai`: which subscription a session on `h` with `model` bills.
pub fn provider_of(h: Harness, model: Option<&str>) -> &'static str {
    match h {
        Harness::Claude => "claude",
        Harness::Codex => "openai",
        Harness::Pi => {
            let m = model
                .map(str::to_string)
                .or_else(|| h.default_model())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if m.starts_with("anthropic/") {
                "claude"
            } else {
                "openai"
            }
        }
    }
}

/// Why `u` counts as exhausted (empty: it does not). `model` scopes per-model weekly limits;
/// `pace` adds the weekly pace check.
pub fn exhausted(
    u: Option<&Usage>,
    model: Option<&str>,
    s: &Settings,
    pace: bool,
    now: DateTime<Utc>,
) -> Vec<String> {
    let Some(u) = u else {
        return vec![];
    };
    let fetched = DateTime::parse_from_rfc3339(&u.fetched_at)
        .map(|t| t.with_timezone(&Utc))
        .ok();
    if fetched.is_none_or(|t| (now - t).num_seconds() > STALE_SECS) {
        return vec![];
    }
    let model = model.unwrap_or_default().to_ascii_lowercase();
    let mut why = vec![];
    for l in &u.limits {
        let resets = l
            .resets_at
            .as_deref()
            .and_then(|r| DateTime::parse_from_rfc3339(r).ok())
            .map(|t| t.with_timezone(&Utc));
        if resets.is_some_and(|r| r <= now) {
            continue; // the window has reset since the read
        }
        if let Some(m) = &l.model
            && (model.is_empty() || !model.contains(&m.to_ascii_lowercase()))
        {
            continue;
        }
        let p = l.percent;
        if l.group == "session" {
            if p >= s.session {
                why.push(format!("session {p:.0}%"));
            }
            continue;
        }
        let name = match &l.model {
            Some(m) => format!("weekly {m}"),
            None => "weekly".to_string(),
        };
        if p >= s.weekly {
            why.push(format!("{name} {p:.0}%"));
        } else if pace && let Some(r) = resets {
            let left = (r - now).num_seconds() as f64 / WEEK_SECS;
            let gone = ((1.0 - left) * 100.0).clamp(0.0, 100.0);
            if p > gone + s.pace_margin {
                why.push(format!(
                    "{name} {p:.0}%, ahead of the week ({gone:.0}% gone)"
                ));
            }
        }
    }
    why
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderState {
    pub provider: String,
    pub exhausted: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FallbackState {
    pub harness: String,
    /// Installed, logged in, and on the other subscription.
    pub usable: bool,
    /// Why not, when not usable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
    pub state: ProviderState,
}

/// `fleet usage check --json` (and the web's `GET /api/hosts/:host/usage/check`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// The check is on (`usage.enabled`, no override).
    pub enabled: bool,
    /// The harness asked for.
    pub primary: String,
    /// The harness the session starts on.
    pub harness: String,
    /// The model it starts with: the one asked for, `None` on a fallback (its default).
    pub model: Option<String>,
    pub fell_back: bool,
    /// One line for a toast / the session's mark: `claude → codex: session 92%`.
    pub note: Option<String>,
    /// Set when the spawn must wait (both exhausted, not started by hand).
    pub hold: Option<String>,
    pub primary_state: ProviderState,
    pub fallback: Option<FallbackState>,
    /// Until when an override turns the check off (ms since the epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_until: Option<i64>,
}

/// What [`decide_with`] needs to know about the machine.
pub struct Inputs<'a> {
    pub claude: Option<&'a Usage>,
    pub openai: Option<&'a Usage>,
    /// Why a harness cannot start here (`None`: it can).
    pub unusable: &'a dyn Fn(Harness) -> Option<String>,
    pub now: DateTime<Utc>,
    pub host: &'a str,
    pub no_fallback: bool,
}

pub fn decide_with(
    h: Harness,
    model: Option<&str>,
    ctx: Ctx,
    s: &Settings,
    i: &Inputs,
) -> Decision {
    let usage_of = |p: &str| if p == "claude" { i.claude } else { i.openai };
    let pace = ctx == Ctx::Bot;
    let state = |h: Harness, model: Option<&str>| {
        let p = provider_of(h, model);
        let reasons = exhausted(
            usage_of(p),
            model.or(h.default_model().as_deref()),
            s,
            pace,
            i.now,
        );
        ProviderState {
            provider: p.into(),
            exhausted: !reasons.is_empty(),
            reasons,
        }
    };
    let primary_state = state(h, model);
    let mut d = Decision {
        enabled: s.enabled,
        primary: h.id().into(),
        harness: h.id().into(),
        model: model.map(str::to_string),
        fell_back: false,
        note: None,
        hold: None,
        primary_state: primary_state.clone(),
        fallback: None,
        override_until: None,
    };
    if !s.enabled {
        return d;
    }
    let fallback = s.fallback.get(&h).map(|&f| {
        let st = state(f, None);
        let why_not = (i.unusable)(f).or_else(|| {
            (st.provider == primary_state.provider)
                .then(|| format!("{} bills the same subscription", f.id()))
        });
        (
            f,
            FallbackState {
                harness: f.id().into(),
                usable: why_not.is_none(),
                why_not,
                state: st,
            },
        )
    });
    d.fallback = fallback.as_ref().map(|(_, f)| f.clone());
    if !primary_state.exhausted {
        return d;
    }
    let why = format!("{} ({})", h.id(), primary_state.reasons.join(", "));
    match &fallback {
        Some((f, fs)) if fs.usable && !fs.state.exhausted && !i.no_fallback => {
            d.harness = f.id().into();
            d.model = None;
            d.fell_back = true;
            d.note = Some(format!(
                "{} → {}: {}",
                h.id(),
                f.id(),
                primary_state.reasons.join(", ")
            ));
        }
        _ if ctx == Ctx::Manual || i.no_fallback => {
            d.note = Some(format!("{why} {USAGE_MARK} — starting on it anyway"));
        }
        _ => {
            let other = match &fallback {
                Some((f, fs)) if fs.usable => {
                    format!(" and so is {} ({})", f.id(), fs.state.reasons.join(", "))
                }
                Some((f, fs)) => format!(
                    "; {} cannot take it ({})",
                    f.id(),
                    fs.why_not.clone().unwrap_or_default()
                ),
                None => "; no fallback set".into(),
            };
            d.hold = Some(format!(
                "{}: {why} {USAGE_MARK}{other} — new sessions wait until it recovers",
                i.host
            ));
        }
    }
    d
}

// --- the machine ---------------------------------------------------------------------------

static NO_FALLBACK: AtomicBool = AtomicBool::new(false);

/// `--no-fallback`: keep the asked-for harness (this process).
pub fn set_no_fallback() {
    NO_FALLBACK.store(true, Ordering::Relaxed);
}

fn no_fallback() -> bool {
    NO_FALLBACK.load(Ordering::Relaxed)
        || std::env::var("FLEET_NO_FALLBACK").is_ok_and(|v| v == "1" || v == "true")
}

pub fn override_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"));
    base.join("fleet").join("usage-override.json")
}

/// The override's end (ms), while one is active.
pub fn override_until() -> Option<i64> {
    let text = std::fs::read_to_string(override_path()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let until = v.get("until")?.as_i64()?;
    (until > Utc::now().timestamp_millis()).then_some(until)
}

/// Turn the check off for `minutes` (0 ends an override).
pub fn set_override(minutes: i64) -> Result<Option<i64>> {
    let path = override_path();
    if minutes <= 0 {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    let until = Utc::now().timestamp_millis() + minutes.min(MAX_OVERRIDE_MINUTES) * 60_000;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::json!({ "until": until }).to_string())?;
    Ok(Some(until))
}

/// Why `h` cannot start here: not installed, or no login for its subscription.
fn unusable(h: Harness) -> Option<String> {
    let cmd = h.command();
    let bin = cmd.split_whitespace().next().unwrap_or(h.id());
    let installed = if bin.contains('/') {
        tools::is_executable(std::path::Path::new(&tools::expand_tilde(bin)))
    } else {
        tools::find_binary(bin).is_some()
    };
    if !installed {
        return Some(format!("{} is not installed", h.id()));
    }
    match h {
        Harness::Codex if !usage::has_openai_login(&harness::codex_home()) => {
            Some("codex has no ChatGPT login".into())
        }
        Harness::Pi if !harness::pi_home().join("auth.json").is_file() => {
            Some("pi has no login".into())
        }
        _ => None,
    }
}

/// The decision for a spawn on this machine. Reads usage (cached, see `core::usage`) only when
/// the check is on.
pub fn decide(h: Harness, model: Option<&str>, ctx: Ctx) -> Decision {
    let s = Settings::load();
    let ov = override_until();
    let on = Settings {
        enabled: s.enabled && ov.is_none(),
        ..s.clone()
    };
    let (claude, openai) = if on.enabled {
        (
            usage::get(&crate::core::discovery::claude_home(), false).ok(),
            usage::get_openai(&harness::codex_home(), false).and_then(|r| r.ok()),
        )
    } else {
        (None, None)
    };
    let host = crate::core::config::get().self_name();
    let mut d = decide_with(
        h,
        model,
        ctx,
        &on,
        &Inputs {
            claude: claude.as_ref(),
            openai: openai.as_ref(),
            unusable: &unusable,
            now: Utc::now(),
            host: &host,
            no_fallback: no_fallback(),
        },
    );
    d.override_until = ov;
    d
}

/// The mark the next tmux spawn of this process puts on its pane (`@fleet_fallback`).
static PENDING: Mutex<Option<String>> = Mutex::new(None);

pub fn take_pending_mark() -> Option<String> {
    PENDING.lock().ok()?.take()
}

/// For the CLI's spawns: swap `h` / `model` for the fallback when the decision says so (a note
/// on stderr), `Err` when the spawn must hold. A fallback's mark goes on the next tmux pane.
pub fn apply(h: &mut Harness, model: &mut Option<String>, ctx: Ctx) -> Result<()> {
    if crate::core::hosts::dry_run() && !Settings::load().enabled {
        return Ok(());
    }
    let d = decide(*h, model.as_deref(), ctx);
    if let Some(hold) = d.hold {
        return Err(Error::Other(hold));
    }
    if let Some(n) = &d.note {
        eprintln!("usage: {n}");
    }
    // Set (or cleared) on every decision: a mark never outlives the spawn it was made for.
    if let Ok(mut p) = PENDING.lock() {
        *p = d.fell_back.then(|| d.note.clone()).flatten();
    }
    if d.fell_back {
        *h = Harness::parse(&d.harness).unwrap_or(*h);
        *model = None;
    }
    Ok(())
}

/// For the ticks: why `h` holds now (bot context), or `None`.
pub fn hold_reason(h: Harness, model: Option<&str>) -> Option<String> {
    let s = Settings::load();
    if !s.enabled {
        return None;
    }
    decide(h, model, Ctx::Bot).hold
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::usage::Limit;

    fn lim(group: &str, model: Option<&str>, percent: f64, resets: &str) -> Limit {
        Limit {
            kind: if group == "session" {
                "session".into()
            } else {
                "weekly_all".into()
            },
            group: group.into(),
            label: String::new(),
            model: model.map(str::to_string),
            percent,
            severity: "normal".into(),
            resets_at: Some(resets.into()),
            active: false,
        }
    }

    fn usage(provider: &str, limits: Vec<Limit>, at: &str) -> Usage {
        Usage {
            provider: provider.into(),
            account: None,
            limits,
            extra_usage: None,
            fetched_at: at.into(),
            stale: false,
            error: None,
            openai: None,
        }
    }

    const NOW: &str = "2026-11-18T12:00:00+00:00"; // a Wednesday
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc)
    }
    fn on() -> Settings {
        Settings {
            enabled: true,
            ..Settings::default()
        }
    }

    #[test]
    fn settings_read_leniently() {
        let s = Settings::from_value(&serde_json::json!({
            "enabled": true,
            "fallback": { "claude": "pi", "codex": null, "pi": "bogus" },
            "limits": { "session": 80, "weekly": 500, "paceMargin": 10 }
        }));
        assert!(s.enabled);
        assert_eq!(s.fallback.get(&Harness::Claude), Some(&Harness::Pi));
        assert_eq!(s.fallback.get(&Harness::Codex), None);
        assert_eq!(
            s.fallback.get(&Harness::Pi),
            Some(&Harness::Claude),
            "bad value keeps the default"
        );
        assert_eq!((s.session, s.weekly, s.pace_margin), (80.0, 95.0, 10.0));
        assert!(!Settings::from_value(&Value::Null).enabled);
    }

    #[test]
    fn thresholds_scopes_and_pace() {
        let s = on();
        // Resets Saturday 12:00: 3 of 7 days left → 57% of the week gone.
        let sat = "2026-11-21T12:00:00+00:00";
        let u = usage(
            "claude",
            vec![
                lim("session", None, 91.0, "2026-11-18T14:00:00+00:00"),
                lim("weekly", None, 80.0, sat),
                lim("weekly", Some("Fable"), 96.0, sat),
            ],
            NOW,
        );
        let why = exhausted(Some(&u), Some("claude-opus-5-5"), &s, false, now());
        assert_eq!(why, vec!["session 91%"], "Fable's limit does not bind Opus");
        let why = exhausted(Some(&u), Some("claude-fable-5-2"), &s, false, now());
        assert_eq!(why, vec!["session 91%", "weekly Fable 96%"]);
        let paced = exhausted(Some(&u), None, &s, true, now());
        assert_eq!(
            paced,
            vec!["session 91%", "weekly 80%, ahead of the week (57% gone)"]
        );
        // A window that reset since the read, and a stale read, do not count.
        let old = usage(
            "claude",
            vec![lim("session", None, 99.0, "2026-11-18T11:00:00+00:00")],
            NOW,
        );
        assert!(exhausted(Some(&old), None, &s, false, now()).is_empty());
        let stale = usage(
            "claude",
            vec![lim("session", None, 99.0, sat)],
            "2026-11-18T11:00:00+00:00",
        );
        assert!(exhausted(Some(&stale), None, &s, false, now()).is_empty());
        assert!(exhausted(None, None, &s, true, now()).is_empty());
    }

    fn inputs<'a>(
        c: Option<&'a Usage>,
        o: Option<&'a Usage>,
        unusable: &'a dyn Fn(Harness) -> Option<String>,
    ) -> Inputs<'a> {
        Inputs {
            claude: c,
            openai: o,
            unusable,
            now: now(),
            host: "shed",
            no_fallback: false,
        }
    }

    #[test]
    fn falls_back_holds_or_warns() {
        let s = on();
        let sat = "2026-11-21T12:00:00+00:00";
        let full = usage("claude", vec![lim("session", None, 95.0, sat)], NOW);
        let ok = usage("openai", vec![lim("session", None, 10.0, sat)], NOW);
        let gpt_full = usage("openai", vec![lim("weekly", None, 99.0, sat)], NOW);
        let fine = |_: Harness| None;

        let d = decide_with(
            Harness::Claude,
            Some("claude-opus-5-5"),
            Ctx::Auto,
            &s,
            &inputs(Some(&full), Some(&ok), &fine),
        );
        assert!(d.fell_back);
        assert_eq!((d.harness.as_str(), d.model.as_deref()), ("codex", None));
        assert_eq!(d.note.as_deref(), Some("claude → codex: session 95%"));

        let d = decide_with(
            Harness::Claude,
            None,
            Ctx::Bot,
            &s,
            &inputs(Some(&full), Some(&gpt_full), &fine),
        );
        assert_eq!(d.harness, "claude");
        let hold = d.hold.unwrap();
        assert!(
            hold.contains(USAGE_MARK) && hold.contains("so is codex (weekly 99%)"),
            "{hold}"
        );

        let d = decide_with(
            Harness::Claude,
            None,
            Ctx::Manual,
            &s,
            &inputs(Some(&full), Some(&gpt_full), &fine),
        );
        assert_eq!((d.harness.as_str(), d.hold.is_none()), ("claude", true));
        assert!(d.note.unwrap().contains("anyway"));

        let missing =
            |h: Harness| (h == Harness::Codex).then(|| "codex is not installed".to_string());
        let d = decide_with(
            Harness::Claude,
            None,
            Ctx::Auto,
            &s,
            &inputs(Some(&full), None, &missing),
        );
        assert!(
            d.hold
                .unwrap()
                .contains("codex cannot take it (codex is not installed)")
        );

        let mut i = inputs(Some(&full), Some(&ok), &fine);
        i.no_fallback = true;
        let d = decide_with(Harness::Claude, None, Ctx::Bot, &s, &i);
        assert!(
            !d.fell_back && d.hold.is_none(),
            "--no-fallback starts the primary"
        );

        let d = decide_with(
            Harness::Claude,
            None,
            Ctx::Bot,
            &Settings::default(),
            &inputs(Some(&full), Some(&ok), &fine),
        );
        assert!(!d.fell_back && d.hold.is_none(), "off by default");
    }

    #[test]
    fn pi_bills_by_model() {
        assert_eq!(
            provider_of(Harness::Pi, Some("anthropic/claude-opus-5-5")),
            "claude"
        );
        assert_eq!(
            provider_of(Harness::Pi, Some("openai-codex/gpt-6")),
            "openai"
        );
        assert_eq!(provider_of(Harness::Codex, None), "openai");
    }
}
