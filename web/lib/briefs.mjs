// Session briefs: a small markdown doc per session — what it is doing and where, what it
// produced, its todos — so a new session can be started from it and just continue. Format:
// lib/brief-format.mjs; contract and budget: docs/architecture.md → "Session briefs".
//
// The web server owns generation: it is the always-on process on every host, it runs inside
// tmux (where `claude -p` has a logged-in keychain) and it already reads transcripts. Hybrid:
//   - Resources and (when the session keeps todos) the Todos come from the transcript, no model
//     (lib/brief-extract.mjs), merged into the file without dropping a line a human wrote;
//   - the Summary (and the Todos otherwise) come from `claude -p --model haiku`, fed the old
//     brief plus ONLY the conversation since `generatedThrough`, heavily truncated.
// Model calls are the expensive part, so they are gated hard: an idle session (≥ idleMs), new
// content (≥ minNewTurns user turns or ≥ minNewChars), ≥ minIntervalMs since that session's last
// call, one call at a time fleet-host-wide, ≤ maxCallsPerHour. Each call is logged `[briefs] …`.
import { promises as fsp } from 'node:fs';
import fs from 'node:fs';
import crypto from 'node:crypto';
import os from 'node:os';
import path from 'node:path';

import {
  addDismissed,
  continuePrompt,
  emptyBrief,
  formatResource,
  formatTodos,
  mergeGit,
  mergeResources,
  parseBrief,
  parseModelOutput,
  removedResourceKeys,
  serializeBrief,
} from './brief-format.mjs';
import { readDelta } from './brief-extract.mjs';

export const SESSION_ID_RE = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/;
const HOUR_MS = 60 * 60 * 1000;
const GIT_TTL_MS = 60 * 1000;
/** Checkouts looked up with git for the Worktrees list per GET (cached like the cwd's). */
const MAX_WORKTREE_ROOTS = 20;

// ------------------------------------------------------------------ storage

/** One file per session: `<dir>/<session id>.md`, written atomically (tmp + rename). */
export function createBriefStore({ dir }) {
  if (typeof dir !== 'string' || !path.isAbsolute(dir)) throw new TypeError('briefs dir must be an absolute path');
  function file(id) {
    if (!SESSION_ID_RE.test(String(id))) throw new Error(`not a session id: ${id}`);
    return path.join(dir, `${id}.md`);
  }
  return {
    dir,
    file,
    async read(id) {
      try {
        return await fsp.readFile(file(id), 'utf8');
      } catch (err) {
        if (err?.code === 'ENOENT') return null;
        throw err;
      }
    },
    async write(id, text) {
      const target = file(id);
      await fsp.mkdir(dir, { recursive: true, mode: 0o700 });
      const tmp = `${target}.${process.pid}.${crypto.randomBytes(4).toString('hex')}.tmp`;
      try {
        await fsp.writeFile(tmp, text, { mode: 0o600 });
        await fsp.rename(tmp, target);
      } catch (err) {
        await fsp.rm(tmp, { force: true }).catch(() => {});
        throw err;
      }
    },
  };
}

// ------------------------------------------------------------------ claude -p

/** Mirrors `core::naming::ask_claude`: prompt on stdin (the tools flag is variadic), no MCP, no tools. */
export const CLAUDE_ARGS = ['--strict-mcp-config', '--disallowed-tools', 'Bash,Edit,Write,Read,WebFetch,WebSearch,Task'];

/**
 * One non-interactive model call → its stdout. Runs in a neutral dir (a project dir would load
 * its CLAUDE.md, skills and plugins: slower and dearer) with `--no-session-persistence` (no
 * transcript per call; dropped once if this `claude` doesn't know the flag). No `--bare`: it
 * demands an API key and ignores the OAuth login.
 */
