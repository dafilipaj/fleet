//! The shared, per-machine config: `$FLEET_CONFIG`, else
//! `${XDG_CONFIG_HOME:-~/.config}/fleet/config.json`.
//!
//! Owned by the CLI (`fleet init`, `fleet config set`), read by every UI (the web
//! server reads the same file). A missing file is not an error: everything works
//! locally as a single host. Unknown keys are preserved on every rewrite — writes
//! go through the raw JSON value, never through the typed view.
//!
//! ```json
//! {
//!   "version": 1,
//!   "self": "laptop",
//!   "defaultHost": "workstation",
//!   "hosts": {
//!     "laptop":      { "ssh": null,          "web": "http://100.x.y.z:7777" },
//!     "workstation": { "ssh": "workstation", "web": "http://100.x.y.z:7777" }
//!   },
//!   "web":  { "port": 7777, "bind": "0.0.0.0", "dir": null },
//!   "tmux": null,
//!   "spawnDirs": [ { "label": "Work", "paths": { "laptop": "~/Code/app" }, "hidden": false } ]
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub const VERSION: u64 = 1;
/// The host name used for this machine when there is no config.
pub const DEFAULT_SELF: &str = "local";
pub const DEFAULT_WEB_PORT: u16 = 7777;

/// Where the config lives (whether or not it exists).
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("FLEET_CONFIG").filter(|p| !p.is_empty()) {
        return PathBuf::from(crate::core::tools::expand_tilde(&p.to_string_lossy()));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(crate::core::tools::expand_tilde(&p.to_string_lossy())))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));
    base.join("fleet").join("config.json")
}

/// One configured machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Host {
    /// ssh destination (an `~/.ssh/config` alias or `user@host`); `None` for self.
    pub ssh: Option<String>,
    /// Base URL of the web UI on that host.
    pub web: Option<String>,
    /// Path to `fleet` on that host; `None` → `~/.local/bin/fleet`, then PATH.
    pub fleet_bin: Option<String>,
    /// ssh destination the web UI's "Open in editor" links use for this host (read by the web
    /// server only); `None` → `ssh`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_ssh: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebConfig {
    pub port: Option<u16>,
    pub bind: Option<String>,
    /// Where the web app (`server.mjs`) lives. `None` → auto-detect.
    pub dir: Option<String>,
    /// The `node` binary that runs the web app. `None` → a stable install
    /// (`/opt/homebrew/bin/node`, `/usr/local/bin/node`), then PATH.
    pub node: Option<String>,
    /// "Open in editor": `"vscode"` (default) | `"cursor"` | `null` (the web UI hides it).
    /// Kept loose so a bad value never breaks loading the config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<Value>,
}

impl WebConfig {
    /// The editor's command-line launcher for `fleet brief --open`: `cursor` for
    /// `web.editor: "cursor"`, else `code`.
    pub fn editor_command(&self) -> &'static str {
        match self.editor.as_ref().and_then(Value::as_str) {
            Some("cursor") => "cursor",
            _ => "code",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpawnDir {
    pub label: Option<String>,
    /// host name → directory on that host.
    pub paths: Map<String, Value>,
    /// Allowed for spawns (the web server's allow-list) but not offered in the New session menu.
    /// Read leniently: anything but `true` is `false`, so a hand-edited value never breaks the config.
    #[serde(
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "true_or_false"
    )]
    pub hidden: bool,
}

fn true_or_false<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool() == Some(true))
}

/// `tui` — dashboard preferences that survive restarts.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct UiConfig {
    /// `"1"`, `"2"` or `"auto"` — how tall one session's item is. `"2"` (and
    /// `"auto"`) is the full item; `"1"` is the compact single line `z` toggles into.
    pub rows: Option<String>,
    /// Mouse/tap capture in the `watch` TUI. Defaults to on.
    pub mouse: Option<bool>,
}

