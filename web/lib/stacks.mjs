// Session stacks, the web side (docs/architecture.md → Session stacks). A stack is N Claude
// sessions sharing one markdown file, the StackBrief; the `fleet` CLI owns it entirely
// (`fleet --local stack … --json`, lib/fleet-cli.mjs). The server never parses the markdown: it
// passes the CLI's JSON (StackView) through, adds `editorUrl` on the serving side, spawns
// siblings with its own spawner (or a peer's, for a sibling on another host) and, in the
// background, adds the new session to the stack once it has registered and keeps membership in
// sync (`stack sync --input -` with the merged fleet, so members on other hosts are judged too).
// This host's stacks go out in its `/api/fleet?local=1` entry (`stacks`, compact) and every
// server stamps `stack` on the rows of members whose stack is homed elsewhere (`stampForeign`).
import { promises as fs } from 'node:fs';
import os from 'node:os';

import { HttpError } from './http.mjs';
import { editorUrl } from './editor.mjs';
import { expandHome } from './config.mjs';
import { isWithin, resolveAllowedDir } from './spawn.mjs';
import { findSpawned } from './autoname.mjs';

/** A stack id: `st-` + 8 lowercase hex chars (never changes). */
export const STACK_ID_RE = /^st-[0-9a-f]{8}$/;
/** PUT …/stacks/:id: the markdown cap (bytes, UTF-8). */
export const MAX_STACK_MARKDOWN = 64 * 1024;
/** The request body cap for that PUT: the markdown plus its JSON envelope and escaping. */
export const STACK_BODY_LIMIT = 256 * 1024;
export const MAX_STACK_LABEL = 80;
const ISO_RE = /^[0-9][0-9T:.+\-Z]{9,39}$/;

/** Waits between tries of the post-spawn `stack add`: 75 s in all, tighter at first. */
export const STACK_JOIN_DELAYS_MS = [3000, 4000, 5000, 6000, 7000, 10000, 10000, 15000, 15000];
/** The first background `stack sync` runs this long after start. */
export const STACK_SYNC_INITIAL_DELAY_MS = 30 * 1000;

/**
 * The CLI's stderr for a stack that is not there: `no stack matches "<q>"` (exit 3) or
 * `stack st-… is gone` (exit 3, deleted between resolve and write). → 404, not 409.
 */
const STACK_MISSING_RE = /\b(no stack|unknown stack|no such stack|stack not found|not found)\b|\bstack st-[0-9a-f]{8} is gone\b/i;

/** A FleetCliError from a `fleet stack` call → the HttpError the API answers with. */
export function stackHttpError(err, what = 'fleet stack') {
  if (err instanceof HttpError) return err;
  const msg = String(err?.message ?? err);
  if (err?.timedOut) return new HttpError(`${what} timed out: ${msg}`, 504);
  if (err?.missing) return new HttpError('this host\'s fleet CLI has no `stack` command — update fleet there', 501);
  if (STACK_MISSING_RE.test(msg)) return new HttpError(msg, 404);
  if (err?.exitCode === 2 || err?.exitCode === 3) return new HttpError(msg, 409); // ambiguous / nothing to act on
  if (err?.exitCode === 1) return new HttpError(msg, 400); // usage / refusal
  return new HttpError(`${what} failed: ${msg}`, 502);
}

/** The body of PUT …/stacks/:id → { markdown, expectUpdated }; throws HttpError(400). */
export function validateStackEdit(body) {
  const markdown = body?.markdown;
  if (typeof markdown !== 'string') throw new HttpError('markdown must be a string', 400);
  if (Buffer.byteLength(markdown) > MAX_STACK_MARKDOWN) throw new HttpError(`markdown too long (max ${MAX_STACK_MARKDOWN} bytes)`, 400);
  let expectUpdated = null;
  if (body.expectUpdated != null && body.expectUpdated !== '') {
    if (typeof body.expectUpdated !== 'string' || !ISO_RE.test(body.expectUpdated)) throw new HttpError('expectUpdated must be an ISO timestamp', 400);
    expectUpdated = body.expectUpdated;
  }
  return { markdown, expectUpdated };
}

