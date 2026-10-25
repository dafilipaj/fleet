// Shared fleet config (owned by the `fleet` CLI, read here).
//
// Path: $FLEET_CONFIG, else ${XDG_CONFIG_HOME:-~/.config}/fleet/config.json.
// Shape (v1) — see config.example.json and ARCHITECTURE.md:
//   { version, self, defaultHost, hosts: { name: { ssh, web, editorSsh } },
//     web: { port, bind, dir, ui, editor, editorSsh, quickReplies, models: [ { id, label } ],
//            autoName: { enabled, intervalMinutes },
//            grouping: { enabled, intervalMinutes }, uploads: { dir, maxMB, retentionDays },
//            memos: { dir, audioDir, model, vocabulary },
//            briefs: { enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour, … },
//            stacks: { syncMinutes } },
//     stacks: { enabled, model },         (read by the CLI; the server only reports them)
//     grouping: { enabled, model, host },   (enabled/model are read by the CLI)
//     inbox: { host, scanMinutes, workHours, workDays, model, briefModel, briefMinutes, language, comms },
//     tmux, fleetBin, claude, spawnDirs: [ { label, paths: { host: dir }, hidden? } ] }
//
// A missing config file is not an error: the server runs as a single local host
// ("local", 127.0.0.1, no peers). A present-but-broken file is a hard error.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { DEFAULT_EDITOR, EDITORS } from './editor.mjs';

export const DEFAULT_PORT = 7777;
export const DEFAULT_SELF = 'local';
export const SUPPORTED_VERSION = 1;
export const DEFAULT_AUTONAME_MINUTES = 5;
export const DEFAULT_GROUPING_MINUTES = 10;
export const DEFAULT_UPLOADS_DIR = '~/.local/share/fleet/uploads';
export const DEFAULT_UPLOAD_MAX_MB = 100;
export const DEFAULT_MEMOS_DIR = '~/.local/share/fleet/memos';
export const DEFAULT_MEMO_AUDIO_DIR = '~/.local/share/fleet/memo-audio';
export const DEFAULT_UPLOAD_RETENTION_DAYS = 14;
export const DEFAULT_STACKS_SYNC_MINUTES = 2;
export const DEFAULT_STACKS_MODEL = 'sonnet';

/** web.briefs defaults (lib/briefs.mjs). Off by default: it spends model calls. */
export const DEFAULT_BRIEFS = Object.freeze({
  enabled: false,
  model: 'haiku',
  idleMs: 60 * 1000, // idle this long before a background pass looks at a session
  minIntervalMs: 15 * 60 * 1000, // per session, between two background model calls
  maxDeltaChars: 12000, // conversation fed to one call
  maxCallsPerHour: 12, // this host, background + manual
  minNewTurns: 2, // new user prompts needed for a background call …
  minNewChars: 2000, // … or this much new conversation text
  maxBriefChars: 3000, // the previous Summary + Plan fed back
});

/** inbox defaults (lib/inbox.mjs). Runs only on the host named by `inbox.host`. */
export const DEFAULT_INBOX = Object.freeze({
  scanMinutes: 60, // Slack + email scan (a `claude -p` with the claude.ai connectors), in work hours
  workHours: [8, 18], // local time, [from, to)
  workDays: [0, 1, 2, 3, 4, 5, 6], // 0 = Sunday
  model: 'haiku', // the Slack + email scan
  briefModel: 'haiku', // the brief over all items
  briefMinutes: 45, // at least this long between two briefs (only when the items changed)
  language: 'English', // the brief's language
  comms: true, // false: no Slack + email scan (no model calls but the brief)
});

/** Where brief files live: $FLEET_BRIEFS_DIR, else ${XDG_STATE_HOME:-~/.local/state}/fleet/briefs. */
export function briefsDir(env = process.env, home = os.homedir()) {
  if (env.FLEET_BRIEFS_DIR) return path.resolve(expandHome(env.FLEET_BRIEFS_DIR, home));
  return path.join(fleetStateDir(env, home), 'briefs');
}

/** fleet's per-machine state: `${XDG_STATE_HOME:-~/.local/state}/fleet`. */
export function fleetStateDir(env = process.env, home = os.homedir()) {
  const base = env.XDG_STATE_HOME ? expandHome(env.XDG_STATE_HOME, home) : path.join(home, '.local', 'state');
  return path.join(base, 'fleet');
}

/** A model id typed after `--model` in a shell: letters, digits and `._[]-` only. */
/** `ghIdentities` → `[{ name, login }]`, entries with a usable name and a `command` only. */
export function ghIdentitiesOf(raw) {
  const m = raw?.ghIdentities;
  if (!m || typeof m !== 'object' || Array.isArray(m)) return [];
  return Object.entries(m)
    .filter(([name, v]) => GH_IDENTITY_RE.test(name) && typeof v?.command === 'string' && v.command.trim())
    .map(([name, v]) => ({ name, login: typeof v.login === 'string' && v.login.trim() ? v.login.trim() : name }));
}

