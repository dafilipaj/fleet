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
//!
//! **OpenAI.** Codex (and pi, on the same ChatGPT plan) bill against a ChatGPT
//! subscription with its own 5-hour and weekly windows. They are read the same
//! way from the endpoint Codex's `/status` calls
//! (`GET https://chatgpt.com/backend-api/wham/usage`), with the ChatGPT login in
//! `$CODEX_HOME/auth.json`, cached in `$CODEX_HOME/fleet-usage.json`, and
//! reported under [`Usage::openai`]. pi's own login is a different OAuth app the
//! endpoint does not accept, so only Codex's is used.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
pub const OPENAI_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
/// A read younger than this is served from the cache.
pub const FRESH_SECS: i64 = 60;
/// After a failed read, don't ask again for this long.
pub const BACKOFF_SECS: i64 = 120;
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// `fleet usage --json` (and the web's `GET /api/hosts/:host/usage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// `claude` or `openai`.
    #[serde(default = "claude")]
    pub provider: String,
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
    /// Why the last refresh failed (with `stale`). With no `limits`, why there are none.
    pub error: Option<String>,
    /// The ChatGPT subscription Codex is logged in with on this machine (only on
    /// the top-level, Claude read; absent when there is no Codex ChatGPT login).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<Box<Usage>>,
}

fn claude() -> String {
    "claude".into()
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
    /// Prepaid credits left (OpenAI), in `currency` units or credits.
    #[serde(default)]
    pub balance: Option<f64>,
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
    /// Where the numbers come from: `endpoint` (a live read: Claude's usage endpoint, or the
    /// ChatGPT plan's for Codex) or `transcripts` (Codex's rollouts, as of its last turn).
    #[serde(default = "endpoint")]
    pub source: String,
}

