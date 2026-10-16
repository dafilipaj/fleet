//! Claude subscription usage limits — the numbers Claude Code's `/usage` shows
//! (current 5-hour session, weekly all-models, weekly per-model such as Fable,
//! extra usage / spend).
//!
//! **Source.** The plan-usage endpoint Claude Code itself calls
//! (`GET https://api.anthropic.com/api/oauth/usage`), with the OAuth access token
//! Claude Code stores: the macOS keychain item `Claude Code-credentials`, else
//! `<claude home>/.credentials.json`. It is undocumented, so parsing is lenient:
//! the generic `limits` list is used when present, the older named windows
//! (`five_hour`, `seven_day`, `seven_day_opus`, …) otherwise. The token is never
//! refreshed here (that would rewrite Claude Code's credentials); an expired one
//! is reported, and the next Claude Code run renews it.
//!
//! **Cost.** The endpoint rate-limits, so a read is cached in
//! `<claude home>/fleet-usage.json` for [`FRESH_SECS`]; a failed refresh serves
//! the last good read marked `stale`, and backs off for [`BACKOFF_SECS`].
//!
//! `curl` does the HTTP (the token goes in on stdin, never on the command line).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
/// A read younger than this is served from the cache.
pub const FRESH_SECS: i64 = 60;
/// After a failed read, don't ask again for this long.
pub const BACKOFF_SECS: i64 = 120;
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// `fleet usage --json` (and the web's `GET /api/hosts/:host/usage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Whose limits these are (from Claude Code's state). Absent when unknown.
    pub account: Option<Account>,
    /// Every limit, in the endpoint's order: session first, then weekly ones.
    pub limits: Vec<Limit>,
    /// Pay-as-you-go usage beyond the plan (absent when the endpoint has none).
    pub extra_usage: Option<ExtraUsage>,
    /// When these numbers were read from the endpoint (RFC 3339).
    pub fetched_at: String,
    /// True when a refresh failed and this is the last good read.
    pub stale: bool,
    /// Why the last refresh failed (with `stale`).
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub uuid: Option<String>,
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Subscription, as Claude Code records it: `pro`, `max`, `team`, `enterprise`.
    pub plan: Option<String>,
    /// Rate-limit tier, e.g. `default_claude_max_5x`.
    pub tier: Option<String>,
    /// Human plan name, e.g. `Team · Max 5x`.
    pub plan_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    /// `session`, `weekly_all`, `weekly_scoped`, … — Claude: the endpoint's `kind`;
    /// Codex: `session`, `weekly_all`, `session_scoped`, `weekly_scoped` (fleet's own).
    pub kind: String,
    /// `session` or `weekly`.
    pub group: String,
    /// Display name: `Current session`, `Weekly · all models`, `Weekly · Fable`.
    pub label: String,
    /// The model a scoped limit counts (`Fable`, `Opus`, …).
    pub model: Option<String>,
    /// Used, as a percentage of the limit (0–100, may exceed 100).
    pub percent: f64,
    /// The endpoint's severity: `normal`, `warning`, `critical`, … (open set).
    pub severity: String,
    /// When the window resets (RFC 3339).
    pub resets_at: Option<String>,
    /// The limit currently binding (the one that stops you first).
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    pub enabled: bool,
    /// Spent this period, in `currency` units (e.g. dollars, not cents).
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub currency: Option<String>,
    pub percent: Option<f64>,
}

/// One provider's limits in a shape shared by every provider: `providers[]` of
/// `fleet usage --json`. Claude's entry repeats the top-level fields; Codex's
/// comes from [`crate::core::usage_codex`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    /// The harness id: `claude`, `codex`.
    pub id: String,
    /// Display name: `Claude`, `Codex`.
    pub label: String,
    /// `ok`, `stale` (a refresh failed; these are the last good numbers),
    /// `no_data` (installed, but no limits recorded yet), `not_installed`, `error`.
    pub status: String,
    /// Human plan name when known: `Team · Max 5x`, `Plus`.
    pub plan: Option<String>,
    /// Every window, in the same item shape as the top-level `limits`.
    pub limits: Vec<Limit>,
    /// When fleet read the source (RFC 3339).
    pub fetched_at: Option<String>,
    /// When the provider recorded these numbers (RFC 3339): the endpoint read for
    /// Claude, the transcript event for Codex. Its age is how stale they may be.
    pub observed_at: Option<String>,
    /// Why the numbers are missing or stale.
    pub error: Option<String>,
    /// Prepaid credits, when the provider reports them (Codex).
    pub credits: Option<Credits>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    /// Balance as the provider records it (Codex: a decimal string, e.g. `"0"`).
    pub balance: Option<String>,
}