/** Optional `label` of a sibling spawn (used only when a new stack is created). */
export function validateStackLabel(label) {
  if (label == null || label === '') return null;
  if (typeof label !== 'string') throw new HttpError('label must be a string', 400);
  const l = label.trim();
  if (!l) return null;
  if (/[\r\n]/.test(l) || [...l].length > MAX_STACK_LABEL) throw new HttpError(`label must be one line of at most ${MAX_STACK_LABEL} characters`, 400);
  return l;
}

/** A StackView + `editor` / `editorUrl` for its `absCwd` (built by the server the browser asked). */
export function withStackEditor(view, host, config) {
  if (!view || typeof view !== 'object' || Array.isArray(view) || view.error) return view;
  return { ...view, editor: config.editor ?? null, editorUrl: editorUrl(config, host, typeof view.absCwd === 'string' ? view.absCwd : null) };
}

/** `{ host, stacks: [StackView] }` (list / sync bodies) with editor links on every stack. */
export function withStacksEditor(body, host, config) {
  if (!body || typeof body !== 'object' || !Array.isArray(body.stacks)) return body;
  return { ...body, stacks: body.stacks.map((s) => withStackEditor(s, host, config)) };
}

/**
 * Where a sibling starts: `base` (the source session's cwd or the stack's absCwd, `~`
 * expanded) unless `requested` is given, which must lie inside `base` or pass
 * resolveAllowedDir against this host's spawn dirs (`roots`). Returns a realpath; throws
 * HttpError(400) when a directory does not exist or is not allowed.
 */
export async function resolveStackSpawnDir({ base, requested = null, roots = [], home = os.homedir() }) {
  const abs = expandHome(typeof base === 'string' ? base : '', home);
  let realBase = null;
  try {
    realBase = await fs.realpath(abs);
    if (!(await fs.stat(realBase)).isDirectory()) realBase = null;
  } catch {
    realBase = null;
  }
  if (!requested) {
    if (!realBase) throw new HttpError(`not a directory on this host: ${base || '(none)'}`, 400);
    return realBase;
  }
  let real;
  try {
    real = await fs.realpath(requested);
  } catch {
    throw new HttpError(`dir does not exist: ${requested}`, 400);
  }
  if (realBase && isWithin(real, realBase)) return real;
  try {
    return await resolveAllowedDir(requested, roots, { home });
  } catch {
    throw new HttpError(`dir must be inside ${base} or one of this host's spawn dirs: ${requested}`, 400);
  }
}

/** A StackView → the compact entry this host advertises in `/api/fleet` (`stacks[]`). */
export function compactStack(view) {
  if (!view || typeof view !== 'object' || !STACK_ID_RE.test(String(view.id))) return null;
  return {
    id: view.id,
    label: typeof view.label === 'string' ? view.label : view.id,
    host: typeof view.host === 'string' ? view.host : null,
    lead: typeof view.lead === 'string' ? view.lead : null,
    members: (Array.isArray(view.members) ? view.members : [])
      .filter((m) => m && typeof m.session === 'string')
      .map((m) => ({ session: m.session, host: typeof m.host === 'string' ? m.host : null, live: typeof m.live === 'boolean' ? m.live : m.closed == null })),
  };
}

/**
 * Stamp `stack: { id, label, host, lead }` on the rows of sessions that are live members of a stack
 * homed on another host (each host's `stacks[]` names its own stacks and their members' hosts).
 * A row's own `stack` (its host's file) wins. Pure: a new `hosts` array, rows copied as needed.
 */