/// `naming` — how `N`/`Ctrl-N` and `fleet name` behave. All keys are opt-*out*.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct NamingConfig {
    /// Generate names at all. `false` leaves only the branch/title heuristic.
    pub enabled: Option<bool>,
    /// Model passed to `claude -p --model`.
    pub model: Option<String>,
    /// Rename the session's tmux session to match a confirmed Claude rename.
    #[serde(alias = "sync_tmux")]
    pub sync_tmux: Option<bool>,
    /// Title every session in the `watch` dashboard automatically.
    #[serde(alias = "auto_title")]
    pub auto_title: Option<bool>,
}

impl NamingConfig {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn model(&self) -> String {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("haiku")
            .to_string()
    }
    pub fn sync_tmux(&self) -> bool {
        self.sync_tmux.unwrap_or(true)
    }
    pub fn auto_title(&self) -> bool {
        self.auto_title.unwrap_or(true)
    }
}

/// `grouping` — the smart-grouping pass behind the web Board view
/// (`fleet group`). Opt-*out* like naming for the CLI; the web server only runs
/// it on a schedule when `web.grouping.enabled` is set.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig {
    /// Call the model at all. `false` leaves the group-by-repository fallback.
    pub enabled: Option<bool>,
    /// Model passed to `claude -p --model`.
    pub model: Option<String>,
    /// The host whose web server runs the pass for the whole fleet (read by the
    /// web server; peers proxy `/api/groups` to it).
    pub host: Option<String>,
    /// Minimum minutes between consolidation passes (merge/rename).
    pub consolidate_minutes: Option<f64>,
}

impl GroupingConfig {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn model(&self) -> String {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("haiku")
            .to_string()
    }
    pub fn consolidate_every_ms(&self) -> i64 {
        match self.consolidate_minutes {
            Some(m) if m.is_finite() && m >= 1.0 => (m * 60_000.0) as i64,
            _ => crate::core::grouping::CONSOLIDATE_EVERY_MS,
        }
    }
}

/// `stacks` — session stacks (`fleet stack`, docs/architecture.md → "Session stacks").
/// Opt-*out*: `enabled: false` never calls the model (a new stack gets the skeleton brief).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct StacksConfig {
    /// Call the model when a stack is created. `false` = skeleton StackBrief only.
    pub enabled: Option<bool>,
    /// Model passed to `claude -p --model` (default `sonnet`).
    pub model: Option<String>,
}

impl StacksConfig {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn model(&self) -> String {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("sonnet")
            .to_string()
    }
}

/// The typed view of the config file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub version: Option<u64>,
    #[serde(rename = "self")]
    pub self_name: Option<String>,
    pub default_host: Option<String>,
    /// Kept as a JSON map so the file's host order is preserved.
    pub hosts: Map<String, Value>,
    pub web: WebConfig,
    /// tmux binary; `None` → PATH (+ Homebrew fallbacks).
    pub tmux: Option<String>,
    /// This machine's own `fleet` binary (used by the web server).
    pub fleet_bin: Option<String>,
    /// The command that launches Claude Code in a spawned session (default `claude`).
    pub claude: Option<String>,
    /// `harnesses.<codex|pi>.command` — how the other harnesses launch. Raw JSON,
    /// read leniently (see [`Config::harness_command`]).
    pub harnesses: Value,
    /// `ghIdentities.<name>` — GitHub Apps a spawned session can act as. Raw JSON, read
    /// leniently (see [`crate::core::gh_identity`]).
    pub gh_identities: Value,
    pub spawn_dirs: Vec<SpawnDir>,
    pub tui: UiConfig,
    pub naming: NamingConfig,
    pub grouping: GroupingConfig,
    pub stacks: StacksConfig,
    /// `restore` — session recovery (`onBoot` is the web server's; `keepClosedDays` ours).
    /// Raw JSON, read leniently: a bad value here must not cost the rest of the config.
    pub restore: Value,
}