impl ProviderUsage {
    /// Claude's entry: the same numbers as the top-level fields.
    pub fn claude(u: &Usage) -> ProviderUsage {
        ProviderUsage {
            id: "claude".into(),
            label: "Claude".into(),
            status: if u.stale { "stale" } else { "ok" }.into(),
            plan: u.account.as_ref().and_then(|a| a.plan_label.clone()),
            limits: u.limits.clone(),
            fetched_at: Some(u.fetched_at.clone()),
            observed_at: Some(u.fetched_at.clone()),
            error: u.error.clone(),
            credits: None,
        }
    }
}

/// `fleet usage --json`: Claude's fields at the top level (the original shape,
/// unchanged), plus `providers` — Claude and Codex in one shape.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    #[serde(flatten)]
    pub claude: Usage,
    pub providers: Vec<ProviderUsage>,
}

impl Report {
    pub fn new(claude: Usage, codex: ProviderUsage) -> Report {
        Report {
            providers: vec![ProviderUsage::claude(&claude), codex],
            claude,
        }
    }
}

/// `fleet usage --json` when Claude has no read at all: printed on stdout before the
/// command fails (exit 1), so a caller still gets Codex. None of the Claude fields.
#[derive(Debug, Clone, Serialize)]
pub struct FailedReport {
    pub error: String,
    pub providers: Vec<ProviderUsage>,
}

impl FailedReport {
    pub fn new(error: &str, codex: ProviderUsage) -> FailedReport {
        let claude = ProviderUsage {
            id: "claude".into(),
            label: "Claude".into(),
            status: "error".into(),
            plan: None,
            limits: Vec::new(),
            fetched_at: None,
            observed_at: None,
            error: Some(error.to_string()),
            credits: None,
        };
        FailedReport {
            error: error.to_string(),
            providers: vec![claude, codex],
        }
    }
}

/// The bands fleet colours limits in (the CLI's bars, the web's `usageLevel`):
/// `normal` below 70%, `warning` from 70%, `critical` from 90%. Codex's `severity`;
/// Claude's comes from its endpoint.
pub fn severity_for(percent: f64) -> &'static str {
    if percent >= 90.0 {
        "critical"
    } else if percent >= 70.0 {
        "warning"
    } else {
        "normal"
    }
}

/// On-disk cache: the last good read plus the last failure.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    usage: Option<Usage>,
    failed_at: Option<String>,
    error: Option<String>,
}

fn cache_path(home: &Path) -> PathBuf {
    home.join("fleet-usage.json")
}