export function stampForeign(hosts) {
  if (!Array.isArray(hosts)) return hosts;
  const out = hosts.map((h) => (h && typeof h === 'object' ? { ...h, sessions: Array.isArray(h.sessions) ? [...h.sessions] : h.sessions } : h));
  const byName = new Map(out.filter((h) => h && typeof h.name === 'string').map((h) => [h.name, h]));
  for (const home of out) {
    if (!home || !Array.isArray(home.stacks)) continue;
    for (const st of home.stacks) {
      if (!st || typeof st.id !== 'string' || !Array.isArray(st.members)) continue;
      const ref = { id: st.id, label: typeof st.label === 'string' ? st.label : st.id, host: home.name, lead: typeof st.lead === 'string' ? st.lead : null };
      for (const m of st.members) {
        if (!m || typeof m.session !== 'string' || !m.host || m.host === home.name || m.live === false) continue;
        const entry = byName.get(m.host);
        if (!entry || !Array.isArray(entry.sessions)) continue;
        const i = entry.sessions.findIndex((s) => s?.session_id === m.session);
        if (i < 0 || entry.sessions[i].stack) continue;
        entry.sessions[i] = { ...entry.sessions[i], stack: ref };
      }
    }
  }
  return out;
}

/**
 * @param deps
 *   cli           lib/fleet-cli.mjs instance (stackList/Show/Set/Remove/Ensure/Add/Sync)
 *   listSessions  () => Promise<rows> — this host's fresh `fleet list --json` rows
 *   gateSessions  () => Promise<rows | null> — the current list for the sync gate (a cached one
 *                 is fine: server.mjs passes fleet.localHost(), 2 s TTL); default listSessions
 *   self          this host's name (members without a host, and the join's default)
 *   syncEnabled   run the background sync (web.stacks; FLEET_WEB_STACKS=0 turns it off)
 *   syncIntervalMs, initialDelayMs, joinDelaysMs, sleep, log, now
 * `attach({ fleetBody, peerSessions })` (server.mjs, once the API exists): the merged
 * `/api/fleet` body for the sync, and a peer's fresh rows for a join on another host.
 */