export const GH_IDENTITY_RE = /^[A-Za-z0-9_-]{1,64}$/;

/** `harnesses.<id>.command` when it is a non-blank string, else the bare id. Read leniently. */
function harnessCommand(raw, id, home) {
  const c = raw?.harnesses?.[id]?.command;
  return typeof c === 'string' && c.trim() ? expandHome(c.trim(), home) : id;
}

export const MODEL_ID_RE = /^[A-Za-z0-9._[\]\/:-]{1,100}$/;

/** The New session form's model picker when `web.models` is unset ('' = no flag, Claude's default). */
export const DEFAULT_MODELS = [
  { id: '', label: 'Default' },
  { id: 'claude-fable-5-1', label: 'Fable 5.1' },
  { id: 'claude-opus-5-5', label: 'Opus 5.5' },
  { id: 'claude-sonnet-5', label: 'Sonnet 5' },
];

const BIN_FALLBACK_DIRS = ['/opt/homebrew/bin', '/usr/local/bin'];

/** Expand a leading `~` / `~/` against `home`. Other paths pass through. */
export function expandHome(p, home = os.homedir()) {
  if (typeof p !== 'string') return p;
  if (p === '~') return home;
  if (p.startsWith('~/')) return path.join(home, p.slice(2));
  return p;
}

/** Where the shared config lives. */
export function configPath(env = process.env, home = os.homedir()) {
  if (env.FLEET_CONFIG) return path.resolve(expandHome(env.FLEET_CONFIG, home));
  const base = env.XDG_CONFIG_HOME ? expandHome(env.XDG_CONFIG_HOME, home) : path.join(home, '.config');
  return path.join(base, 'fleet', 'config.json');
}

/** Prepend the usual Homebrew / user bin dirs so children launched by launchd find binaries. */
export function ensurePath(env = process.env, home = os.homedir()) {
  const extra = ['/opt/homebrew/bin', path.join(home, '.local', 'bin'), '/usr/local/bin'];
  const current = (env.PATH || '').split(':').filter(Boolean);
  const merged = [...extra.filter((p) => !current.includes(p)), ...current];
  env.PATH = merged.join(':');
  return env.PATH;
}

/**
 * A UTF-8 locale for every child (tmux, the fleet CLI, `claude -p`). launchd and similar
 * start the server with none, and then tmux escapes its `-F` output (tabs become `_`,
 * so no pane matched and tmux sessions had no handle) and mangles UTF-8 in send-keys.
 */
export function ensureUtf8Locale(env = process.env) {
  const current = env.LC_ALL || env.LC_CTYPE || env.LANG || '';
  if (!/utf-?8/i.test(current)) env[env.LC_ALL ? 'LC_ALL' : 'LC_CTYPE'] = 'en_US.UTF-8';
  return env;
}

function isExecutable(file, fsImpl = fs) {
  try {
    fsImpl.accessSync(file, fs.constants.X_OK);
    return fsImpl.statSync(file).isFile();
  } catch {
    return false;
  }
}

/**
 * Resolve a binary: explicit path (config/env) wins; else first hit on PATH, then the
 * extra dirs. Returns an absolute path, or the bare `name` so execFile reports ENOENT.
 */
export function resolveBinary(name, { explicit = null, env = process.env, extraDirs = [], home = os.homedir(), fsImpl = fs } = {}) {
  if (typeof explicit === 'string' && explicit.trim()) return expandHome(explicit.trim(), home);
  const dirs = [...(env.PATH || '').split(':').filter(Boolean), ...BIN_FALLBACK_DIRS, ...extraDirs];
  for (const dir of dirs) {
    const candidate = path.join(expandHome(dir, home), name);
    if (isExecutable(candidate, fsImpl)) return candidate;
  }
  return name;
}

/** `fleet` binary: FLEET_BIN env > config.fleetBin > PATH > ~/.local/bin > ~/.cargo/bin. */
export function resolveFleetBin(raw = {}, { env = process.env, home = os.homedir(), fsImpl = fs } = {}) {
  return resolveBinary('fleet', {
    explicit: env.FLEET_BIN || raw.fleetBin,
    env,
    home,
    fsImpl,
    extraDirs: [path.join(home, '.local', 'bin'), path.join(home, '.cargo', 'bin')],
  });
}

function parsePort(value, label) {
  const port = typeof value === 'string' ? Number.parseInt(value, 10) : Number(value);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) throw new Error(`${label} invalid: ${value}`);
  return port;
}

function isObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

/**
 * Map one `spawnDirs` entry to the `{ label, path }` this host offers (or null when the
 * entry has no path for this host). Accepts `{ label, paths: { host: dir } }` (canonical)
 * and `{ label, path }` (same dir on every host).
 */
