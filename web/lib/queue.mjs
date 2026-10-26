// Prompt queue: messages typed while a session works wait here, on the session's host, and go in
// one per run, once the session is idle again — never while it waits on you (a question, a
// permission prompt), never mid-run (Claude Code's own queue would hand them over between steps).
//
// A run has ended when the session is idle and its `updated_at` (it moves on every status change:
// each turn start and end) is past `after`: the last delivery, or the moment a pause turned into a
// hold. So one queued prompt goes in per run, and nothing goes in twice while the status lags.
//
// Pause holds the queue for the run under way: once that run ends the pause clears itself, and the
// queue holds through that idle (you read the answer, follow up by hand); it moves again when the
// next run ends — or at once with "send now".
//
// State (`<fleet state>/prompt-queue.json`, 0600): { [session id]: { items, paused, after } }.
import { randomBytes } from 'node:crypto';
import { promises as fsp } from 'node:fs';
import path from 'node:path';

import { findSession, validateSendText } from './util.mjs';

export const MAX_ITEMS = 20;
const TICK_MS = 2500;
/** A delivery that never shows as a run (lost keystrokes, a status that never moved): move on. */
const STUCK_MS = 2 * 60 * 1000;
const ITEM_ID_RE = /^q-[0-9a-f]{6}$/;
const SESSION_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;

export class QueueError extends Error {
  constructor(message, status = 400) {
    super(message);
    this.status = status;
  }
}

export const validItemId = (id) => typeof id === 'string' && ITEM_ID_RE.test(id);

function validText(text) {
  const check = validateSendText(text);
  if (!check.ok) throw new QueueError(check.error, 400);
  return check.text;
}

/**
 * @param {object} deps
 *   file          the state file
 *   listSessions  () → the local sessions (fleet list rows) or null when discovery failed
 *   send          (session, text) → typed into the session (lib/backends.mjs#send)
 *   log, now, randomId, tickMs   (tests)
 */