export function createClaudeAsk({ bin = 'claude', run, timeoutMs = 120 * 1000, cwd = os.tmpdir(), env = process.env }) {
  if (typeof run !== 'function') throw new TypeError('run must be a function');
  let noPersist = true;
  return async function ask({ prompt, model = 'haiku' }) {
    const args = ['-p', '--model', model, ...(noPersist ? ['--no-session-persistence'] : []), ...CLAUDE_ARGS];
    try {
      const { stdout } = await run(bin, args, { input: prompt, timeout: timeoutMs, cwd, env, maxBuffer: 1024 * 1024 });
      return stdout;
    } catch (err) {
      if (noPersist && /no-session-persistence/.test(String(err?.message))) {
        noPersist = false;
        return ask({ prompt, model });
      }
      const why = err?.killed ? `\`claude\` did not answer within ${Math.round(timeoutMs / 1000)}s` : `\`claude -p\` failed: ${String(err?.message ?? err).trim().split('\n').pop()}`;
      const e = new Error(why);
      e.timedOut = Boolean(err?.killed);
      throw e;
    }
  };
}

/**
 * Branch and checkout root of a directory, or null outside git. One `git` call, 2 s cap. →
 * { branch (null when detached), toplevel (absolute), linked (a linked worktree, not the main
 * checkout), worktree (toplevel when linked, else null) }.
 */