fn endpoint() -> String {
    "endpoint".into()
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
            // A read that failed with nothing cached (`get_all` keeps it when ChatGPT read).
            status: if u.limits.is_empty() && u.error.is_some() {
                "error"
            } else if u.stale {
                "stale"
            } else {
                "ok"
            }
            .into(),
            plan: u.account.as_ref().and_then(|a| a.plan_label.clone()),
            limits: u.limits.clone(),
            fetched_at: Some(u.fetched_at.clone()),
            observed_at: Some(u.fetched_at.clone()),
            error: u.error.clone(),
            credits: None,
            source: endpoint(),
        }
    }

    /// Codex's entry from the ChatGPT plan's live read (`Usage.openai`), when it has numbers.
    /// The transcripts' entry ([`crate::core::usage_codex`]) stands in otherwise.
    pub fn codex_from_openai(o: &Usage) -> Option<ProviderUsage> {
        if o.limits.is_empty() {
            return None;
        }
        Some(ProviderUsage {
            id: "codex".into(),
            label: "Codex".into(),
            status: if o.stale { "stale" } else { "ok" }.into(),
            plan: o.account.as_ref().and_then(|a| a.plan_label.clone()),
            limits: o.limits.clone(),
            fetched_at: Some(o.fetched_at.clone()),
            observed_at: Some(o.fetched_at.clone()),
            error: o.error.clone(),
            credits: o.extra_usage.as_ref().map(|x| Credits {
                has_credits: x.enabled,
                unlimited: false,
                balance: x.balance.map(|b| format!("{b}")),
            }),
            source: endpoint(),
        })
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
    /// `codex`: the transcripts' entry, used when the ChatGPT read (`claude.openai`) has no numbers.
    pub fn new(claude: Usage, codex: ProviderUsage) -> Report {
        let codex = claude
            .openai
            .as_deref()
            .and_then(ProviderUsage::codex_from_openai)
            .unwrap_or(codex);
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
            source: endpoint(),
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

fn read_cache(path: &Path) -> Cache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Path, c: &Cache) {
    if let Ok(text) = serde_json::to_string_pretty(c) {
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
    let now = Utc::now();
    get_with(&cache_path(home), refresh, now, &|| {
        fetch_live(home).map(|(v, account)| parse(&v, account, now))
    })
}

/// Usage for the ChatGPT account Codex is logged in with (`codex_home`), or
/// `None` when there is no ChatGPT login there. Cached like [`get`].
pub fn get_openai(codex_home: &Path, refresh: bool) -> Option<Result<Usage, String>> {
    let creds = openai_credentials(codex_home)?;
    let now = Utc::now();
    Some(get_with(&cache_path(codex_home), refresh, now, &|| {
        fetch_openai(&creds).map(|v| parse_openai(&v, now))
    }))
}

/// Both subscriptions: the Claude read at the top level (as before), the ChatGPT
/// one under `openai`. Fails only when neither has anything to show.
pub fn get_all(claude_home: &Path, codex_home: &Path, refresh: bool) -> Result<Usage, String> {
    let openai = get_openai(codex_home, refresh).map(|r| {
        Box::new(r.unwrap_or_else(|e| Usage {
            provider: "openai".into(),
            error: Some(e),
            ..empty(Utc::now())
        }))
    });
    match get(claude_home, refresh) {
        Ok(u) => Ok(Usage { openai, ..u }),
        Err(e) if openai.is_some() => Ok(Usage {
            error: Some(e),
            openai,
            ..empty(Utc::now())
        }),
        Err(e) => Err(e),
    }
}

/// A read with no limits (a provider whose read failed with nothing cached).
fn empty(now: DateTime<Utc>) -> Usage {
    Usage {
        provider: claude(),
        account: None,
        limits: vec![],
        extra_usage: None,
        fetched_at: now.to_rfc3339(),
        stale: false,
        error: None,
        openai: None,
    }
}

/// A fresh read from the endpoint, parsed (swapped out in tests).
type Fetch<'a> = dyn Fn() -> Result<Usage, String> + 'a;

fn get_with(
    path: &Path,
    refresh: bool,
    now: DateTime<Utc>,
    fetch: &Fetch<'_>,
) -> Result<Usage, String> {
    let mut cache = read_cache(path);
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
    match fetch() {
        Ok(usage) => {
            cache = Cache {
                usage: Some(usage.clone()),
                failed_at: None,
                error: None,
            };
            write_cache(path, &cache);
            Ok(usage)
        }
        Err(e) => {
            cache.failed_at = Some(now.to_rfc3339());
            cache.error = Some(e.clone());
            write_cache(path, &cache);
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
    let cfg = format!(
        "header = \"Authorization: Bearer {token}\"\n\
         header = \"anthropic-beta: oauth-2025-04-20\"\n\
         header = \"User-Agent: fleet/{}\"\n",
        env!("CARGO_PKG_VERSION")
    );
    let v = curl_json(ENDPOINT, &cfg).map_err(|e| match e {
        HttpErr::Refused(code) => {
            format!("usage endpoint refused the login (HTTP {code}) — run `claude` and /login")
        }
        HttpErr::Other(e) => e,
    })?;
    Ok((v, account))
}

enum HttpErr {
    /// 401 / 403: the login was refused.
    Refused(String),
    Other(String),
}

/// GET `url` with curl, its config (headers) on stdin; the 200 body as JSON.
fn curl_json(url: &str, cfg: &str) -> Result<Value, HttpErr> {
    let other = |e: String| HttpErr::Other(e);
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "10",
            "-K",
            "-",
            "-w",
            "\n%{http_code}",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| other(format!("curl: {e}")))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| other("curl: no stdin".into()))?;
        stdin
            .write_all(cfg.as_bytes())
            .map_err(|e| other(format!("curl: {e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| other(format!("curl: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(other(format!("usage request failed: {}", err.trim())));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code.trim() {
        "200" => {
            serde_json::from_str(body).map_err(|e| other(format!("usage endpoint: bad JSON ({e})")))
        }
        c @ ("401" | "403") => Err(HttpErr::Refused(c.to_string())),
        "429" => Err(other("usage endpoint is rate-limited (HTTP 429)".into())),
        c => Err(other(format!("usage endpoint answered HTTP {c}"))),
    }
}

// --- OpenAI (Codex's ChatGPT login) ------------------------------------------------

struct OpenAiCreds {
    token: String,
    account_id: Option<String>,
}

/// Codex's ChatGPT login, or `None` (no Codex, or an API-key login: no plan limits).
fn openai_credentials(codex_home: &Path) -> Option<OpenAiCreds> {
    let text = std::fs::read_to_string(codex_home.join("auth.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let t = &v["tokens"];
    let token = t["access_token"].as_str().filter(|s| !s.is_empty())?;
    if v["auth_mode"].as_str().is_some_and(|m| m != "chatgpt") {
        return None;
    }
    Some(OpenAiCreds {
        token: token.to_string(),
        account_id: t["account_id"].as_str().map(str::to_string),
    })
}

fn fetch_openai(c: &OpenAiCreds) -> Result<Value, String> {
    let mut cfg = format!(
        "header = \"Authorization: Bearer {}\"\n\
         header = \"User-Agent: fleet/{}\"\n",
        c.token,
        env!("CARGO_PKG_VERSION")
    );
    if let Some(id) = &c.account_id {
        cfg.push_str(&format!("header = \"ChatGPT-Account-Id: {id}\"\n"));
    }
    curl_json(OPENAI_ENDPOINT, &cfg).map_err(|e| match e {
        HttpErr::Refused(code) => format!(
            "ChatGPT usage endpoint refused the Codex login (HTTP {code}) — run `codex` once to renew it"
        ),
        HttpErr::Other(e) => e,
    })
}

fn parse_openai(v: &Value, now: DateTime<Utc>) -> Usage {
    let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let plan = s(&v["plan_type"]);
    let account = Account {
        uuid: s(&v["account_id"]),
        email: s(&v["email"]),
        organization: None,
        plan_label: plan.as_deref().map(|p| match p {
            "plus" => "ChatGPT Plus".to_string(),
            "pro" => "ChatGPT Pro".to_string(),
            p => format!("ChatGPT {}", p[..1].to_uppercase() + &p[1..]),
        }),
        plan,
        tier: None,
    };
    let mut limits = openai_windows(&v["rate_limit"], None);
    limits.extend(openai_windows(
        &v["code_review_rate_limit"],
        Some("Code review"),
    ));
    if let Some(extra) = v["additional_rate_limits"].as_array() {
        for l in extra {
            let name = s(&l["limit_name"]).or_else(|| s(&l["metered_feature"]));
            limits.extend(openai_windows(
                &l["rate_limit"],
                Some(name.as_deref().unwrap_or("Other")),
            ));
        }
    }
    let c = &v["credits"];
    let extra_usage = c.is_object().then(|| ExtraUsage {
        enabled: c["has_credits"].as_bool().unwrap_or(false)
            || c["unlimited"].as_bool().unwrap_or(false),
        used: None,
        limit: None,
        currency: None,
        percent: None,
        balance: c["balance"]
            .as_f64()
            .or_else(|| c["balance"].as_str().and_then(|b| b.parse().ok())),
    });
    Usage {
        provider: "openai".into(),
        account: (account.uuid.is_some() || account.email.is_some() || account.plan.is_some())
            .then_some(account),
        limits,
        extra_usage,
        fetched_at: now.to_rfc3339(),
        stale: false,
        error: None,
        openai: None,
    }
}

/// A ChatGPT `rate_limit` object's primary (5h) and secondary (weekly) windows.
/// `scope` names a non-default limit (`Code review`, …).
fn openai_windows(rl: &Value, scope: Option<&str>) -> Vec<Limit> {
    let reached = rl["limit_reached"].as_bool().unwrap_or(false);
    ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| {
            let w = &rl[*key];
            let percent = num(&w["used_percent"])?;
            let secs = w["limit_window_seconds"].as_i64().unwrap_or(0);
            let weekly = secs >= 24 * 3600 || (secs == 0 && *key == "secondary_window");
            let window = match secs {
                18000 => "Current session".to_string(),
                604800 => "Weekly".to_string(),
                0 if weekly => "Weekly".to_string(),
                0 => "Current session".to_string(),
                s if s % 86400 == 0 => format!("{}-day", s / 86400),
                s => format!("{}h window", (s as f64 / 3600.0).round()),
            };
            let (kind, group) = match (weekly, scope) {
                (false, None) => ("session", "session"),
                (true, None) => ("weekly_all", "weekly"),
                (false, Some(_)) => ("session_scoped", "session"),
                (true, Some(_)) => ("weekly_scoped", "weekly"),
            };
            Some(Limit {
                kind: kind.into(),
                group: group.into(),
                label: match scope {
                    Some(s) => format!("{window} · {s}"),
                    None => window,
                },
                model: None,
                percent,
                severity: if reached && percent >= 100.0 {
                    "exceeded".into()
                } else {
                    "normal".into()
                },
                resets_at: w["reset_at"]
                    .as_i64()
                    .and_then(|t| DateTime::from_timestamp(t, 0))
                    .map(|t| t.to_rfc3339()),
                active: false,
            })
        })
        .collect()
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
        provider: claude(),
        account,
        limits,
        extra_usage: extra_usage(v),
        fetched_at: now.to_rfc3339(),
        stale: false,
        error: None,
        openai: None,
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
            balance: None,
        });
    }
    if extra.is_object() {
        return Some(ExtraUsage {
            enabled: extra["is_enabled"].as_bool().unwrap_or(false),
            used: num(&extra["used_credits"]),
            limit: num(&extra["monthly_limit"]),
            currency: extra["currency"].as_str().map(str::to_string),
            percent: num(&extra["utilization"]),
            balance: None,
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
        let p = d.path().join("fleet-usage.json");
        let ok = || {
            calls.set(calls.get() + 1);
            Ok(parse(&sample(), None, now()))
        };
        let fail = || {
            calls.set(calls.get() + 1);
            Err("usage endpoint is rate-limited (HTTP 429)".to_string())
        };
        let t0 = now();
        let secs = |s| t0 + chrono::Duration::seconds(s);

        // First read fetches; a second within FRESH_SECS is served from the cache.
        assert!(!get_with(&p, false, t0, &ok).unwrap().stale);
        get_with(&p, false, secs(30), &ok).unwrap();
        assert_eq!(calls.get(), 1);
        // Past FRESH_SECS a failure serves the last read, stale, with the reason.
        let u = get_with(&p, false, secs(90), &fail).unwrap();
        assert!(u.stale);
        assert!(u.error.unwrap().contains("429"));
        assert_eq!(calls.get(), 2);
        // Backing off: even --refresh doesn't ask again.
        assert!(get_with(&p, true, secs(120), &ok).unwrap().stale);
        assert_eq!(calls.get(), 2);
        // After the backoff a good read clears the error.
        assert!(!get_with(&p, false, secs(300), &ok).unwrap().stale);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn a_claude_read_that_failed_with_nothing_cached_is_an_error_provider() {
        // `get_all` keeps a failed Claude read (no limits, an error) when ChatGPT read.
        let failed = Usage {
            error: Some("no Claude Code login".into()),
            ..empty(now())
        };
        assert_eq!(ProviderUsage::claude(&failed).status, "error");
        let mut stale = parse(&sample(), None, now());
        stale.stale = true;
        stale.error = Some("HTTP 429".into());
        assert_eq!(ProviderUsage::claude(&stale).status, "stale");
        assert_eq!(
            ProviderUsage::claude(&parse(&sample(), None, now())).status,
            "ok"
        );
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
            source: "transcripts".into(),
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
                "provider",
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
        let fail = || Err("no Claude Code login found".to_string());
        assert!(get_with(&d.path().join("c.json"), false, now(), &fail).is_err());
    }

    fn openai_sample() -> Value {
        serde_json::json!({
            "account_id": "acct-1", "email": "me@example.com", "plan_type": "plus",
            "rate_limit": {
                "allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 12, "limit_window_seconds": 18000,
                                   "reset_after_seconds": 600, "reset_at": 1790000000},
                "secondary_window": {"used_percent": 40, "limit_window_seconds": 604800,
                                     "reset_after_seconds": 6000, "reset_at": 1790500000}
            },
            "code_review_rate_limit": null,
            "additional_rate_limits": null,
            "credits": {"has_credits": true, "unlimited": false, "balance": "4.5"}
        })
    }

    #[test]
    fn codex_s_provider_entry_comes_from_the_chatgpt_read_when_it_has_numbers() {
        let transcripts = || ProviderUsage {
            id: "codex".into(),
            label: "Codex".into(),
            status: "ok".into(),
            plan: Some("Plus".into()),
            limits: vec![],
            fetched_at: None,
            observed_at: Some("2026-10-16T10:30:00Z".into()),
            error: None,
            credits: None,
            source: "transcripts".into(),
        };
        let mut claude = parse(&sample(), None, now());
        claude.openai = Some(Box::new(parse_openai(&openai_sample(), now())));
        let r = Report::new(claude.clone(), transcripts());
        let codex = &r.providers[1];
        assert_eq!(
            (
                codex.id.as_str(),
                codex.source.as_str(),
                codex.status.as_str()
            ),
            ("codex", "endpoint", "ok")
        );
        assert_eq!(codex.plan.as_deref(), Some("ChatGPT Plus"));
        assert_eq!(codex.observed_at, codex.fetched_at);
        let kinds: Vec<_> = codex.limits.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, ["session", "weekly_all"]);
        let c = codex.credits.as_ref().unwrap();
        assert_eq!((c.has_credits, c.balance.as_deref()), (true, Some("4.5")));
        assert_eq!(r.providers[0].source, "endpoint");

        // A ChatGPT read with no numbers (it failed, nothing cached), or none at all: the transcripts.
        claude.openai = Some(Box::new(Usage {
            provider: "openai".into(),
            error: Some("not logged in".into()),
            ..empty(now())
        }));
        assert_eq!(
            Report::new(claude.clone(), transcripts()).providers[1].source,
            "transcripts"
        );
        claude.openai = None;
        assert_eq!(
            Report::new(claude, transcripts()).providers[1].source,
            "transcripts"
        );
    }

    #[test]
    fn parses_openai_windows() {
        let u = parse_openai(&openai_sample(), now());
        assert_eq!(u.provider, "openai");
        let a = u.account.unwrap();
        assert_eq!(
            (a.uuid.as_deref(), a.plan_label.as_deref()),
            (Some("acct-1"), Some("ChatGPT Plus"))
        );
        let rows: Vec<_> = u
            .limits
            .iter()
            .map(|l| (l.kind.as_str(), l.label.as_str(), l.percent))
            .collect();
        assert_eq!(
            rows,
            [
                ("session", "Current session", 12.0),
                ("weekly_all", "Weekly", 40.0)
            ]
        );
        assert_eq!(
            u.limits[0].resets_at.as_deref(),
            Some("2026-09-21T14:13:20+00:00")
        );
        let x = u.extra_usage.unwrap();
        assert_eq!((x.enabled, x.balance), (true, Some(4.5)));
    }

    #[test]
    fn openai_needs_a_chatgpt_login() {
        let d = tempfile::tempdir().unwrap();
        assert!(openai_credentials(d.path()).is_none());
        let auth = d.path().join("auth.json");
        std::fs::write(
            &auth,
            r#"{"auth_mode":"apikey","tokens":{"access_token":"t"}}"#,
        )
        .unwrap();
        assert!(openai_credentials(d.path()).is_none());
        std::fs::write(
            &auth,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"t","account_id":"a"}}"#,
        )
        .unwrap();
        let c = openai_credentials(d.path()).unwrap();
        assert_eq!(
            (c.token.as_str(), c.account_id.as_deref()),
            ("t", Some("a"))
        );
    }

    #[test]
    fn old_caches_read_as_claude() {
        let mut v = serde_json::to_value(parse(&sample(), None, now())).unwrap();
        v.as_object_mut().unwrap().remove("provider");
        let u: Usage = serde_json::from_value(v).unwrap();
        assert_eq!(u.provider, "claude");
        assert!(u.openai.is_none());
    }
}