export function createStacks({
  cli,
  listSessions,
  gateSessions = null,
  self = 'local',
  syncEnabled = true,
  syncIntervalMs = 2 * 60 * 1000,
  initialDelayMs = STACK_SYNC_INITIAL_DELAY_MS,
  joinDelaysMs = STACK_JOIN_DELAYS_MS,
  sleep,
  log = () => {},
  now = Date.now,
}) {
  if (!cli || typeof cli.stackSync !== 'function') throw new TypeError('cli.stackSync must be a function');
  if (typeof listSessions !== 'function') throw new TypeError('listSessions must be a function');
  const wait = sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms).unref?.()));
  let fleetBody = null; // () => Promise<{ hosts }> — the merged fleet (attach)
  let peerSessions = null; // (host) => Promise<rows | null> (attach)

  const call = async (what, fn) => {
    try {
      return await fn();
    } catch (err) {
      throw stackHttpError(err, what);
    }
  };

  // --- what this host's stacks look like now: every StackView seen, by id ------------------
  const known = new Map(); // id → compact
  const remember = (view) => {
    const c = compactStack(view);
    if (c) known.set(c.id, c);
    return view;
  };
  const rememberAll = (body) => {
    if (Array.isArray(body?.stacks)) {
      known.clear();
      for (const v of body.stacks) remember(v);
    }
    return body;
  };
  /** The nested StackView of an `ensure` answer (its own `created` is the timestamp). */
  const rememberEnsured = (out) => {
    remember(out?.stack && typeof out.stack === 'object' ? out.stack : out);
    return out;
  };

  // --- membership sync: de-duplicated, with one trailing re-run for a call made mid-run ---
  let lastSync = null;
  let inFlight = null;
  let queued = null;

  async function doSync(reason) {
    const at = now();
    try {
      const input = fleetBody ? await fleetBody().catch(() => null) : null;
      const report = rememberAll(await cli.stackSync(input && Array.isArray(input.hosts) ? { input } : {}));
      const changed = Array.isArray(report?.changed) ? report.changed : [];
      lastSync = { at, ms: now() - at, reason, ok: true, changed, merged: Boolean(input) };
      if (changed.length) log(`[stacks] ${reason} sync: ${changed.join(', ')} updated`);
      return report;
    } catch (err) {
      lastSync = { at, ms: now() - at, reason, ok: false, changed: [], error: String(err?.message ?? err) };
      log(`[stacks] ${reason} sync failed: ${lastSync.error}`);
      throw err;
    }
  }

  /** `fleet --local stack sync --json` → `{ host, changed, stacks }`; throws FleetCliError. */
  function sync(reason = 'manual') {
    if (!inFlight) {
      inFlight = doSync(reason).finally(() => {
        inFlight = null;
      });
      return inFlight;
    }
    if (!queued) {
      queued = inFlight
        .catch(() => {})
        .then(() => {
          queued = null;
          return sync(reason);
        });
    }
    return queued;
  }

  /**
   * One background tick: sync when the last list showed a session in a stack, or a known
   * stack may have members elsewhere (`known`, seeded by the first tick, which always runs).
   */
  let ticked = false;
  async function tick() {
    const first = !ticked;
    ticked = true;
    const rows = await (gateSessions ?? listSessions)().catch(() => null);
    const inStack = Array.isArray(rows) && rows.some((r) => r?.stack != null);
    if (!first && !inStack && known.size === 0) return { ran: false };
    await sync(first ? 'initial' : 'scheduled').catch(() => {}); // logged in doSync
    return { ran: true };
  }

  let timer = null;
  let stopped = false;
  function schedule(delay) {
    if (stopped) return;
    clearTimeout(timer);
    timer = setTimeout(async () => {
      await tick();
      schedule(syncIntervalMs);
    }, delay);
    timer.unref?.();
  }

  // --- after a sibling spawn: find the new session by its tmux name, then `stack add` ----
  const pending = new Map(); // `${stackId} ${host} ${tmuxSession}` -> promise

  /**
   * The new session's row: this host's fresh list, or — a sibling started on `host` — that
   * peer's (`peerSessions`, its `/api/fleet?local=1`). `null` = could not list this try.
   */
  async function rowsOn(host) {
    if (host === self) return listSessions().catch(() => null);
    if (!peerSessions) return null;
    return peerSessions(host).catch(() => null);
  }

  async function runJoin(tmuxSession, stackId, host) {
    const where = host === self ? '' : ` on ${host}`;
    const hint = host === self ? `fleet stack add ${stackId} <session>` : `fleet stack add ${stackId} <session> --on ${host}`;
    let tries = 0;
    for (const delay of joinDelaysMs) {
      await wait(delay);
      if (stopped) return { ok: false, reason: 'stopped', tries };
      tries += 1;
      const rows = await rowsOn(host);
      if (!Array.isArray(rows)) continue;
      const s = findSpawned(rows, tmuxSession);
      if (!s) continue; // not registered yet
      if (s.stack?.id === stackId) return { ok: true, reason: 'already a member', session: s.session_id, tries };
      try {
        const view =
          host === self
            ? await cli.stackAdd(stackId, s.session_id)
            : await cli.stackAdd(stackId, s.session_id, { host, input: { hosts: [{ name: host, ok: true, sessions: [s] }] } });
        remember(view);
        log(`[stacks] ${tmuxSession}${where} (${String(s.session_id).slice(0, 8)}) joined ${stackId}`);
        return { ok: true, reason: 'added', session: s.session_id, tries };
      } catch (err) {
        const msg = String(err?.message ?? err);
        if (!err?.timedOut && [1, 2, 3].includes(err?.exitCode)) {
          log(`[stacks] ${tmuxSession}${where}: not added to ${stackId}: ${msg}`);
          return { ok: false, reason: 'refused', session: s.session_id, tries, error: msg };
        }
        log(`[stacks] ${tmuxSession}${where}: add to ${stackId} failed (try ${tries}): ${msg}`);
      }
    }
    log(`[stacks] ${tmuxSession}${where}: not found after ${tries} tries — run \`${hint}\``);
    return { ok: false, reason: 'not found', tries };
  }

  /**
   * Start the background add for a just-spawned tmux session (once per stack / host / name);
   * `host` is where it was started (default: this host). Its promise never rejects.
   */
  function join(tmuxSession, stackId, host = self) {
    const key = `${stackId} ${host} ${tmuxSession}`;
    if (pending.has(key)) return pending.get(key);
    const p = runJoin(tmuxSession, stackId, host).finally(() => pending.delete(key));
    pending.set(key, p);
    return p;
  }

  return {
    list: () => call('fleet stack list', async () => rememberAll(await cli.stackList())),
    show: (id) => call('fleet stack show', async () => remember(await cli.stackShow(id))),
    /** A human edit. Conflict (CLI exit 3) → HttpError 409 carrying `body: { error, updated }`. */
    async set(id, markdown, expectUpdated = null) {
      try {
        return remember(await cli.stackSet(id, markdown, expectUpdated));
      } catch (err) {
        if (err?.exitCode === 3 && !err?.missing && !STACK_MISSING_RE.test(String(err?.message))) {
          const report = err.report ?? {};
          const e = new HttpError(report.error ?? report.message ?? err.message ?? 'the stack changed since it was loaded', 409);
          e.body = { ...report, error: e.message, updated: report.updated ?? null };
          throw e;
        }
        throw stackHttpError(err, 'fleet stack set');
      }
    },
    remove: (id) =>
      call('fleet stack rm', async () => {
        const r = await cli.stackRemove(id);
        known.delete(id);
        return r;
      }),
    rename: (id, label) => call('fleet stack rename', async () => remember(await cli.stackRename(id, label))),
    lead: (id, session, opts) => call('fleet stack lead', async () => remember(await cli.stackLead(id, session, opts))),
    /** Settings → Session stacks: the lead / report prompts (`fleet stack list --json` → `settings`). */
    settings: () => call('fleet stack list', async () => {
      const out = await cli.stackList();
      rememberAll(out);
      return out?.settings ?? null;
    }),
    ensure: (sessionId, opts) => call('fleet stack ensure', async () => rememberEnsured(await cli.stackEnsure(sessionId, opts))),
    /**
     * Add an existing live session to a stack: this host's (`host` absent / self) or a peer's,
     * its row looked up in that host's list (carried to the CLI like a join). 404 when it is not
     * live there; 409 when it is already in another stack; already a member → the stack as is.
     */
    async add(stackId, sessionId, host = self) {
      const rows = await rowsOn(host);
      if (!Array.isArray(rows)) throw new HttpError(`could not list the sessions on ${host}`, 502);
      const s = rows.find((r) => r?.session_id === sessionId);
      if (!s) throw new HttpError(`no live session ${sessionId}${host === self ? '' : ` on ${host}`}`, 404);
      if (s.stack?.id === stackId) return call('fleet stack show', async () => remember(await cli.stackShow(stackId)));
      if (s.stack?.id) throw new HttpError(`that session is already in the stack "${s.stack.label || s.stack.id}" — remove it there first`, 409);
      const opts = host === self ? undefined : { host, input: { hosts: [{ name: host, ok: true, sessions: [s] }] } };
      return call('fleet stack add', async () => remember(await cli.stackAdd(stackId, sessionId, opts)));
    },
    /** POST …/stacks/sync: like `sync`, errors mapped to HttpError. */
    syncNow: (reason = 'manual') => call('fleet stack sync', () => sync(reason)),
    sync,
    tick,
    join,
    /** This host's stacks, compact (`/api/fleet` → `hosts[].stacks`): what the last calls showed. */
    compact: () => [...known.values()],
    /** Wire the merged fleet (sync) and a peer's rows (join on another host); either may be absent. */
    attach({ fleetBody: fb = null, peerSessions: ps = null } = {}) {
      fleetBody = typeof fb === 'function' ? fb : null;
      peerSessions = typeof ps === 'function' ? ps : null;
    },
    start() {
      stopped = false;
      if (syncEnabled) schedule(initialDelayMs);
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
    },
    status: () => ({ sync: syncEnabled, syncMinutes: syncIntervalMs / 60000, lastSync, known: known.size }),
    get lastSync() {
      return lastSync;
    },
    get pending() {
      return [...pending.keys()];
    },
  };
}