export function createPromptQueue({
  file,
  listSessions,
  send,
  log = () => {},
  now = () => Date.now(),
  randomId = () => randomBytes(3).toString('hex'),
  tickMs = TICK_MS,
}) {
  let state = null; // { [sid]: { items: [{ id, text, createdAt }], paused, after, sentAt } }
  let chain = Promise.resolve(); // one change at a time
  let timer = null;
  let stopped = true;
  let writeSeq = 0;

  async function load() {
    if (state) return state;
    try {
      const data = JSON.parse(await fsp.readFile(file, 'utf8'));
      state = data && typeof data === 'object' && !Array.isArray(data) ? data : {};
    } catch (err) {
      if (err?.code !== 'ENOENT') log(`[queue] unreadable ${file}: ${err?.message ?? err}`);
      state = {};
    }
    for (const [sid, q] of Object.entries(state)) {
      if (!SESSION_ID_RE.test(sid) || !q || !Array.isArray(q.items)) delete state[sid];
    }
    return state;
  }

  async function save() {
    await fsp.mkdir(path.dirname(file), { recursive: true, mode: 0o700 });
    const tmp = `${file}.${process.pid}.${++writeSeq}.tmp`;
    await fsp.writeFile(tmp, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
    await fsp.rename(tmp, file);
  }

  /** Run `fn(state)` after every earlier change; it returns true when the state changed. */
  function locked(fn) {
    const run = chain.then(async () => {
      const s = await load();
      const r = await fn(s);
      if (r?.changed) await save();
      return r?.result;
    });
    chain = run.catch(() => {});
    return run;
  }

  const entry = (s, sid) => (s[sid] ??= { items: [], paused: false, after: null, sentAt: null });
  const tidy = (s, sid) => {
    const q = s[sid];
    if (q && !q.items.length && !q.paused) {
      // Keep `after` while a delivered prompt may still be running: the next one waits for it.
      if (!q.sentAt || now() - q.sentAt > STUCK_MS) delete s[sid];
    }
  };
  const sidOf = (session) => {
    const sid = session?.session_id;
    if (typeof sid !== 'string' || !SESSION_ID_RE.test(sid) || sid.startsWith('pid-')) {
      throw new QueueError('this session has no session id yet', 409);
    }
    return sid;
  };

  /** What the UI shows: the items and whether the queue is paused or holding until the next run. */
  function view(s, sid) {
    const q = s[sid];
    if (!q) return { items: [], paused: false, held: false };
    return {
      items: q.items.map(({ id, text, createdAt }) => ({ id, text, createdAt })),
      paused: q.paused === true,
      held: !q.paused && q.held === true,
    };
  }

  function newId(q) {
    for (;;) {
      const id = `q-${randomId()}`;
      if (!q.items.some((it) => it.id === id)) return id;
    }
  }

  function itemOf(q, itemId) {
    const i = q?.items.findIndex((it) => it.id === itemId) ?? -1;
    if (i < 0) throw new QueueError('no such queued message', 404);
    return i;
  }

  /** Type `text` in now; the next queued prompt waits for the run it starts. */
  async function deliver(q, session, text) {
    await send(session, text);
    q.sentAt = now();
    q.after = q.sentAt;
    q.held = false;
  }

  function schedule(ms = tickMs) {
    if (stopped) return;
    clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      tick()
        .catch((err) => log(`[queue] tick failed: ${err?.message ?? err}`))
        .finally(() => schedule());
    }, ms);
    timer.unref?.();
  }

  /** The sessions with something to do: deliver to the idle ones whose run has ended. */
  async function tick() {
    const s = await load();
    if (!Object.keys(s).length) return;
    const sessions = await listSessions().catch(() => null);
    if (!sessions) return;
    await locked(async (st) => {
      let changed = false;
      for (const [sid, q] of Object.entries(st)) {
        const session = findSession(sessions, sid);
        if (!session) continue; // gone for now (a restart): keep the queue
        if (session.status !== 'idle') continue; // busy, or waiting on you: never now
        const at = Number(session.updated_at) || 0;
        const ended = q.after == null || at > q.after || (q.sentAt != null && now() - q.sentAt > STUCK_MS && at <= q.after);
        if (q.paused) {
          // The paused run is over (or there was none): hold through this idle, then clear.
          q.paused = false;
          q.held = q.items.length > 0;
          q.after = Math.max(at, q.after ?? 0);
          q.sentAt = null;
          changed = true;
          continue;
        }
        if (!q.items.length) {
          tidy(st, sid);
          changed = true;
          continue;
        }
        if (!ended) continue;
        const [item] = q.items;
        try {
          await deliver(q, session, item.text);
          q.items.shift();
          log(`[queue] ${sid.slice(0, 8)}: sent a queued prompt (${q.items.length} left)`);
        } catch (err) {
          log(`[queue] ${sid.slice(0, 8)}: delivery failed: ${err?.message ?? err}`);
          q.after = now(); // try again after the next run, not every tick
        }
        changed = true;
      }
      return { changed };
    });
  }

  return {
    start() {
      stopped = false;
      schedule(0);
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
      timer = null;
    },
    tick,

    /** The session's queue, for the chat poll (null when it has none). */
    async peek(session) {
      const sid = session?.session_id;
      if (typeof sid !== 'string' || !SESSION_ID_RE.test(sid)) return null;
      const s = await load();
      return s[sid] ? view(s, sid) : null;
    },

    /** How many prompts wait per session id (for list badges). */
    async counts() {
      const s = await load();
      const out = {};
      for (const [sid, q] of Object.entries(s)) if (q.items.length) out[sid] = q.items.length;
      return out;
    },

    async get(session) {
      const sid = sidOf(session);
      return locked((s) => ({ result: view(s, sid) }));
    },

    async add(session, body) {
      const sid = sidOf(session);
      const text = validText(body?.text);
      const r = await locked((s) => {
        const q = entry(s, sid);
        if (q.items.length >= MAX_ITEMS) throw new QueueError(`the queue is full (${MAX_ITEMS} messages)`, 409);
        const item = { id: newId(q), text, createdAt: now() };
        q.items.push(item);
        return { changed: true, result: { item, ...view(s, sid) } };
      });
      schedule(0);
      return r;
    },

    async edit(session, itemId, body) {
      const sid = sidOf(session);
      const text = validText(body?.text);
      return locked((s) => {
        const q = s[sid];
        q.items[itemOf(q, itemId)].text = text;
        return { changed: true, result: view(s, sid) };
      });
    },

    async remove(session, itemId) {
      const sid = sidOf(session);
      return locked((s) => {
        const q = s[sid];
        q.items.splice(itemOf(q, itemId), 1);
        if (!q.items.length) q.held = false;
        tidy(s, sid);
        return { changed: true, result: view(s, sid) };
      });
    },

    /** Move a message: `to` is its new index. */
    async move(session, itemId, body) {
      const sid = sidOf(session);
      const to = body?.to;
      return locked((s) => {
        const q = s[sid];
        const from = itemOf(q, itemId);
        if (!Number.isInteger(to) || to < 0 || to >= q.items.length) throw new QueueError('invalid position', 400);
        const [it] = q.items.splice(from, 1);
        q.items.splice(to, 0, it);
        return { changed: true, result: view(s, sid) };
      });
    },

    /** Send one now, whatever the session is doing (busy: Claude Code takes it between steps). */
    async sendNow(session, itemId) {
      const sid = sidOf(session);
      return locked(async (s) => {
        const q = s[sid];
        const i = itemOf(q, itemId);
        await deliver(q, session, q.items[i].text);
        q.items.splice(i, 1);
        tidy(s, sid);
        return { changed: true, result: view(s, sid) };
      });
    },

    async pause(session, body) {
      const sid = sidOf(session);
      const paused = body?.paused === true;
      const r = await locked((s) => {
        const q = entry(s, sid);
        q.paused = paused;
        if (!paused) q.held = false;
        if (!paused && q.after != null && !q.sentAt) q.after = null; // resume: the next idle sends
        tidy(s, sid);
        return { changed: true, result: view(s, sid) };
      });
      schedule(0);
      return r;
    },
  };
}
