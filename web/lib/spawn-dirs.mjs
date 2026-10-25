// The `spawnDirs` editor behind GET/PUT /api/hosts/:host/spawn-dirs (Settings → Start
// directories). Each host validates and stores its own config; the UI writes the same
// list to every host (the shared-list model: one entry per label, a path per host).
//
// Writes go through the CLI (`fleet --local config set spawnDirs <json>`, lib/fleet-cli.mjs):
// the CLI owns the config file (atomic tmp + rename, unknown keys kept). After a write the
// running server's `config.spawnDirs` is replaced in place, so /api/fleet and the spawn
// allow-list use the new list at once — no restart.
//
// An entry with `hidden: true` stays on the spawn allow-list but is not offered in the New
// session menu (e.g. a dir only an app's embedded panel spawns in). Keys this editor does not
// know are carried through a GET → PUT round trip unchanged.
//
// Editing this list is as powerful as spawning (it widens what spawn accepts); like
// spawn/send it relies on the network being private (no auth).
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { expandHome, hiddenSpawnDirsFor, spawnDirsFor } from './config.mjs';

export const SPAWN_DIRS_LIMITS = Object.freeze({ maxEntries: 30, maxLabel: 40, maxPath: 1024, maxHost: 64 });

const CONTROL_RE = /[\u0000-\u001f\u007f]/;