fn read_cache(home: &Path) -> Cache {
    std::fs::read_to_string(cache_path(home))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_cache(home: &Path, c: &Cache) {
    if let Ok(text) = serde_json::to_string_pretty(c) {
        let path = cache_path(home);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

fn age_secs(ts: Option<&str>, now: DateTime<Utc>) -> Option<i64> {
    let t = DateTime::parse_from_rfc3339(ts?).ok()?;
    Some((now - t.with_timezone(&Utc)).num_seconds())
}

/// Usage for the Claude account on this machine. `refresh` skips the cache
/// (but not the backoff after a failure, unless the cache is empty).
pub fn get(home: &Path, refresh: bool) -> Result<Usage, String> {
    get_with(home, refresh, Utc::now(), &fetch_live)
}

/// The endpoint's JSON and the account it belongs to (swapped out in tests).
type Fetch<'a> = dyn Fn(&Path) -> Result<(Value, Option<Account>), String> + 'a;

fn get_with(
    home: &Path,
    refresh: bool,
    now: DateTime<Utc>,
    fetch: &Fetch<'_>,
) -> Result<Usage, String> {
    let mut cache = read_cache(home);
    let fresh = cache
        .usage
        .as_ref()
        .and_then(|u| age_secs(Some(&u.fetched_at), now))
        .is_some_and(|a| a < FRESH_SECS);
    let backing_off = age_secs(cache.failed_at.as_deref(), now).is_some_and(|a| a < BACKOFF_SECS);
    let serve_cached = |c: &Cache| {
        c.usage.clone().map(|mut u| {
            u.stale = c.error.is_some();
            u.error = c.error.clone();
            u
        })
    };
    if ((fresh && !refresh) || backing_off)
        && let Some(u) = serve_cached(&cache)
    {
        return Ok(u);
    }
    match fetch(home) {
        Ok((v, account)) => {
            let usage = parse(&v, account, now);
            cache = Cache {
                usage: Some(usage.clone()),
                failed_at: None,
                error: None,
            };
            write_cache(home, &cache);
            Ok(usage)
        }
        Err(e) => {
            cache.failed_at = Some(now.to_rfc3339());
            cache.error = Some(e.clone());
            write_cache(home, &cache);
            serve_cached(&cache).ok_or(e)
        }
    }
}

// --- credentials & HTTP ----------------------------------------------------------

fn credentials(home: &Path) -> Result<Value, String> {
    if cfg!(target_os = "macos")
        && let Ok(out) = Command::new("security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
            .stderr(Stdio::null())
            .output()
        && out.status.success()
        && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
    {
        return Ok(v);
    }
    let file = home.join(".credentials.json");
    let text = std::fs::read_to_string(&file)
        .map_err(|_| "no Claude Code login found (run `claude` and /login)".to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))
}

fn access_token(creds: &Value) -> Result<String, String> {
    let o = &creds["claudeAiOauth"];
    let token = o["accessToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Claude Code is not logged in with a Claude subscription")?;
    if let Some(exp) = o["expiresAt"].as_i64()
        && exp <= Utc::now().timestamp_millis()
    {
        return Err(
            "the Claude Code login token has expired — run `claude` once to renew it".into(),
        );
    }
    Ok(token.to_string())
}

/// The endpoint's JSON, plus the account it belongs to.
fn fetch_live(home: &Path) -> Result<(Value, Option<Account>), String> {
    let creds = credentials(home)?;
    let token = access_token(&creds)?;
    let account = account(&creds);
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "10",
            "-K",
            "-",
            "-w",
            "\n%{http_code}",
            ENDPOINT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("curl: no stdin")?;
        let cfg = format!(
            "header = \"Authorization: Bearer {token}\"\n\
             header = \"anthropic-beta: oauth-2025-04-20\"\n\
             header = \"User-Agent: fleet/{}\"\n",
            env!("CARGO_PKG_VERSION")
        );
        stdin
            .write_all(cfg.as_bytes())
            .map_err(|e| format!("curl: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("usage request failed: {}", err.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code.trim() {
        "200" => serde_json::from_str(body)
            .map(|v| (v, account))
            .map_err(|e| format!("usage endpoint: bad JSON ({e})")),
        "401" | "403" => Err(format!(
            "usage endpoint refused the login (HTTP {}) — run `claude` and /login",
            code.trim()
        )),
        "429" => Err("usage endpoint is rate-limited (HTTP 429)".into()),
        c => Err(format!("usage endpoint answered HTTP {c}")),
    }
}

// --- account ------------------------------------------------------------------------

fn claude_state_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(|d| PathBuf::from(d).join(".claude.json"))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude.json"))
}

fn account(creds: &Value) -> Option<Account> {
    let state: Value = std::fs::read_to_string(claude_state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    account_from(&state["oauthAccount"], &creds["claudeAiOauth"])
}

fn account_from(acct: &Value, oauth: &Value) -> Option<Account> {
    let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let plan = s(&oauth["subscriptionType"]);
    let tier = s(&oauth["rateLimitTier"]).or_else(|| s(&acct["userRateLimitTier"]));
    let a = Account {
        uuid: s(&acct["accountUuid"]),
        email: s(&acct["emailAddress"]),
        organization: s(&acct["organizationName"]),
        plan_label: plan_label(plan.as_deref(), tier.as_deref()),
        plan,
        tier,
    };
    (a.uuid.is_some() || a.email.is_some() || a.plan.is_some()).then_some(a)
}

/// `team` + `default_claude_max_5x` → `Team · Max 5x`; `max` + `…max_20x` → `Max 20x`.
fn plan_label(plan: Option<&str>, tier: Option<&str>) -> Option<String> {
    let cap = |w: &str| {
        let mut c = w.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    let tier_name = tier
        .map(|t| t.strip_prefix("default_").unwrap_or(t))
        .map(|t| t.strip_prefix("claude_").unwrap_or(t))
        .filter(|t| !t.is_empty() && *t != "raven")
        .map(|t| t.split('_').map(cap).collect::<Vec<_>>().join(" "));
    match (plan.map(cap), tier_name) {
        (Some(p), Some(t)) if t.to_lowercase().starts_with(&p.to_lowercase()) => Some(t),
        (Some(p), Some(t)) => Some(format!("{p} · {t}")),
        (Some(p), None) => Some(p),
        (None, t) => t,
    }
}

// --- parsing ------------------------------------------------------------------------

fn parse(v: &Value, account: Option<Account>, now: DateTime<Utc>) -> Usage {
    let limits = match v["limits"].as_array() {
        Some(list) if !list.is_empty() => list.iter().filter_map(limit_from_list).collect(),
        _ => legacy_limits(v),
    };
    Usage {
        account,
        limits,
        extra_usage: extra_usage(v),
        fetched_at: now.to_rfc3339(),
        stale: false,
        error: None,
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn limit_from_list(l: &Value) -> Option<Limit> {
    let kind = l["kind"].as_str()?.to_string();
    let group = l["group"].as_str().unwrap_or("").to_string();
    let model = l["scope"]["model"]["display_name"]
        .as_str()
        .or_else(|| l["scope"]["model"]["id"].as_str())
        .map(str::to_string);
    let surface = l["scope"]["surface"]["display_name"]
        .as_str()
        .or_else(|| l["scope"]["surface"].as_str())
        .map(str::to_string);
    let label = match (kind.as_str(), &model, &surface) {
        ("session", _, _) => "Current session".to_string(),
        ("weekly_all", _, _) => "Weekly · all models".to_string(),
        (_, Some(m), _) => format!("{} · {m}", group_title(&group)),
        (_, None, Some(s)) => format!("{} · {s}", group_title(&group)),
        (k, None, None) => k.replace('_', " "),
    };
    Some(Limit {
        kind,
        group,
        label,
        model,
        percent: num(&l["percent"]).unwrap_or(0.0),
        severity: l["severity"].as_str().unwrap_or("normal").to_string(),
        resets_at: l["resets_at"].as_str().map(str::to_string),
        active: l["is_active"].as_bool().unwrap_or(false),
    })
}

fn group_title(group: &str) -> String {
    match group {
        "weekly" => "Weekly".into(),
        "session" => "Session".into(),
        "" => "Limit".into(),
        g => g.to_string(),
    }
}

/// Older responses: named windows with `utilization` and `resets_at`.
fn legacy_limits(v: &Value) -> Vec<Limit> {
    let named = [
        ("five_hour", "session", "session", "Current session", None),
        (
            "seven_day",
            "weekly_all",
            "weekly",
            "Weekly · all models",
            None,
        ),
        (
            "seven_day_opus",
            "weekly_scoped",
            "weekly",
            "Weekly · Opus",
            Some("Opus"),
        ),
        (
            "seven_day_sonnet",
            "weekly_scoped",
            "weekly",
            "Weekly · Sonnet",
            Some("Sonnet"),
        ),
    ];
    named
        .iter()
        .filter_map(|(key, kind, group, label, model)| {
            let w = &v[*key];
            let percent = num(&w["utilization"])?;
            Some(Limit {
                kind: kind.to_string(),
                group: group.to_string(),
                label: label.to_string(),
                model: model.map(str::to_string),
                percent,
                severity: "normal".into(),
                resets_at: w["resets_at"].as_str().map(str::to_string),
                active: false,
            })
        })
        .collect()
}

fn money(m: &Value) -> Option<f64> {
    let minor = m["amount_minor"].as_f64()?;
    let exp = m["exponent"].as_i64().unwrap_or(2) as i32;
    Some(minor / 10f64.powi(exp))
}

fn extra_usage(v: &Value) -> Option<ExtraUsage> {
    let spend = &v["spend"];
    let extra = &v["extra_usage"];
    if spend.is_object() {
        return Some(ExtraUsage {
            enabled: spend["enabled"].as_bool().unwrap_or(false),
            used: money(&spend["used"]),
            limit: money(&spend["limit"]).or_else(|| money(&spend["cap"])),
            currency: spend["used"]["currency"].as_str().map(str::to_string),
            percent: num(&spend["percent"]),
        });
    }
    if extra.is_object() {
        return Some(ExtraUsage {
            enabled: extra["is_enabled"].as_bool().unwrap_or(false),
            used: num(&extra["used_credits"]),
            limit: num(&extra["monthly_limit"]),
            currency: extra["currency"].as_str().map(str::to_string),
            percent: num(&extra["utilization"]),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn sample() -> Value {
        serde_json::json!({
            "five_hour": {"utilization": 18.0, "resets_at": "2026-09-29T22:20:00+00:00"},
            "seven_day": {"utilization": 38.0, "resets_at": "2026-10-02T12:00:00+00:00"},
            "limits": [
                {"kind": "session", "group": "session", "percent": 18, "severity": "normal",
                 "resets_at": "2026-09-29T22:20:00+00:00", "scope": null, "is_active": false},
                {"kind": "weekly_all", "group": "weekly", "percent": 38, "severity": "normal",
                 "resets_at": "2026-10-02T12:00:00+00:00", "scope": null, "is_active": true},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 37, "severity": "warning",
                 "resets_at": "2026-10-02T12:00:00+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null},
                 "is_active": false}
            ],
            "spend": {"used": {"amount_minor": 1250, "currency": "USD", "exponent": 2},
                      "limit": null, "percent": 0, "enabled": false}
        })
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-29T20:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_the_limits_list() {
        let u = parse(&sample(), None, now());
        let labels: Vec<_> = u.limits.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Current session", "Weekly · all models", "Weekly · Fable"]
        );
        assert_eq!(u.limits[2].model.as_deref(), Some("Fable"));
        assert_eq!(u.limits[2].severity, "warning");
        assert!(u.limits[1].active);
        let x = u.extra_usage.unwrap();
        assert_eq!(
            (x.enabled, x.used, x.currency.as_deref()),
            (false, Some(12.5), Some("USD"))
        );
    }

    #[test]
    fn falls_back_to_named_windows() {
        let mut v = sample();
        v.as_object_mut().unwrap().remove("limits");
        let u = parse(&v, None, now());
        assert_eq!(u.limits.len(), 2);
        assert_eq!(
            (u.limits[0].kind.as_str(), u.limits[0].percent),
            ("session", 18.0)
        );
    }

    #[test]
    fn plan_labels() {
        assert_eq!(
            plan_label(Some("team"), Some("default_claude_max_5x")).as_deref(),
            Some("Team · Max 5x")
        );
        assert_eq!(
            plan_label(Some("max"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(plan_label(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(plan_label(None, None), None);
    }

    #[test]
    fn caches_and_backs_off() {
        let d = tempfile::tempdir().unwrap();
        let calls = Cell::new(0);
        let ok = |_: &Path| {
            calls.set(calls.get() + 1);
            Ok((sample(), None))
        };
        let fail = |_: &Path| {
            calls.set(calls.get() + 1);
            Err("usage endpoint is rate-limited (HTTP 429)".to_string())
        };
        let t0 = now();
        let secs = |s| t0 + chrono::Duration::seconds(s);

        // First read fetches; a second within FRESH_SECS is served from the cache.
        assert!(!get_with(d.path(), false, t0, &ok).unwrap().stale);
        get_with(d.path(), false, secs(30), &ok).unwrap();
        assert_eq!(calls.get(), 1);
        // Past FRESH_SECS a failure serves the last read, stale, with the reason.
        let u = get_with(d.path(), false, secs(90), &fail).unwrap();
        assert!(u.stale);
        assert!(u.error.unwrap().contains("429"));
        assert_eq!(calls.get(), 2);
        // Backing off: even --refresh doesn't ask again.
        assert!(get_with(d.path(), true, secs(120), &ok).unwrap().stale);
        assert_eq!(calls.get(), 2);
        // After the backoff a good read clears the error.
        assert!(!get_with(d.path(), false, secs(300), &ok).unwrap().stale);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn the_report_keeps_the_claude_shape_and_adds_providers() {
        let mut claude = parse(&sample(), None, now());
        claude.stale = true;
        claude.error = Some("usage endpoint is rate-limited (HTTP 429)".into());
        let codex = ProviderUsage {
            id: "codex".into(),
            label: "Codex".into(),
            status: "no_data".into(),
            plan: None,
            limits: vec![],
            fetched_at: None,
            observed_at: None,
            error: Some("no Codex sessions yet".into()),
            credits: None,
        };
        let v = serde_json::to_value(Report::new(claude.clone(), codex)).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "account",
                "error",
                "extra_usage",
                "fetched_at",
                "limits",
                "providers",
                "stale"
            ]
        );
        // Every original field is exactly what it was.
        let mut top = v.clone();
        top.as_object_mut().unwrap().remove("providers");
        assert_eq!(top, serde_json::to_value(&claude).unwrap());
        let p = &v["providers"];
        assert_eq!(
            (p[0]["id"].as_str(), p[1]["id"].as_str()),
            (Some("claude"), Some("codex"))
        );
        assert_eq!(p[0]["status"], "stale");
        assert_eq!(p[0]["limits"], v["limits"]);
        assert_eq!(p[0]["observed_at"], v["fetched_at"]);
        assert_eq!(p[1]["error"], "no Codex sessions yet");
    }

    #[test]
    fn severities() {
        assert_eq!(
            [69.9, 70.0, 89.9, 90.0, 120.0].map(severity_for),
            ["normal", "warning", "warning", "critical", "critical"]
        );
    }

    #[test]
    fn a_failure_with_no_cache_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let fail = |_: &Path| Err("no Claude Code login found".to_string());
        assert!(get_with(d.path(), false, now(), &fail).is_err());
    }
}