function spawnDirFor(entry, self, home) {
  if (typeof entry === 'string') entry = { path: entry };
  if (!isObject(entry)) throw new Error('config.spawnDirs entries must be objects');
  let dir = null;
  if (isObject(entry.paths)) dir = entry.paths[self] ?? null;
  else if (typeof entry.path === 'string') dir = entry.path;
  if (dir == null) return null;
  if (typeof dir !== 'string' || !dir.trim()) throw new Error('config.spawnDirs paths must be non-empty strings');
  const abs = expandHome(dir.trim(), home);
  if (!path.isAbsolute(abs)) throw new Error(`config.spawnDirs path must be absolute or start with ~: ${dir}`);
  const label = typeof entry.label === 'string' && entry.label.trim() ? entry.label.trim() : path.basename(abs);
  return { label, path: path.normalize(abs), hidden: entry.hidden === true };
}

function spawnDirsOf(list, self, home) {
  if (list == null) list = [];
  if (!Array.isArray(list)) throw new Error('config.spawnDirs must be an array');
  return list.map((e) => spawnDirFor(e, self, home)).filter(Boolean);
}

/**
 * What this host offers for new sessions: the raw `spawnDirs` list → `[{ label, path }]`
 * (absolute, `~` expanded), without the `hidden: true` entries; `[{ label: "Home", path: home }]`
 * when none is offered on this host (also when all of this host's entries are hidden).
 * Also used to hot-reload the list after a Settings edit (lib/spawn-dirs.mjs).
 */
export function spawnDirsFor(list, self, home = os.homedir()) {
  const dirs = spawnDirsOf(list, self, home)
    .filter((d) => !d.hidden)
    .map(({ label, path: p }) => ({ label, path: p }));
  return dirs.length ? dirs : [{ label: 'Home', path: home }];
}

/**
 * This host's `hidden: true` entries → `[{ label, path }]`: allowed for spawns (e.g. an app's
 * embedded panel starting sessions in its own dir) but not offered in the New session menu.
 */
export function hiddenSpawnDirsFor(list, self, home = os.homedir()) {
  return spawnDirsOf(list, self, home)
    .filter((d) => d.hidden)
    .map(({ label, path: p }) => ({ label, path: p }));
}

/** The spawn allow-list: every dir offered (`spawnDirs`) plus the hidden ones. */
export function spawnRoots(config) {
  return [...(config.spawnDirs ?? []), ...(config.hiddenSpawnDirs ?? [])].map((d) => d.path);
}

/** The retired `web.ui` shortcut for the old vanilla UI; now means "the default". */
export const CLASSIC_UI = 'classic';

/**
 * The static UI directory:
 *   explicit path (`web.ui` / FLEET_WEB_UI, `~` expanded) → that directory;
 *   unset (or the retired `"classic"`) → <webRoot>/ui/dist; until it is built the server
 *   answers `/` with a "not built" placeholder.
 */
export function resolveUiDir(uiRaw, { webRoot = null, home = os.homedir() } = {}) {
  if (uiRaw && uiRaw !== CLASSIC_UI) return path.resolve(expandHome(uiRaw, home));
  return webRoot ? path.join(webRoot, 'ui', 'dist') : null;
}

/**
 * Turn the raw shared config into what the server needs:
 *   { self, port, bind, peers: { name: url }, sshHosts: { name: ssh }, hostAddrs: { name: hostname }, editor, editorSsh, hosts: [names], fleetBin, tmux, claude,
 *     spawnDirs: [{ label, path }], uiDir, quickReplies, models: [{ id, label }], autoName: { enabled, intervalMinutes },
 *     grouping: { enabled, intervalMinutes, host }, uploads: { dir, maxMB, retentionDays },
 *     briefs: { enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour, minNewTurns,
 *               minNewChars, maxBriefChars, dir }, stacks: { sync, syncMinutes, generate, model },
 *     restore: { onBoot, markerFile }, configFile, configFound }
 */