export async function gitInfo(cwd, { run }) {
  try {
    const { stdout } = await run('git', ['-C', cwd, 'rev-parse', '--abbrev-ref', 'HEAD', '--show-toplevel', '--git-dir', '--git-common-dir'], { timeout: 2000 });
    const [branch, top, gitDir, common] = stdout.trim().split('\n').map((s) => s.trim());
    if (!top) return null;
    // Relative paths are relative to the directory git ran in.
    const abs = (p) => path.resolve(cwd, p || '.git');
    const linked = Boolean(gitDir && common) && abs(gitDir) !== abs(common);
    return {
      branch: branch && branch !== 'HEAD' ? branch : null,
      toplevel: top,
      linked,
      worktree: linked ? top : null,
    };
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ the model prompt

function capText(s, max) {
  const t = String(s ?? '').trim();
  return t.length > max ? `${t.slice(0, max)} […]` : t;
}

/** The whole prompt handed to `claude -p`. Pure. */
export function buildPrompt({ brief, delta, host, cwd, branch, todosFromSession, maxBriefChars = 3000, recap = false }) {
  const where = [host ? `host ${host}` : null, cwd ? `directory ${cwd}` : null, branch ? `git branch ${branch}` : null].filter(Boolean).join(', ');
  const current = [`## Summary\n${brief.summary || '(none yet)'}`, todosFromSession ? null : `## Todos\n${brief.todosText || '(none yet)'}`]
    .filter(Boolean)
    .join('\n\n');
  const edited = brief.meta?.editedAt ? ' The user edited it by hand: what they wrote is authoritative — keep it, change it only where the conversation clearly moved on.' : '';
  const out = [
    'You maintain a short brief for a Claude Code session, so that a brand-new session could be started from it and just continue the work.',
    `Where the session runs: ${where || 'unknown'}.`,
    `The current brief.${edited}\n<brief>\n${capText(current, maxBriefChars)}\n</brief>`,
    `${recap ? 'The recent conversation' : 'What happened since the brief was last updated'} (user prompts and the assistant's replies, truncated):\n<conversation>\n${delta.text || '(nothing)'}\n</conversation>`,
    'Update the brief: keep what is still true, fix what changed, add what is new. Do not rewrite it from scratch.',
    'Answer with ONLY the following markdown, nothing before or after it:',
    todosFromSession
      ? '## Summary\n<at most 2 sentences, under 60 words: what exactly the session is doing and where (repo or directory, branch, host), and where it stands now>'
      : '## Summary\n<at most 2 sentences, under 60 words: what exactly the session is doing and where (repo or directory, branch, host), and where it stands now>\n\n## Todos\n- [x] <done todo>\n- [ ] <open todo>\n(3–8 short todos in order; keep existing todos and their wording while accurate, tick finished ones, add what comes next)',
  ];
  return out.join('\n\n');
}

// ------------------------------------------------------------------ the service

function shortHash(v) {
  return crypto.createHash('sha1').update(JSON.stringify(v)).digest('hex').slice(0, 12);
}

/** How a resource path is shown: relative to the session's cwd, else `~/…`, else absolute. */
export function displayPath(abs, cwd, home = os.homedir()) {
  if (cwd && (abs === cwd || abs.startsWith(`${cwd.replace(/\/+$/, '')}/`))) return path.relative(cwd, abs) || '.';
  if (home && abs.startsWith(`${home}/`)) return `~/${abs.slice(home.length + 1)}`;
  return abs;
}

/**
 * @param deps
 *   settings      config.briefs ({ enabled, model, idleMs, minIntervalMs, maxDeltaChars,
 *                 maxCallsPerHour, minNewTurns, minNewChars, maxBriefChars })
 *   self          this host's name (written into the frontmatter)
 *   store         createBriefStore
 *   extractor     lib/brief-extract.mjs#createBriefExtractor
 *   listSessions  () => Promise<session[] | null> — this host's live `list --json` rows
 *   ask           ({ prompt, model }) => Promise<string> — createClaudeAsk
 *   git           (cwd) => Promise<{ branch, toplevel, worktree } | null>
 *   exists        (abs path) => boolean (resources: files that are gone are skipped)
 *   now, setTimer, clearTimer, checkMs, log, home
 */
export function createBriefs({
  settings,
  self = 'local',
  store,
  extractor,
  listSessions = async () => null,
  ask,
  git = async () => null,
  exists = (p) => fs.existsSync(p),
  now = Date.now,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
  checkMs = 30 * 1000,
  log = () => {},
  home = os.homedir(),
}) {
  if (!store || !extractor || typeof ask !== 'function') throw new TypeError('store, extractor and ask are required');
  const cfg = settings;
  const mem = new Map(); // sessionId → { lastSize, lastGenAt, idleSeenAt, pending }
  const gitCache = new Map(); // cwd → { at, value }
  let calls = []; // timestamps of model calls in the last hour
  let busy = null; // session id of the call running now
  const waiting = new Set(); // manual regenerations queued behind it
  let chain = Promise.resolve();
  let lastRun = null;
  let capLoggedAt = 0;
  let timer = null;
  let stopped = true;

  const iso = (t = now()) => new Date(t).toISOString();
  const memFor = (id) => {
    let m = mem.get(id);
    if (!m) mem.set(id, (m = { lastSize: -1, lastGenAt: null, idleSeenAt: null, pending: false }));
    return m;
  };

  function callsLastHour() {
    const t = now();
    calls = calls.filter((c) => t - c < HOUR_MS);
    return calls.length;
  }

  async function load(id) {
    const text = await store.read(id);
    return text == null ? { brief: emptyBrief(id), exists: false } : { brief: parseBrief(text), exists: true };
  }

  async function save(id, brief) {
    brief.meta = { ...brief.meta, session: id, updated: iso() };
    await store.write(id, serializeBrief(brief));
  }

  async function gitFor(cwd) {
    if (!cwd) return null;
    const hit = gitCache.get(cwd);
    if (hit && now() - hit.at < GIT_TTL_MS) return hit.value;
    const value = await git(cwd).catch(() => null);
    gitCache.set(cwd, { at: now(), value });
    if (gitCache.size > 200) gitCache.delete(gitCache.keys().next().value);
    return value;
  }

  /** Fold extracted resources / todos into `brief`. → true when its content changed. */
  function applyExtraction(brief, ext, session, gi) {
    let changed = false;
    const cwd = session?.cwd ?? null;
    const items = [];
    for (const r of ext?.resources ?? []) {
      if (r.path) {
        if (!exists(r.path)) continue;
        items.push({ kind: r.kind, path: displayPath(r.path, cwd, home) });
      } else items.push({ kind: r.kind, label: r.label, url: r.url });
    }
    items.reverse(); // oldest first: the section reads in the order things happened
    const merged = mergeResources(brief.resourcesText, items, brief.meta.dismissed ?? []);
    if (merged.added) {
      brief.resourcesText = merged.text;
      changed = true;
    }
    if (gi?.toplevel) {
      // One `Git:` line (branch + checkout), replaced in place when either changes.
      const item = { kind: 'Git', branch: gi.branch, path: displayPath(gi.toplevel, null, home), linked: Boolean(gi.linked ?? gi.worktree) };
      const g = mergeGit(brief.resourcesText, item, { migrated: brief.meta.git != null, dismissed: brief.meta.dismissed ?? [] });
      if (g.changed) {
        brief.resourcesText = g.text;
        brief.meta.git = formatResource(item);
        changed = true;
      }
    }
    if (ext?.todos) {
      const h = shortHash(ext.todos);
      if (h !== brief.meta.todos) {
        brief.todosText = formatTodos(ext.todos);
        brief.meta.todos = h;
        changed = true;
      }
    }
    // Where it worked (the Worktrees list), kept so a gone session still shows them.
    const dirs = ext?.dirs ?? [];
    if (dirs.length && JSON.stringify(dirs) !== JSON.stringify(brief.meta.dirs ?? [])) {
      brief.meta.dirs = dirs;
      changed = true;
    }
    // Where the session lives: informational, written along with the next content change.
    if (self) brief.meta.host = self;
    if (cwd) brief.meta.cwd = cwd;
    return changed;
  }

  function lastGenAt(id, brief) {
    return memFor(id).lastGenAt ?? (Date.parse(brief.meta.generatedAt ?? '') || null);
  }

  /** The model call for one session. Caller holds the slot (`busy`). */
  async function generate(session, ext, gi, { reason }) {
    const id = session.session_id;
    const m = memFor(id);
    const { brief } = await load(id);
    let delta = await readDelta(ext.file, brief.meta.generatedThrough ?? 0, { maxChars: cfg.maxDeltaChars });
    let recap = false;
    if (reason === 'manual' && delta.messages === 0) {
      // Nothing new: a manual regenerate re-reads the recent conversation instead.
      delta = { ...(await readDelta(ext.file, 0, { maxChars: cfg.maxDeltaChars })), end: delta.end };
      recap = true;
    }
    const prompt = buildPrompt({
      brief,
      delta,
      host: self,
      cwd: session.cwd ?? null,
      branch: gi?.branch ?? null,
      todosFromSession: Boolean(ext.todos),
      maxBriefChars: cfg.maxBriefChars,
      recap,
    });
    const startedAt = now();
    calls.push(startedAt);
    m.lastGenAt = startedAt;
    m.pending = false;
    const tag = `${reason} ${id.slice(0, 8)}`;
    log(`[briefs] ${tag}: claude -p --model ${cfg.model}, ${delta.messages} msg(s) / ${delta.userTurns} user turn(s), ${prompt.length} chars in (${callsLastHour()}/${cfg.maxCallsPerHour} this hour)`);
    let out;
    try {
      out = await ask({ prompt, model: cfg.model });
    } catch (err) {
      lastRun = { at: startedAt, ms: now() - startedAt, id, reason, ok: false, error: err?.message ?? String(err), inputChars: prompt.length };
      log(`[briefs] ${tag} failed: ${lastRun.error}`);
      return lastRun;
    }
    const parsed = parseModelOutput(out);
    if (!parsed) {
      lastRun = { at: startedAt, ms: now() - startedAt, id, reason, ok: false, error: 'unusable model answer — brief kept', inputChars: prompt.length, outputChars: out.length };
      log(`[briefs] ${tag}: unusable answer (${out.length} chars) — brief kept`);
      return lastRun;
    }
    // Re-read: a human edit that landed while the model ran wins, and the next pass retries.
    const { brief: fresh } = await load(id);
    if (fresh.meta.editedAt && Date.parse(fresh.meta.editedAt) >= startedAt) {
      lastRun = { at: startedAt, ms: now() - startedAt, id, reason, ok: false, error: 'edited during generation — answer discarded', inputChars: prompt.length };
      log(`[briefs] ${tag}: edited while generating — answer discarded`);
      return lastRun;
    }
    applyExtraction(fresh, ext, session, gi);
    fresh.summary = parsed.summary;
    if (!ext.todos && parsed.todos) fresh.todosText = formatTodos(parsed.todos);
    fresh.meta.generatedThrough = delta.end;
    fresh.meta.generatedAt = iso(startedAt);
    await save(id, fresh);
    lastRun = { at: startedAt, ms: now() - startedAt, id, reason, ok: true, inputChars: prompt.length, outputChars: out.length };
    log(`[briefs] ${tag}: updated in ${((now() - startedAt) / 1000).toFixed(1)}s (${out.length} chars out)`);
    return lastRun;
  }

  /** Extraction (always) + a model call when every gate passes. Background passes only. */
  async function autoUpdate(session) {
    const id = session.session_id;
    const m = memFor(id);
    const ext = await extractor.refresh(session);
    if (!ext) return 'no transcript';
    if (ext.size === m.lastSize && !m.pending) return 'unchanged';
    m.lastSize = ext.size;
    const gi = await gitFor(session.cwd);
    const { brief } = await load(id);
    if (applyExtraction(brief, ext, session, gi)) await save(id, brief);

    const since = lastGenAt(id, brief);
    if (since != null && now() - since < cfg.minIntervalMs) return (m.pending = true), 'min interval';
    const through = brief.meta.generatedThrough ?? 0;
    if (ext.offset <= through) return 'nothing new';
    if (busy || waiting.size) return (m.pending = true), 'busy';
    if (callsLastHour() >= cfg.maxCallsPerHour) {
      m.pending = true;
      if (now() - capLoggedAt > HOUR_MS / 4) {
        capLoggedAt = now();
        log(`[briefs] hourly cap reached (${cfg.maxCallsPerHour}/h) — model calls paused`);
      }
      return 'hourly cap';
    }
    const delta = await readDelta(ext.file, through, { maxChars: cfg.maxDeltaChars });
    if (delta.userTurns < cfg.minNewTurns && delta.chars < cfg.minNewChars) return (m.pending = false), 'too little new';
    busy = id;
    try {
      await generate(session, ext, gi, { reason: 'idle' });
    } finally {
      busy = null;
    }
    return 'generated';
  }

  function isIdle(session, t) {
    const m = memFor(session.session_id);
    // `shell` is Claude Code's idle with a background shell running (2.1.308+): idle too.
    if (session.status !== 'idle' && session.status !== 'shell' && session.status !== 'waiting') {
      m.idleSeenAt = null;
      return false;
    }
    m.idleSeenAt ??= t;
    // `updated_at` is the registry's last status change: idle since then.
    const since = Number(session.updated_at) > 0 ? Number(session.updated_at) : m.idleSeenAt;
    return t - since >= cfg.idleMs;
  }

  /** One background pass over this host's live sessions. */
  async function tick() {
    const sessions = await listSessions().catch(() => null);
    if (!Array.isArray(sessions)) return;
    const live = new Set(sessions.map((s) => s?.session_id).filter(Boolean));
    for (const id of [...mem.keys()]) {
      if (!live.has(id)) {
        mem.delete(id); // gone: ignored from now on
        extractor.forget(id);
      }
    }
    const t = now();
    for (const s of sessions) {
      if (!s?.session_id || !SESSION_ID_RE.test(s.session_id) || !isIdle(s, t)) continue;
      try {
        await autoUpdate(s);
      } catch (err) {
        log(`[briefs] ${s.session_id.slice(0, 8)}: ${err?.message ?? err}`);
      }
    }
  }

  function schedule() {
    if (stopped) return;
    timer = setTimer(async () => {
      await tick().catch(() => {});
      schedule();
    }, checkMs);
    timer?.unref?.();
  }

  /** The session's directory on this host, absolute (`~` expanded), or null. */
  function absCwdOf(brief, session) {
    const c = session?.cwd ?? brief.meta.cwd ?? null;
    if (typeof c !== 'string' || !c) return null;
    const abs = c === '~' ? home : c.startsWith('~/') ? path.join(home, c.slice(2)) : c;
    return path.isAbsolute(abs) ? path.normalize(abs) : null;
  }

  /**
   * The git checkouts the session works in: its cwd's first, then those of the directories it
   * was in (`meta.dirs`), then those of the files and specs in Resources (a session often edits
   * a worktree it never cd'd into). Deduped by root, at most
   * MAX_WORKTREE_ROOTS checkouts looked up. → [{ path (absolute), display, branch, linked }].
   */
  async function worktreesOf(brief, absCwd, gi) {
    const roots = new Map();
    const add = (g) => {
      if (g?.toplevel && !roots.has(g.toplevel)) {
        roots.set(g.toplevel, { path: g.toplevel, display: displayPath(g.toplevel, null, home), branch: g.branch ?? null, linked: Boolean(g.linked ?? g.worktree) });
      }
    };
    add(gi);
    const dirs = new Set();
    for (const d of Array.isArray(brief.meta.dirs) ? brief.meta.dirs : []) if (typeof d === 'string' && path.isAbsolute(d)) dirs.add(path.normalize(d));
    for (const r of brief.resources) {
      if (!r.path || (r.kind !== 'File' && r.kind !== 'Spec')) continue;
      const p = r.path.startsWith('~/') ? path.join(home, r.path.slice(2)) : path.isAbsolute(r.path) ? r.path : absCwd ? path.join(absCwd, r.path) : null;
      if (p) dirs.add(path.dirname(path.normalize(p)));
    }
    // The nearest directory with a `.git` (a dir: main checkout, a file: linked worktree) is the
    // checkout root — found without running git, so many files cost one lookup per checkout.
    const seen = new Map(); // dir → root | null
    const rootOf = (dir) => {
      const trail = [];
      let d = dir;
      let root = null;
      for (;;) {
        if (seen.has(d)) {
          root = seen.get(d);
          break;
        }
        trail.push(d);
        if (exists(path.join(d, '.git'))) {
          root = d;
          break;
        }
        const up = path.dirname(d);
        if (up === d) break;
        d = up;
      }
      for (const t of trail) seen.set(t, root);
      return root;
    };
    const found = new Set();
    for (const dir of dirs) {
      const root = rootOf(dir);
      if (root && !roots.has(root)) found.add(root);
    }
    for (const root of [...found].slice(0, MAX_WORKTREE_ROOTS)) add(await gitFor(root));
    return [...roots.values()];
  }

  /** What the API serves for one brief. `editorUrl` is the API layer's (lib/editor.mjs). */
  function view(id, brief, { exists: had, session = null, absCwd = null, gi = null, worktrees = [] } = {}) {
    const resources = brief.resources.map((r) => ({ kind: r.kind, label: r.label, url: r.url, path: r.path, text: r.text, branch: r.branch ?? null, linked: r.linked ?? null }));
    return {
      host: self,
      id,
      exists: had,
      markdown: serializeBrief(brief),
      parsed: {
        summary: brief.summary,
        resources,
        todos: brief.todos,
        plan: brief.todos, // deprecated alias of `todos` (the section was `## Plan`); removed in the next API version
      },
      absCwd,
      gitRoot: gi?.toplevel ?? null,
      worktrees,
      updated: brief.meta.updated ?? null,
      editedAt: brief.meta.editedAt ?? null,
      generatedAt: brief.meta.generatedAt ?? null,
      generatedThrough: brief.meta.generatedThrough ?? 0,
      generating: busy === id || waiting.has(id),
      enabled: cfg.enabled,
      continuePrompt: continuePrompt(brief, { host: brief.meta.host ?? self, cwd: session?.cwd ?? brief.meta.cwd ?? null }),
    };
  }

  return {
    store,
    start() {
      if (!cfg.enabled) return;
      stopped = false;
      schedule();
    },
    stop() {
      stopped = true;
      clearTimer(timer);
    },
    tick,
    autoUpdate,

    /** GET: the stored brief (with fresh no-model extraction when the session is live), or an empty skeleton. */
    async get(id, session = null) {
      let { brief, exists: had } = await load(id);
      const absCwd = absCwdOf(brief, session);
      const gi = absCwd && exists(absCwd) ? await gitFor(absCwd) : null;
      if (session) {
        // Cheap and model-free: bring Resources / Todos up to date for whoever is looking.
        const ext = await extractor.refresh(session).catch(() => null);
        if (ext && applyExtraction(brief, ext, session, gi)) {
          await save(id, brief);
          had = true;
          ({ brief } = await load(id));
        }
      }
      return view(id, brief, { exists: had, session, absCwd, gi, worktrees: await worktreesOf(brief, absCwd, gi) });
    },

    /** PUT: a human edit. Deleted resource lines are remembered as dismissed. */
    async put(id, markdown, session = null) {
      if (typeof markdown !== 'string') throw Object.assign(new Error('markdown must be a string'), { status: 400 });
      if (markdown.length > 60000) throw Object.assign(new Error('markdown too long (max 60000 chars)'), { status: 413 });
      const { brief: before } = await load(id);
      const incoming = parseBrief(markdown);
      const removed = removedResourceKeys(before, incoming);
      // Machine keys stay the server's; other keys a human added are kept.
      const meta = { ...incoming.meta, ...before.meta };
      meta.dismissed = addDismissed(before.meta.dismissed ?? [], removed);
      meta.editedAt = iso();
      if (session?.cwd) meta.cwd = session.cwd;
      meta.host = meta.host ?? self;
      await save(id, { ...incoming, meta });
      const { brief } = await load(id);
      const absCwd = absCwdOf(brief, session);
      const gi = absCwd && exists(absCwd) ? await gitFor(absCwd) : null;
      return view(id, brief, { exists: true, session, absCwd, gi, worktrees: await worktreesOf(brief, absCwd, gi) });
    },

    /**
     * POST regenerate: skips the idle / interval / new-content gates, not the one-at-a-time slot
     * nor the hourly cap. Returns at once; the call runs in the background (GET `generating`).
     * → { started, queued, generating } | throws { status: 429 } at the cap.
     */
    async regenerate(session) {
      const id = session.session_id;
      if (busy === id || waiting.has(id)) return { started: false, queued: waiting.has(id), generating: true };
      if (callsLastHour() + waiting.size >= cfg.maxCallsPerHour) {
        const oldest = calls[0] ?? now();
        throw Object.assign(new Error(`brief model calls are capped at ${cfg.maxCallsPerHour}/hour — try again later`), {
          status: 429,
          retryAfterMs: Math.max(0, oldest + HOUR_MS - now()),
        });
      }
      const ext = await extractor.refresh(session);
      if (!ext) throw Object.assign(new Error('transcript not found for this session'), { status: 404 });
      const queued = Boolean(busy);
      waiting.add(id);
      const job = chain.then(async () => {
        // Auto passes don't start while `waiting` is non-empty; wait out one already running.
        while (busy) await new Promise((r) => setTimer(r, 200));
        waiting.delete(id);
        busy = id;
        try {
          const fresh = (await extractor.refresh(session)) ?? ext;
          const gi = await gitFor(session.cwd);
          return await generate(session, fresh, gi, { reason: 'manual' });
        } catch (err) {
          log(`[briefs] manual ${id.slice(0, 8)}: ${err?.message ?? err}`);
          return null;
        } finally {
          busy = null;
        }
      });
      chain = job.catch(() => {});
      return { started: !queued, queued, generating: true, job };
    },

    status() {
      return {
        enabled: cfg.enabled,
        model: cfg.model,
        callsLastHour: callsLastHour(),
        maxCallsPerHour: cfg.maxCallsPerHour,
        generating: busy,
        lastRun,
      };
    },
  };
}