impl Config {
    /// Which host entry this machine is.
    pub fn self_name(&self) -> String {
        self.self_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_SELF)
            .to_string()
    }

    /// `harnesses.<id>.command`, when it is a non-blank string.
    pub fn harness_command(&self, id: &str) -> Option<String> {
        self.harnesses
            .get(id)?
            .get("command")?
            .as_str()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
    }

    /// Target for tmux-session commands when no `--host` is given.
    pub fn default_host(&self) -> String {
        self.default_host
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.self_name())
    }

    /// Configured hosts in file order. Malformed entries are skipped here and
    /// reported by [`Config::problems`].
    pub fn hosts(&self) -> Vec<(String, Host)> {
        self.hosts
            .iter()
            .filter_map(|(k, v)| {
                serde_json::from_value::<Host>(v.clone())
                    .ok()
                    .map(|h| (k.clone(), h))
            })
            .collect()
    }

    pub fn host(&self, name: &str) -> Option<Host> {
        self.hosts
            .get(name)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Names of every host, `self` first even when it isn't listed.
    pub fn host_names(&self) -> Vec<String> {
        let me = self.self_name();
        let mut out = vec![me.clone()];
        out.extend(self.hosts.keys().filter(|k| **k != me).cloned());
        out
    }

    /// Hosts fleet can run commands on: this machine plus every host with an
    /// ssh destination (web-only peers are left out).
    pub fn ssh_host_names(&self) -> Vec<String> {
        let me = self.self_name();
        self.host_names()
            .into_iter()
            .filter(|n| {
                *n == me
                    || self
                        .hosts
                        .get(n)
                        .and_then(|h| h.get("ssh"))
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| !s.trim().is_empty())
            })
            .collect()
    }

    /// `restore.keepClosedDays`: how long a session closed within a boot stays in the
    /// recently-closed list. Missing or not a number ≥ 0 → the default; 0 turns it off.
    pub fn keep_closed_days(&self) -> f64 {
        self.restore
            .get("keepClosedDays")
            .and_then(Value::as_f64)
            .filter(|d| d.is_finite() && *d >= 0.0)
            .unwrap_or(crate::core::snapshot::DEFAULT_KEEP_CLOSED_DAYS)
    }

    pub fn web_port(&self) -> u16 {
        self.web.port.unwrap_or(DEFAULT_WEB_PORT)
    }

    /// Everything wrong with the config that would make a feature misbehave.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(v) = self.version
            && v != VERSION
        {
            out.push(format!("version {v} is not supported (expected {VERSION})"));
        }
        let me = self.self_name();
        if self.self_name.is_none() {
            out.push("`self` is not set — this machine has no name".into());
        } else if !self.hosts.is_empty() && !self.hosts.contains_key(&me) {
            out.push(format!(
                "`self` is \"{me}\" but hosts has no \"{me}\" entry"
            ));
        }
        if let Some(d) = self.default_host.as_deref()
            && d != me
            && !self.hosts.contains_key(d)
        {
            out.push(format!("`defaultHost` \"{d}\" is not a configured host"));
        }
        for (name, v) in &self.hosts {
            match serde_json::from_value::<Host>(v.clone()) {
                Err(e) => out.push(format!("hosts.{name}: {e}")),
                Ok(h) => {
                    // No ssh is fine for a web-only peer; a host with neither is unreachable.
                    if *name != me
                        && h.ssh.as_deref().is_none_or(|s| s.trim().is_empty())
                        && h.web.is_none()
                    {
                        out.push(format!(
                            "hosts.{name} has neither `ssh` nor `web` — it can't be reached"
                        ));
                    }
                    if let Some(w) = &h.web
                        && !(w.starts_with("http://") || w.starts_with("https://"))
                    {
                        out.push(format!("hosts.{name}.web must be an http(s) URL"));
                    }
                }
            }
        }
        out
    }

    /// The spawn directory list for one host: `(label, path)` with `~` kept, hidden entries
    /// included (see [`Config::spawn_dir_entries_for`] to tell them apart).
    pub fn spawn_dirs_for(&self, host: &str) -> Vec<(String, String)> {
        self.spawn_dir_entries_for(host)
            .into_iter()
            .map(|(label, path, _)| (label, path))
            .collect()
    }

    /// Like [`Config::spawn_dirs_for`], plus each entry's `hidden` flag.
    pub fn spawn_dir_entries_for(&self, host: &str) -> Vec<(String, String, bool)> {
        self.spawn_dirs
            .iter()
            .filter_map(|d| {
                let p = d.paths.get(host)?.as_str()?.to_string();
                let label = d.label.clone().unwrap_or_else(|| {
                    Path::new(&p)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.clone())
                });
                Some((label, p, d.hidden))
            })
            .collect()
    }
}