export function normalizeConfig(
  raw = {},
  { env = process.env, home = os.homedir(), found = true, webRoot = null, fsImpl = fs } = {},
) {
  if (!isObject(raw)) throw new Error('config must be a JSON object');
  if (raw.version != null && raw.version !== SUPPORTED_VERSION) {
    throw new Error(`config.version ${raw.version} is not supported (expected ${SUPPORTED_VERSION})`);
  }

  const self = raw.self == null ? DEFAULT_SELF : raw.self;
  if (typeof self !== 'string' || !self.trim()) throw new Error('config.self must be a non-empty string');

  const hosts = raw.hosts == null ? {} : raw.hosts;
  if (!isObject(hosts)) throw new Error('config.hosts must be an object of name -> { ssh, web }');
  const peers = {};
  const sshHosts = {}; // name → ssh destination (the "Open in editor" Remote-SSH links)
  const hostAddrs = {}; // name → the hostname of its web url (which machine a browser is on)
  for (const [name, host] of Object.entries(hosts)) {
    // `editorSsh`: the destination the browser's machine uses, for a host this server reaches over
    // the web only (no `ssh`) or under another alias.
    const dest = isObject(host) ? [host.editorSsh, host.ssh].find((v) => typeof v === 'string' && v.trim()) : null;
    if (dest) sshHosts[name] = dest.trim();
    if (isObject(host) && typeof host.web === 'string') {
      try {
        hostAddrs[name] = new URL(host.web).hostname.replace(/^\[|\]$/g, '');
      } catch {
        // validated below for peers; a bad url on self just isn't matched
      }
    }
    if (name === self.trim()) continue; // never peer with yourself
    if (!isObject(host)) throw new Error(`config.hosts.${name} must be an object`);
    if (host.web == null) continue; // host without a web server: not a peer
    if (typeof host.web !== 'string' || !/^https?:\/\//.test(host.web)) {
      throw new Error(`config.hosts.${name}.web must be an http(s) url`);
    }
    peers[name] = host.web.replace(/\/+$/, '');
  }

  const web = raw.web == null ? {} : raw.web;
  if (!isObject(web)) throw new Error('config.web must be an object');

  let port = DEFAULT_PORT;
  if (web.port != null) port = parsePort(web.port, 'config.web.port');
  const envPort = env.FLEET_WEB_PORT || env.PORT;
  if (envPort) port = parsePort(envPort, 'FLEET_WEB_PORT/PORT');

  // No config → loopback only (there is no auth). With a config, default to all
  // interfaces so peers on the tailnet can reach this host.
  let bind = found ? '0.0.0.0' : '127.0.0.1';
  if (typeof web.bind === 'string' && web.bind) bind = web.bind;
  if (env.FLEET_WEB_BIND) bind = env.FLEET_WEB_BIND;

  // web.editor: the "Open in editor" link scheme (lib/editor.mjs); null hides it.
  let editor = DEFAULT_EDITOR;
  if (web.editor !== undefined) {
    if (web.editor !== null && !EDITORS.includes(web.editor)) throw new Error(`config.web.editor must be ${EDITORS.map((e) => `"${e}"`).join(', ')} or null: ${web.editor}`);
    editor = web.editor;
  }
  // web.editorSsh: the ssh destination other machines use for this one in editor links.
  const editorSsh = typeof web.editorSsh === 'string' && web.editorSsh.trim() ? web.editorSsh.trim() : null;

  const uiRaw = env.FLEET_WEB_UI || (typeof web.ui === 'string' && web.ui ? web.ui : null);
  const uiDir = resolveUiDir(uiRaw, { webRoot, home });

  const spawnDirs = spawnDirsFor(raw.spawnDirs, self.trim(), home);
  const hiddenSpawnDirs = hiddenSpawnDirsFor(raw.spawnDirs, self.trim(), home);

  let quickReplies = null;
  if (web.quickReplies != null) {
    if (!Array.isArray(web.quickReplies)) throw new Error('config.web.quickReplies must be an array');
    quickReplies = web.quickReplies
      // `{ label, kind: "key", value }` entries: the key chips (Esc, Enter, arrows) are
      // built into the UI, so they are accepted and skipped rather than rejected.
      .filter((q) => !(isObject(q) && q.kind === 'key'))
      .map((q) => {
        let r = typeof q === 'string' ? { label: q, text: q } : q;
        // `{ label, kind: "text", value }` is accepted as an alias of `{ label, text }`.
        if (isObject(r) && r.text == null && typeof r.value === 'string') r = { ...r, text: r.value };
        if (!isObject(r) || typeof r.text !== 'string' || !r.text) {
          throw new Error('config.web.quickReplies entries must be strings or { label, text }');
        }
        return { label: typeof r.label === 'string' && r.label ? r.label : r.text, text: r.text };
      });
  }

  // web.models: the New session model picker ({ id, label } or a bare id; id '' = no --model).
  let models = DEFAULT_MODELS;
  if (web.models != null) {
    if (!Array.isArray(web.models)) throw new Error('config.web.models must be an array of { id, label }');
    models = web.models.map((m) => {
      const r = typeof m === 'string' ? { id: m } : m;
      if (!isObject(r) || typeof r.id !== 'string' || (r.id !== '' && !MODEL_ID_RE.test(r.id))) {
        throw new Error('config.web.models entries must be { id, label } with an id of letters, digits and ._[]- (or "" for the default)');
      }
      const label = typeof r.label === 'string' && r.label.trim() ? r.label.trim() : r.id || 'Default';
      return { id: r.id, label };
    });
  }

  // web.autoName: the periodic `fleet name --all --apply` pass (lib/autoname.mjs).
  const an = web.autoName == null ? {} : web.autoName;
  if (!isObject(an)) throw new Error('config.web.autoName must be an object { enabled, intervalMinutes }');
  if (an.enabled != null && typeof an.enabled !== 'boolean') throw new Error('config.web.autoName.enabled must be true or false');
  let intervalMinutes = DEFAULT_AUTONAME_MINUTES;
  if (an.intervalMinutes != null) {
    const m = Number(an.intervalMinutes);
    if (!Number.isFinite(m) || m < 1) throw new Error(`config.web.autoName.intervalMinutes must be a number >= 1: ${an.intervalMinutes}`);
    intervalMinutes = m;
  }
  let autoNameEnabled = an.enabled === true; // opt-in: it types /rename into sessions and spends model calls
  if (env.FLEET_WEB_AUTONAME != null && env.FLEET_WEB_AUTONAME !== '') autoNameEnabled = !/^(0|false|off|no)$/i.test(env.FLEET_WEB_AUTONAME);
  const autoName = { enabled: autoNameEnabled, intervalMinutes };

  // web.grouping: the periodic `fleet group` pass (lib/grouping.mjs); grouping.host: which
  // host's server runs it for the whole fleet (peers proxy /api/groups there).
  const wg = web.grouping == null ? {} : web.grouping;
  if (!isObject(wg)) throw new Error('config.web.grouping must be an object { enabled, intervalMinutes }');
  if (wg.enabled != null && typeof wg.enabled !== 'boolean') throw new Error('config.web.grouping.enabled must be true or false');
  let groupingMinutes = DEFAULT_GROUPING_MINUTES;
  if (wg.intervalMinutes != null) {
    const m = Number(wg.intervalMinutes);
    if (!Number.isFinite(m) || m < 1) throw new Error(`config.web.grouping.intervalMinutes must be a number >= 1: ${wg.intervalMinutes}`);
    groupingMinutes = m;
  }
  const g = raw.grouping == null ? {} : raw.grouping;
  if (!isObject(g)) throw new Error('config.grouping must be an object { enabled, model, host }');
  if (g.host != null && (typeof g.host !== 'string' || !g.host.trim())) throw new Error('config.grouping.host must be a host name');
  let groupingEnabled = wg.enabled === true; // opt-in: it spends model calls
  if (env.FLEET_WEB_GROUPING != null && env.FLEET_WEB_GROUPING !== '') groupingEnabled = !/^(0|false|off|no)$/i.test(env.FLEET_WEB_GROUPING);
  const groupingHost = g.host == null ? null : g.host.trim();
  // With grouping.host set, only that host runs the pass — one source of truth.
  const grouping = {
    enabled: groupingEnabled && (groupingHost == null || groupingHost === self.trim()),
    intervalMinutes: groupingMinutes,
    host: groupingHost,
  };

  // inbox: the live to-do list (lib/inbox.mjs). One host runs it (`inbox.host`); the others
  // proxy /api/inbox there. FLEET_WEB_INBOX=0 turns it off on this server.
  const ib = raw.inbox == null ? {} : raw.inbox;
  if (!isObject(ib)) throw new Error('config.inbox must be an object { host, scanMinutes, workHours, workDays, … }');
  if (ib.host != null && (typeof ib.host !== 'string' || !ib.host.trim())) throw new Error('config.inbox.host must be a host name');
  const inbox = { ...DEFAULT_INBOX, host: ib.host == null ? null : ib.host.trim() };
  for (const key of ['scanMinutes', 'briefMinutes']) {
    if (ib[key] == null) continue;
    const n = Number(ib[key]);
    if (!Number.isFinite(n) || n < 1) throw new Error(`config.inbox.${key} must be a number >= 1: ${ib[key]}`);
    inbox[key] = n;
  }
  if (ib.workHours != null) {
    const h = ib.workHours;
    if (!Array.isArray(h) || h.length !== 2 || !h.every((x) => Number.isInteger(x) && x >= 0 && x <= 24) || h[0] >= h[1]) {
      throw new Error('config.inbox.workHours must be [from, to] hours, 0–24, from < to');
    }
    inbox.workHours = h;
  }
  if (ib.workDays != null) {
    if (!Array.isArray(ib.workDays) || !ib.workDays.every((d) => Number.isInteger(d) && d >= 0 && d <= 6)) {
      throw new Error('config.inbox.workDays must be an array of weekdays 0–6 (0 = Sunday)');
    }
    inbox.workDays = ib.workDays;
  }
  for (const key of ['model', 'briefModel']) {
    if (ib[key] == null) continue;
    if (typeof ib[key] !== 'string' || !MODEL_ID_RE.test(ib[key])) throw new Error(`config.inbox.${key} must be a model id (letters, digits and ._[]-)`);
    inbox[key] = ib[key];
  }
  if (ib.language != null) {
    if (typeof ib.language !== 'string' || !ib.language.trim()) throw new Error('config.inbox.language must be a non-empty string');
    inbox.language = ib.language.trim();
  }
  if (ib.comms != null && typeof ib.comms !== 'boolean') throw new Error('config.inbox.comms must be true or false');
  if (ib.comms != null) inbox.comms = ib.comms;
  inbox.enabled = inbox.host != null && inbox.host === self.trim();
  if (env.FLEET_WEB_INBOX != null && env.FLEET_WEB_INBOX !== '' && /^(0|false|off|no)$/i.test(env.FLEET_WEB_INBOX)) inbox.enabled = false;

  // web.uploads: where dropped/pasted files are stored (lib/uploads.mjs).
  const wu = web.uploads == null ? {} : web.uploads;
  if (!isObject(wu)) throw new Error('config.web.uploads must be an object { dir, maxMB, retentionDays }');
  let uploadsDir = DEFAULT_UPLOADS_DIR;
  if (wu.dir != null) {
    if (typeof wu.dir !== 'string' || !wu.dir.trim()) throw new Error('config.web.uploads.dir must be a non-empty string');
    uploadsDir = wu.dir.trim();
  }
  uploadsDir = expandHome(uploadsDir, home);
  if (!path.isAbsolute(uploadsDir)) throw new Error(`config.web.uploads.dir must be absolute or start with ~: ${wu.dir}`);
  let maxMB = DEFAULT_UPLOAD_MAX_MB;
  if (wu.maxMB != null) {
    maxMB = Number(wu.maxMB);
    if (!Number.isFinite(maxMB) || maxMB <= 0) throw new Error(`config.web.uploads.maxMB must be a number > 0: ${wu.maxMB}`);
  }
  let retentionDays = DEFAULT_UPLOAD_RETENTION_DAYS;
  if (wu.retentionDays != null) {
    retentionDays = Number(wu.retentionDays);
    if (!Number.isFinite(retentionDays) || retentionDays < 0) {
      throw new Error(`config.web.uploads.retentionDays must be a number >= 0 (0 = keep forever): ${wu.retentionDays}`);
    }
  }
  const uploads = { dir: path.normalize(uploadsDir), maxMB, retentionDays };

  // web.files.roots: extra dirs a relative path in chat may be under (lib/files.mjs fallback).
  const wf = web.files == null ? {} : web.files;
  if (!isObject(wf)) throw new Error('config.web.files must be an object { roots }');
  const fileRoots = [];
  if (wf.roots != null) {
    if (!Array.isArray(wf.roots)) throw new Error('config.web.files.roots must be an array of directories');
    for (const r of wf.roots) {
      if (typeof r !== 'string' || !r.trim()) throw new Error('config.web.files.roots entries must be non-empty strings');
      const d = expandHome(r.trim(), home);
      if (!path.isAbsolute(d)) throw new Error(`config.web.files.roots entries must be absolute or start with ~: ${r}`);
      fileRoots.push(path.normalize(d));
    }
  }
  const files = { roots: fileRoots };

  // web.notes: the notes explorer (lib/notes.mjs) — off unless `root` is set.
  const wn = web.notes == null ? {} : web.notes;
  if (!isObject(wn)) throw new Error('config.web.notes must be an object { root, name, searchCmd, exclude }');
  let notes = null;
  if (wn.root != null) {
    if (typeof wn.root !== 'string' || !wn.root.trim()) throw new Error('config.web.notes.root must be a non-empty string');
    const root = expandHome(wn.root.trim(), home);
    if (!path.isAbsolute(root)) throw new Error(`config.web.notes.root must be absolute or start with ~: ${wn.root}`);
    if (wn.name != null && (typeof wn.name !== 'string' || !wn.name.trim())) throw new Error('config.web.notes.name must be a non-empty string');
    let searchCmd = null;
    if (wn.searchCmd != null) {
      const argv = typeof wn.searchCmd === 'string' ? wn.searchCmd.trim().split(/\s+/) : wn.searchCmd;
      if (!Array.isArray(argv) || !argv.length || argv.some((a) => typeof a !== 'string' || !a)) {
        throw new Error('config.web.notes.searchCmd must be an argv array of strings, e.g. ["rg", "-n", "-i", "-F", "{query}"]');
      }
      searchCmd = [expandHome(argv[0], home), ...argv.slice(1)];
    }
    if (wn.exclude != null && (!Array.isArray(wn.exclude) || wn.exclude.some((e) => typeof e !== 'string' || !e.trim()))) {
      throw new Error('config.web.notes.exclude must be an array of names or root-relative paths');
    }
    notes = {
      root: path.normalize(root),
      name: wn.name?.trim() || path.basename(path.normalize(root)),
      searchCmd,
      exclude: (wn.exclude ?? []).map((e) => e.trim().replace(/^\/+|\/+$/g, '')),
    };
  }

  // web.memos: voice memos (lib/memos.mjs) — transcripts as markdown in `dir` (a notes folder,
  // say), segment audio in `audioDir`; `model` names their titles.
  const wm = web.memos == null ? {} : web.memos;
  if (!isObject(wm)) throw new Error('config.web.memos must be an object { dir, audioDir, model, vocabulary }');
  if (wm.vocabulary != null && (!Array.isArray(wm.vocabulary) || wm.vocabulary.some((v) => typeof v !== 'string'))) {
    throw new Error('config.web.memos.vocabulary must be an array of strings (names and terms the transcription should expect)');
  }
  const memoDir = (key, fallback) => {
    if (wm[key] != null && (typeof wm[key] !== 'string' || !wm[key].trim())) throw new Error(`config.web.memos.${key} must be a non-empty string`);
    const d = expandHome(wm[key]?.trim() ?? fallback, home);
    if (!path.isAbsolute(d)) throw new Error(`config.web.memos.${key} must be absolute or start with ~: ${wm[key]}`);
    return path.normalize(d);
  };
  if (wm.model != null && (typeof wm.model !== 'string' || !MODEL_ID_RE.test(wm.model))) throw new Error('config.web.memos.model must be a model id (letters, digits and ._[]-)');
  const memos = { dir: memoDir('dir', DEFAULT_MEMOS_DIR), audioDir: memoDir('audioDir', DEFAULT_MEMO_AUDIO_DIR), model: wm.model ?? 'haiku', vocabulary: (wm.vocabulary ?? []).map((v) => v.trim()).filter(Boolean) };

  // web.briefs: per-session briefs (lib/briefs.mjs) — the budget knobs for its model calls.
  const wb = web.briefs == null ? {} : web.briefs;
  if (!isObject(wb)) throw new Error('config.web.briefs must be an object { enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour }');
  if (wb.enabled != null && typeof wb.enabled !== 'boolean') throw new Error('config.web.briefs.enabled must be true or false');
  const briefs = { ...DEFAULT_BRIEFS, enabled: wb.enabled === true };
  if (wb.model != null) {
    if (typeof wb.model !== 'string' || !MODEL_ID_RE.test(wb.model)) throw new Error('config.web.briefs.model must be a model id (letters, digits and ._[]-)');
    briefs.model = wb.model;
  }
  for (const [key, min] of [['idleMs', 0], ['minIntervalMs', 0], ['maxDeltaChars', 500], ['maxCallsPerHour', 0], ['minNewTurns', 0], ['minNewChars', 0], ['maxBriefChars', 200]]) {
    if (wb[key] == null) continue;
    const n = Number(wb[key]);
    if (!Number.isFinite(n) || n < min) throw new Error(`config.web.briefs.${key} must be a number >= ${min}: ${wb[key]}`);
    briefs[key] = n;
  }
  if (env.FLEET_WEB_BRIEFS != null && env.FLEET_WEB_BRIEFS !== '') briefs.enabled = !/^(0|false|off|no)$/i.test(env.FLEET_WEB_BRIEFS);
  briefs.dir = briefsDir(env, home);

  // web.stacks: the background `fleet stack sync` (lib/stacks.mjs); top-level `stacks` is the
  // CLI's (`enabled` = may call the model when a stack is created, `model`), reported in /api/settings.
  const ws = web.stacks == null ? {} : web.stacks;
  if (!isObject(ws)) throw new Error('config.web.stacks must be an object { syncMinutes }');
  let syncMinutes = DEFAULT_STACKS_SYNC_MINUTES;
  if (ws.syncMinutes != null) {
    const m = Number(ws.syncMinutes);
    if (!Number.isFinite(m) || m <= 0) throw new Error(`config.web.stacks.syncMinutes must be a number > 0: ${ws.syncMinutes}`);
    syncMinutes = m;
  }
  let stacksSync = true;
  if (env.FLEET_WEB_STACKS != null && env.FLEET_WEB_STACKS !== '') stacksSync = !/^(0|false|off|no)$/i.test(env.FLEET_WEB_STACKS);
  // The CLI validates its own keys; here a bad value only falls back to the default.
  const st = isObject(raw.stacks) ? raw.stacks : {};
  const stacksModel = typeof st.model === 'string' && MODEL_ID_RE.test(st.model) ? st.model : DEFAULT_STACKS_MODEL;
  const stacks = { sync: stacksSync, syncMinutes, generate: st.enabled !== false, model: stacksModel };

  // web.operator: the voice operator — a Claude session that watches the fleet (lib/operator.mjs).
  const wo = web.operator == null ? {} : web.operator;
  if (!isObject(wo)) throw new Error('config.web.operator must be an object { hosts, model, voice, clearIdleMinutes }');
  if (wo.hosts != null && (!Array.isArray(wo.hosts) || !wo.hosts.length || wo.hosts.some((h) => typeof h !== 'string' || !h.trim()))) {
    throw new Error('config.web.operator.hosts must be a non-empty array of host names (first reachable runs the operator)');
  }
  for (const key of ['clearIdleMinutes']) {
    if (wo[key] != null && (!Number.isFinite(Number(wo[key])) || Number(wo[key]) < 0)) throw new Error(`config.web.operator.${key} must be a number >= 0: ${wo[key]}`);
  }
  if (wo.model != null && (typeof wo.model !== 'string' || !MODEL_ID_RE.test(wo.model))) throw new Error('config.web.operator.model must be a model id (letters, digits and ._[]-)');
  if (wo.voice != null && (typeof wo.voice !== 'string' || !/^[a-z0-9_-]{1,40}$/i.test(wo.voice))) throw new Error('config.web.operator.voice must be an OpenAI voice name (e.g. "alloy")');
  const operator = {
    // Where the operator runs: the first of these that answers (this machine when unset).
    hosts: (wo.hosts ?? [self.trim()]).map((h) => h.trim()),
    model: wo.model ?? '',
    voice: wo.voice ?? null,
    // Idle this long → /clear, a fresh conversation (0 = never).
    clearIdleMinutes: wo.clearIdleMinutes != null ? Number(wo.clearIdleMinutes) : 60,
    dir: path.join(fleetStateDir(env, home), 'operator'),
  };

  // web.focus.host: the machine whose `focus` CLI the Focus toggle drives — the one you sit at
  // (unset: no toggle). Every host's server proxies there.
  const wfo = web.focus == null ? {} : web.focus;
  if (!isObject(wfo)) throw new Error('config.web.focus must be an object { host }');
  if (wfo.host != null && (typeof wfo.host !== 'string' || !wfo.host.trim())) throw new Error('config.web.focus.host must be a host name');
  const focus = { host: wfo.host?.trim() || null };

  // restore.onBoot: at start, bring back what the last boot left dormant (lib/dormant.mjs).
  const rs = raw.restore == null ? {} : raw.restore;
  if (!isObject(rs)) throw new Error('config.restore must be an object { onBoot }');
  if (rs.onBoot != null && typeof rs.onBoot !== 'boolean') throw new Error('config.restore.onBoot must be true or false');
  const restore = { onBoot: rs.onBoot === true, markerFile: path.join(fleetStateDir(env, home), 'web-restored-boot') };

  return {
    self: self.trim(),
    port,
    bind,
    peers,
    sshHosts,
    hostAddrs,
    editor,
    editorSsh,
    hosts: [self.trim(), ...Object.keys(peers)],
    fleetBin: resolveFleetBin(raw, { env, home, fsImpl }),
    tmux: resolveBinary('tmux', { explicit: env.FLEET_TMUX || raw.tmux, env, home, fsImpl }),
    claude: typeof raw.claude === 'string' && raw.claude ? expandHome(raw.claude, home) : 'claude',
    // How each harness launches in a spawned session (`claude`, `harnesses.<id>.command`).
    launchers: {
      claude: typeof raw.claude === 'string' && raw.claude ? expandHome(raw.claude, home) : 'claude',
      codex: harnessCommand(raw, 'codex', home),
      pi: harnessCommand(raw, 'pi', home),
    },
    // GitHub identities a spawned session can act as (`ghIdentities`): names and logins only —
    // the launcher comes from `fleet gh-identity launcher`, which owns the shim.
    ghIdentities: ghIdentitiesOf(raw),
    spawnDirs,
    hiddenSpawnDirs,
    uiDir,
    quickReplies,
    models,
    autoName,
    grouping,
    inbox,
    uploads,
    files,
    notes,
    memos,
    briefs,
    stacks,
    restore,
    operator,
    focus,
  };
}

/** Read + normalize the shared config. Throws with a clear message on a broken file. */
export function loadConfig({ env = process.env, home = os.homedir(), webRoot = null, log = console, fsImpl = fs } = {}) {
  const file = configPath(env, home);
  let raw = {};
  let found = false;
  let text = null;
  try {
    text = fsImpl.readFileSync(file, 'utf8');
  } catch (err) {
    if (err?.code !== 'ENOENT') throw new Error(`cannot read fleet config ${file}: ${err.message}`);
  }
  if (text != null) {
    found = true;
    try {
      raw = JSON.parse(text);
    } catch (err) {
      throw new Error(`fleet config ${file} is not valid JSON: ${err.message}`);
    }
  } else {
    log.warn?.(`[config] no fleet config at ${file} — running as a single local host. Run \`fleet init\` to set up hosts.`);
  }
  let cfg;
  try {
    cfg = normalizeConfig(raw, { env, home, found, webRoot, fsImpl });
  } catch (err) {
    throw new Error(`invalid fleet config ${file}: ${err.message}`);
  }
  return { ...cfg, configFile: found ? file : null };
}