function isObject(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

// The keys the editor owns; any other key on an entry is kept as is.
const OWN_KEYS = new Set(['label', 'paths', 'path', 'hidden']);

/** An entry's other keys (unknown to the editor), to carry through a rewrite. */
function extraKeys(e) {
  return Object.fromEntries(Object.entries(e).filter(([k]) => !OWN_KEYS.has(k)));
}

/**
 * The raw config list in the canonical shape `[{ label, paths: { host: dir }, hidden? }]`
 * (`hidden: true` only when set; unknown keys kept). A
 * `{ label, path }` entry (or a bare string) means "the same dir on every host": it is
 * spelled out for each of `hostNames`. A missing label becomes the dir's basename.
 */
export function canonicalSpawnDirs(list, hostNames = []) {
  if (!Array.isArray(list)) return [];
  const out = [];
  for (let e of list) {
    if (typeof e === 'string') e = { path: e };
    if (!isObject(e)) continue;
    const paths = {};
    if (isObject(e.paths)) {
      for (const [h, d] of Object.entries(e.paths)) if (typeof d === 'string' && d.trim()) paths[h] = d.trim();
    } else if (typeof e.path === 'string' && e.path.trim()) {
      for (const h of hostNames) paths[h] = e.path.trim();
    }
    const first = Object.values(paths)[0] ?? '';
    const label = typeof e.label === 'string' && e.label.trim() ? e.label.trim() : path.basename(first) || first;
    out.push({ ...extraKeys(e), label, paths, ...(e.hidden === true ? { hidden: true } : {}) });
  }
  return out;
}

function checkPathSyntax(p) {
  if (p.length > SPAWN_DIRS_LIMITS.maxPath) return `longer than ${SPAWN_DIRS_LIMITS.maxPath} characters`;
  if (CONTROL_RE.test(p)) return 'contains a control character';
  if (!(p === '~' || p.startsWith('~/') || p.startsWith('/'))) return 'must be absolute or start with ~/';
  return null;
}

/** Does `dir` (already `~`-expanded) exist, and is it a directory? */
export async function statDir(dir, fsp = fs.promises) {
  try {
    const st = await fsp.stat(dir);
    return { exists: true, isDir: st.isDirectory() };
  } catch {
    return { exists: false, isDir: false };
  }
}

/**
 * Validate a proposed list for THIS host (`self`). Labels: 1–40 chars, one line, unique
 * (case-insensitive). Paths: absolute or `~/…`, ≤ 1024 chars, no control characters; an
 * empty path = not offered on that host; every entry needs at least one. This host's path
 * must be an existing directory; other hosts' paths are stored as given (they check on
 * their own host). `hidden`: optional boolean (stored only when true). Other keys are kept.
 * → { entries: [{ label, paths, hidden? }], errors: [{ index, field: 'label'|'paths'|'hidden', host?, error }],
 *     checks: [{ path, resolved, exists, isDir } | null] }  (checks: this host, per entry)
 */
export async function validateSpawnDirs(list, { self, home = os.homedir(), stat = statDir } = {}) {
  const L = SPAWN_DIRS_LIMITS;
  const errors = [];
  const entries = [];
  const checks = [];
  if (!Array.isArray(list)) return { entries, checks, errors: [{ index: -1, field: 'list', error: 'spawnDirs must be an array' }] };
  if (list.length > L.maxEntries) return { entries, checks, errors: [{ index: -1, field: 'list', error: `at most ${L.maxEntries} directories` }] };
  const seen = new Map();
  for (let i = 0; i < list.length; i++) {
    const e = list[i];
    if (!isObject(e)) {
      errors.push({ index: i, field: 'label', error: 'must be an object { label, paths }' });
      entries.push(null);
      checks.push(null);
      continue;
    }
    const label = typeof e.label === 'string' ? e.label.trim() : '';
    if (!label) errors.push({ index: i, field: 'label', error: 'label is required' });
    else if (label.length > L.maxLabel) errors.push({ index: i, field: 'label', error: `label is longer than ${L.maxLabel} characters` });
    else if (CONTROL_RE.test(label)) errors.push({ index: i, field: 'label', error: 'label must be one line of text' });
    else {
      const key = label.toLowerCase();
      if (seen.has(key)) errors.push({ index: i, field: 'label', error: `duplicate label "${label}"` });
      else seen.set(key, i);
    }
    if (e.hidden != null && typeof e.hidden !== 'boolean') errors.push({ index: i, field: 'hidden', error: 'hidden must be true or false' });
    const paths = {};
    if (e.paths != null && !isObject(e.paths)) errors.push({ index: i, field: 'paths', error: 'paths must be an object { host: dir }' });
    for (const [host, raw] of Object.entries(isObject(e.paths) ? e.paths : {})) {
      if (!host || host.length > L.maxHost || CONTROL_RE.test(host)) {
        errors.push({ index: i, field: 'paths', error: 'bad host name' });
        continue;
      }
      if (raw == null) continue;
      if (typeof raw !== 'string') {
        errors.push({ index: i, field: 'paths', host, error: 'path must be a string' });
        continue;
      }
      const p = raw.trim();
      if (!p) continue; // not offered on that host
      const bad = checkPathSyntax(p);
      if (bad) errors.push({ index: i, field: 'paths', host, error: `path ${bad}` });
      paths[host] = p;
    }
    if (!Object.keys(paths).length && !errors.some((x) => x.index === i && x.field === 'paths')) {
      errors.push({ index: i, field: 'paths', error: 'set a directory for at least one host' });
    }
    let check = null;
    const mine = paths[self];
    if (mine && !checkPathSyntax(mine)) {
      const resolved = path.normalize(expandHome(mine, home));
      check = { path: mine, resolved, ...(await stat(resolved)) };
      if (!check.isDir) {
        errors.push({ index: i, field: 'paths', host: self, error: check.exists ? `not a directory on ${self}` : `no such directory on ${self}` });
      }
    }
    checks.push(check);
    entries.push({ ...extraKeys(e), label, paths, ...(e.hidden === true ? { hidden: true } : {}) });
  }
  return { entries, errors, checks };
}

/**
 * @param config      the server's normalized config — `config.spawnDirs` is replaced in place after a save
 * @param configFile  the config path the CLI writes (FLEET_CONFIG)
 * @param cli         lib/fleet-cli.mjs instance (`configSet`)
 */
export function createSpawnDirsEditor({ config, configFile, cli, home = os.homedir(), fsImpl = fs, stat = statDir }) {
  let queue = Promise.resolve(); // one write at a time (the CLI's tmp file is fixed)

  function readRaw() {
    let text;
    try {
      text = fsImpl.readFileSync(configFile, 'utf8');
    } catch (err) {
      if (err?.code === 'ENOENT') return {};
      throw Object.assign(new Error(`cannot read fleet config: ${err.message}`), { status: 500 });
    }
    try {
      const raw = JSON.parse(text);
      return isObject(raw) ? raw : {};
    } catch (err) {
      throw Object.assign(new Error(`fleet config is not valid JSON: ${err.message}`), { status: 500 });
    }
  }

  function hostNames(raw) {
    return [...new Set([config.self, ...Object.keys(isObject(raw.hosts) ? raw.hosts : {})])];
  }

  function describe(raw, entries, checks) {
    return {
      host: config.self,
      hosts: hostNames(raw),
      spawnDirs: entries,
      checks,
      offered: config.spawnDirs,
      hiddenSpawnDirs: config.hiddenSpawnDirs ?? [],
      limits: SPAWN_DIRS_LIMITS,
    };
  }

  /** GET: the stored list (canonical shape) + whether each of this host's paths is a directory. */
  async function get() {
    const raw = readRaw();
    const entries = canonicalSpawnDirs(raw.spawnDirs, hostNames(raw));
    const checks = await Promise.all(
      entries.map(async (e) => {
        const p = e.paths[config.self];
        if (!p) return null;
        const resolved = path.normalize(expandHome(p, home));
        return { path: p, resolved, ...(await stat(resolved)) };
      }),
    );
    return describe(raw, entries, checks);
  }

  /** PUT `{ spawnDirs, dryRun? }` → { status, body }. 400 `{ error, errors, checks }` when invalid. */
  async function put(body) {
    const { entries, errors, checks } = await validateSpawnDirs(body?.spawnDirs, { self: config.self, home, stat });
    if (errors.length) return { status: 400, body: { host: config.self, error: describeError(errors[0], body?.spawnDirs), errors, checks } };
    if (body?.dryRun === true) return { status: 200, body: { ...describe(readRaw(), entries, checks), saved: false } };
    const run = queue.then(async () => {
      await cli.configSet({ key: 'spawnDirs', value: entries, configFile });
      const raw = readRaw();
      config.spawnDirs = spawnDirsFor(raw.spawnDirs, config.self, home); // hot reload
      config.hiddenSpawnDirs = hiddenSpawnDirsFor(raw.spawnDirs, config.self, home);
      return { status: 200, body: { ...describe(raw, canonicalSpawnDirs(raw.spawnDirs, hostNames(raw)), checks), saved: true } };
    });
    queue = run.catch(() => {});
    return run;
  }

  return { get, put };
}

function describeError(e, list) {
  const label = Array.isArray(list) && isObject(list[e.index]) && typeof list[e.index].label === 'string' ? list[e.index].label.trim() : '';
  const where = e.index < 0 ? '' : label ? `${label}: ` : `entry ${e.index + 1}: `;
  return `${where}${e.host && !e.error.includes(e.host) ? `${e.host}: ` : ''}${e.error}`;
}