/// Where `fleet install` puts the web app on a host.
pub const INSTALLED_WEB_DIR: &str = "~/.local/share/fleet/web";

/// Find the web app without consulting the config: a `web/server.mjs` next to
/// (an ancestor of) this binary — a repo checkout — then the install dir.
pub fn detect_web_dir() -> Option<PathBuf> {
    let has_server = |d: &Path| d.join("server.mjs").is_file();
    if let Ok(exe) = std::env::current_exe() {
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        for anc in exe.ancestors().skip(1) {
            let cand = anc.join("web");
            if has_server(&cand) {
                return Some(cand);
            }
        }
    }
    let installed = PathBuf::from(crate::core::tools::expand_tilde(INSTALLED_WEB_DIR));
    has_server(&installed).then_some(installed)
}

/// The web app directory: config `web.dir`, else [`detect_web_dir`].
pub fn web_dir(cfg: &Config) -> Option<PathBuf> {
    if let Some(d) = cfg
        .web
        .dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        return Some(PathBuf::from(crate::core::tools::expand_tilde(d)));
    }
    detect_web_dir()
}

/// Everything one read of the config file yields.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub path: PathBuf,
    pub exists: bool,
    pub config: Config,
    /// The file as JSON (an empty object when missing or unreadable).
    pub raw: Value,
    /// `Some(message)` when the file exists but doesn't parse. Loading degrades
    /// to defaults on purpose; this is how the user finds out why.
    pub problem: Option<String>,
}

/// Read and parse the config at `path`.
pub fn load_from(path: &Path) -> Loaded {
    let mut out = Loaded {
        path: path.to_path_buf(),
        raw: Value::Object(Map::new()),
        ..Default::default()
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.exists = true;
            out.problem = Some(format!("cannot read {}: {e}", path.display()));
            return out;
        }
    };
    out.exists = true;
    let raw: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            out.problem = Some(format!("{} is not valid JSON: {e}", path.display()));
            return out;
        }
    };
    if !raw.is_object() {
        out.problem = Some(format!("{} must hold a JSON object", path.display()));
        return out;
    }
    match serde_json::from_value::<Config>(raw.clone()) {
        Ok(c) => out.config = c,
        Err(e) => out.problem = Some(format!("{}: {e}", path.display())),
    }
    out.raw = raw;
    out
}

/// The config for this process, read once.
pub fn load() -> &'static Loaded {
    static LOADED: OnceLock<Loaded> = OnceLock::new();
    LOADED.get_or_init(|| load_from(&path()))
}

/// Shorthand for the typed view.
pub fn get() -> &'static Config {
    &load().config
}

/// Write `raw` to `path` atomically (tmp + rename), pretty-printed.
pub fn write_raw(path: &Path, raw: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(raw)?;
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse a CLI value: valid JSON (`7777`, `true`, `null`, `"x"`, `{..}`) as
/// JSON, anything else as a plain string.
pub fn parse_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

/// Set `a.b.c` in `raw`, creating objects on the way. Unrelated keys are kept.
pub fn set_path(raw: &mut Value, key: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = key.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(Error::Other("empty key".into()));
    }
    if !raw.is_object() {
        *raw = Value::Object(Map::new());
    }
    let mut cur = raw;
    for (i, part) in parts.iter().enumerate() {
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| Error::Other(format!("`{}` is not an object", parts[..i].join("."))))?;
        if i == parts.len() - 1 {
            obj.insert(part.to_string(), value);
            return Ok(());
        }
        let next = obj
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if next.is_null() {
            *next = Value::Object(Map::new());
        }
        cur = next;
    }
    unreachable!()
}

/// Read `a.b.c` from `raw`.
pub fn get_path<'a>(raw: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.')
        .filter(|p| !p.is_empty())
        .try_fold(raw, |cur, part| cur.get(part))
}

/// Patch one key in the on-disk config, leaving everything else alone. Refuses
/// to touch a file that exists but doesn't parse — a hand-edited syntax error
/// must not cost the user the rest of their settings.
pub fn patch(key: &str, value: Value) -> Result<()> {
    patch_at(&path(), key, value)
}

pub fn patch_at(path: &Path, key: &str, value: Value) -> Result<()> {
    let loaded = load_from(path);
    if loaded.exists
        && loaded.raw.as_object().is_none_or(|o| o.is_empty())
        && loaded.problem.is_some()
    {
        return Err(Error::Other(format!(
            "not rewriting a config that doesn't parse: {}",
            loaded.problem.unwrap_or_default()
        )));
    }
    let mut raw = loaded.raw;
    if !loaded.exists {
        set_path(&mut raw, "version", Value::from(VERSION))?;
    }
    set_path(&mut raw, key, value)?;
    write_raw(path, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "version": 1,
            "self": "laptop",
            "defaultHost": "workstation",
            "hosts": {
                "laptop": { "ssh": null, "web": "http://100.x.y.z:7777" },
                "workstation": { "ssh": "workstation", "web": "http://100.x.y.z:7777" }
            },
            "web": { "port": 7777, "bind": "0.0.0.0", "dir": null },
            "tmux": null,
            "spawnDirs": [ { "label": "Work", "paths": { "laptop": "~/Code/app", "workstation": "~/src/app" } } ],
            "somethingNew": { "keep": true }
        })
    }

    #[test]
    fn spawn_dirs_carry_hidden_leniently() {
        let mut raw = sample();
        raw["spawnDirs"] = json!([
            { "label": "Work", "paths": { "laptop": "~/Code/app" } },
            { "label": "Brain", "paths": { "laptop": "~/brain" }, "hidden": true },
            { "label": "Odd", "paths": { "laptop": "~/odd" }, "hidden": "yes" }
        ]);
        let c: Config = serde_json::from_value(raw).unwrap();
        assert_eq!(
            c.spawn_dir_entries_for("laptop")
                .into_iter()
                .map(|(l, _, h)| (l, h))
                .collect::<Vec<_>>(),
            vec![
                ("Work".to_string(), false),
                ("Brain".to_string(), true),
                ("Odd".to_string(), false)
            ]
        );
        assert_eq!(
            c.spawn_dirs_for("laptop").len(),
            3,
            "hidden ones are still listed"
        );
    }

    #[test]
    fn parses_the_documented_shape() {
        let c: Config = serde_json::from_value(sample()).unwrap();
        assert_eq!(c.self_name(), "laptop");
        assert_eq!(c.default_host(), "workstation");
        let hosts = c.hosts();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].0, "laptop", "file order is kept");
        assert_eq!(hosts[1].1.ssh.as_deref(), Some("workstation"));
        assert_eq!(c.web_port(), 7777);
        assert_eq!(
            c.spawn_dirs_for("workstation"),
            vec![("Work".to_string(), "~/src/app".to_string())]
        );
        assert!(c.problems().is_empty(), "{:?}", c.problems());
    }

    #[test]
    fn empty_config_is_a_single_local_host() {
        let c = Config::default();
        assert_eq!(c.self_name(), DEFAULT_SELF);
        assert_eq!(c.default_host(), DEFAULT_SELF);
        assert_eq!(c.host_names(), vec![DEFAULT_SELF.to_string()]);
    }

    #[test]
    fn problems_are_reported() {
        let c: Config = serde_json::from_value(json!({
            "version": 2,
            "self": "a",
            "defaultHost": "nope",
            "hosts": { "b": { "ssh": null, "web": "ftp://x" }, "c": { "ssh": null } }
        }))
        .unwrap();
        let p = c.problems().join("\n");
        assert!(p.contains("version 2"), "{p}");
        assert!(p.contains("no \"a\" entry"), "{p}");
        assert!(p.contains("defaultHost"), "{p}");
        assert!(
            !p.contains("hosts.b has neither"),
            "web-only peer is fine: {p}"
        );
        assert!(p.contains("hosts.c has neither"), "{p}");
        assert!(p.contains("http(s)"), "{p}");
    }

    #[test]
    fn ssh_host_names_skip_web_only_peers() {
        let c: Config = serde_json::from_value(json!({
            "self": "ws",
            "hosts": {
                "ws": { "ssh": null },
                "laptop": { "ssh": null, "web": "http://laptop:7777" },
                "box": { "ssh": "box" }
            }
        }))
        .unwrap();
        assert_eq!(
            c.ssh_host_names(),
            vec!["ws".to_string(), "box".to_string()]
        );
    }

    #[test]
    fn set_path_preserves_unknown_keys() {
        let mut raw = sample();
        set_path(&mut raw, "tui.rows", json!("1")).unwrap();
        set_path(&mut raw, "hosts.workstation.ssh", json!("devbox2")).unwrap();
        set_path(&mut raw, "tmux", json!("/opt/tmux")).unwrap();
        assert_eq!(raw["somethingNew"]["keep"], true);
        assert_eq!(raw["tui"]["rows"], "1");
        assert_eq!(raw["hosts"]["workstation"]["ssh"], "devbox2");
        assert_eq!(raw["hosts"]["workstation"]["web"], "http://100.x.y.z:7777");
        assert_eq!(get_path(&raw, "tmux"), Some(&json!("/opt/tmux")));
        // Setting through a scalar is an error, not silent data loss.
        assert!(set_path(&mut raw, "tmux.inner", json!(1)).is_err());
    }

    #[test]
    fn values_parse_as_json_or_string() {
        assert_eq!(parse_value("7777"), json!(7777));
        assert_eq!(parse_value("null"), Value::Null);
        assert_eq!(parse_value("true"), json!(true));
        assert_eq!(parse_value("devbox"), json!("devbox"));
        assert_eq!(parse_value("http://x:1"), json!("http://x:1"));
    }

    #[test]
    fn patch_creates_then_preserves() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/config.json");
        patch_at(&p, "tui.rows", json!("2")).unwrap();
        let l = load_from(&p);
        assert!(l.exists && l.problem.is_none());
        assert_eq!(l.raw["version"], 1);
        assert_eq!(l.config.tui.rows.as_deref(), Some("2"));

        std::fs::write(&p, serde_json::to_string(&sample()).unwrap()).unwrap();
        patch_at(&p, "tui.mouse", json!(false)).unwrap();
        let l = load_from(&p);
        assert_eq!(l.raw["somethingNew"]["keep"], true);
        assert_eq!(l.config.tui.mouse, Some(false));
    }

    #[test]
    fn patch_replaces_spawn_dirs_in_place() {
        // The web UI's Settings → Start directories writes through `fleet config set spawnDirs`.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(&p, serde_json::to_string(&sample()).unwrap()).unwrap();
        let keys = |raw: &Value| raw.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let before = keys(&load_from(&p).raw);
        let list = r#"[{"label":"Fleet","paths":{"laptop":"~/Code/fleet"}}]"#;
        patch_at(&p, "spawnDirs", parse_value(list)).unwrap();
        let l = load_from(&p);
        assert_eq!(keys(&l.raw), before, "key order kept");
        assert_eq!(l.raw["somethingNew"]["keep"], true);
        assert_eq!(
            l.config.spawn_dirs_for("laptop"),
            vec![("Fleet".to_string(), "~/Code/fleet".to_string())]
        );
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn patch_refuses_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(patch_at(&p, "tui.rows", json!("1")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
    }
}
