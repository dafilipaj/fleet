// The diff reviewer's comment threads. A thread is anchored on lines of the diff (path, side,
// start–end, the selected code) and is a conversation with the session: every comment or reply
// is typed into the session as a prompt carrying the thread's marker, `[review c-xxxxxx]`, and
// the answer is read back from the transcript — the assistant messages after the user message
// that carries the marker, up to the next user message. So the exchange is an ordinary part of
// the session's conversation and shows in the thread too.
//
// Two ways to send, like GitHub: a single comment goes out at once; draft comments wait for
// `submit`, which sends them all in one prompt (each still with its own marker, so every thread
// gets the answer to the review).
//
// Storage: one JSON file per session under `<state>/fleet/reviews/<session_id>.json` on the
// session's host (dir 0700, files 0600, atomic temp + rename). Replies are copied into the file
// as they are read, so a thread keeps its answer after the transcript window moves on.
import { randomBytes } from 'node:crypto';
import { promises as fsp } from 'node:fs';
import path from 'node:path';

export const MAX_BODY = 8000;
/** Lines of selected code quoted per comment (the rest is elided). */
export const MAX_CODE_LINES = 60;
/** The whole prompt (a review with many comments) stays under this. */
export const MAX_PROMPT = 24000;
const MAX_THREADS = 500;
const MARKER_RE = /\[review (c-[0-9a-f]{6,12})\]/g;
const THREAD_ID_RE = /^c-[0-9a-f]{6,12}$/;
const SESSION_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;

export class ReviewError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export const validThreadId = (id) => typeof id === 'string' && THREAD_ID_RE.test(id);

const LANG_BY_EXT = {
  js: 'js', mjs: 'js', cjs: 'js', jsx: 'jsx', ts: 'ts', tsx: 'tsx', rs: 'rust', py: 'python', rb: 'ruby',
  go: 'go', java: 'java', kt: 'kotlin', swift: 'swift', c: 'c', h: 'c', cpp: 'cpp', cs: 'csharp', php: 'php',
  sh: 'sh', zsh: 'sh', bash: 'sh', md: 'md', json: 'json', yml: 'yaml', yaml: 'yaml', toml: 'toml',
  css: 'css', scss: 'scss', html: 'html', sql: 'sql', ex: 'elixir', exs: 'elixir', hbs: 'handlebars',
};
export function langOf(file) {
  const ext = path.extname(file).slice(1).toLowerCase();
  return LANG_BY_EXT[ext] ?? '';
}

export function rangeLabel(t) {
  const lines = t.start === t.end ? `${t.start}` : `${t.start}-${t.end}`;
  return `${t.display ?? t.path}:${lines}${t.side === 'old' ? ' (removed lines)' : ''}`;
}

/**
 * How the prompt names a file: relative to the session's cwd when the checkout is inside it
 * (`worktrees/api-fix/app/x.rb` from a meta repo), else absolute; without a root, as given.
 */
export function displayPath(root, file, cwd) {
  if (!root) return file;
  const full = path.join(root, file);
  if (cwd) {
    const rel = path.relative(cwd, full);
    if (rel && !rel.startsWith('..') && !path.isAbsolute(rel)) return rel;
  }
  return full;
}

/** A fenced block that its own content cannot close. */
function fence(code, lang) {
  let ticks = '```';
  while (code.includes(ticks)) ticks += '`';
  return `${ticks}${lang}\n${code}\n${ticks}`;
}

function quoted(t) {
  const lines = t.code.length > MAX_CODE_LINES ? [...t.code.slice(0, MAX_CODE_LINES), `… (${t.code.length - MAX_CODE_LINES} more lines)`] : t.code;
  return fence(lines.join('\n'), langOf(t.path));
}

/** The prompt for a comment sent on its own. */
export function commentPrompt(t, body) {
  return `[review ${t.id}] Review comment on ${rangeLabel(t)}:\n${quoted(t)}\n\n${body.trim()}`;
}

/** A follow-up in an existing thread. */
export function replyPrompt(t, body) {
  return `[review ${t.id}] Re: ${rangeLabel(t)}\n\n${body.trim()}`;
}

/** Every draft in one prompt: the overall note, then each comment with its own marker. */
export function reviewPrompt(threads, body = '') {
  const n = threads.length;
  const parts = [`Code review of your changes: ${n} comment${n === 1 ? '' : 's'}. Address each one.`];
  if (body.trim()) parts.push(body.trim());
  threads.forEach((t, i) => {
    parts.push(`## ${i + 1}. [review ${t.id}] ${rangeLabel(t)}\n${quoted(t)}\n\n${t.entries[0].body.trim()}`);
  });
  return parts.join('\n\n');
}

function validBody(body) {
  if (typeof body !== 'string' || !body.trim()) throw new ReviewError('body must be a non-empty string', 400);
  if (body.length > MAX_BODY) throw new ReviewError(`body too long (max ${MAX_BODY} chars)`, 400);
  return body.trim();
}

/** A new thread's anchor from the request body. */
export function parseAnchor(b) {
  if (!b || typeof b !== 'object') throw new ReviewError('body must be a JSON object', 400);
  const { path: p, side = 'new', start, end = start, code, root = null } = b;
  if (root != null && (typeof root !== 'string' || !path.isAbsolute(root) || root.includes('\0') || root.length > 4096 || path.normalize(root) !== root)) {
    throw new ReviewError('invalid root', 400);
  }
  if (typeof p !== 'string' || !p || p.length > 1024 || p.includes('\0') || p.startsWith('/')) throw new ReviewError('invalid path', 400);
  if (side !== 'new' && side !== 'old') throw new ReviewError('side must be "new" or "old"', 400);
  if (!Number.isInteger(start) || !Number.isInteger(end) || start < 1 || end < start || end - start > 2000) throw new ReviewError('invalid line range', 400);
  if (!Array.isArray(code) || code.length > 2001 || !code.every((l) => typeof l === 'string' && l.length <= 4000)) throw new ReviewError('code must be an array of lines', 400);
  // A PR the session made that has no worktree here: the comment is on the PR's code.
  const pr = b.pr ?? null;
  if (pr != null && (typeof pr !== 'string' || !PR_REF_RE.test(pr))) throw new ReviewError('invalid pr', 400);
  return { root, path: p, side, start, end, code, ...(pr ? { pr } : {}) };
}

const PR_REF_RE = /^[A-Za-z0-9_.-]{1,100}\/[A-Za-z0-9_.-]{1,100}#[1-9][0-9]{0,9}$/;

/**
 * Which user turns carry which thread markers, and the assistant's answer to each:
 * → Map<threadId, [{ ts, queued, replies: [{ text, ts, final }], done }]> in transcript order.
 */
export function turnsByThread(messages) {
  const out = new Map();
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    if (m.role !== 'user' || typeof m.text !== 'string') continue;
    const ids = new Set([...m.text.matchAll(MARKER_RE)].map((x) => x[1]));
    if (!ids.size) continue;
    const replies = [];
    let next = false;
    if (!m.queued) {
      for (let j = i + 1; j < messages.length; j++) {
        const r = messages[j];
        if (r.role === 'user' && !r.queued) {
          next = true;
          break;
        }
        if (r.role === 'assistant' && r.kind === 'assistant' && r.text) replies.push({ text: r.text, ts: r.ts ?? null, final: Boolean(r.final) });
      }
    }
    // Done: the turn ended with a final message, or another prompt came after it.
    const done = next || (replies.length > 0 && replies.at(-1).final);
    for (const id of ids) {
      if (!out.has(id)) out.set(id, []);
      out.get(id).push({ ts: m.ts ?? null, queued: Boolean(m.queued), replies, done });
    }
  }
  return out;
}

/**
 * @param {object} deps
 *   dir       where the per-session files go
 *   send      (session, text) → typed into the session (lib/backends.mjs#send)
 *   messages  (session) → transcript messages ([{ role, kind, text, ts, final, queued }]) or null
 *   now, randomId   (tests)
 */
export function createReviews({ dir, send, messages, now = () => Date.now(), randomId = () => randomBytes(3).toString('hex') }) {
  const locks = new Map(); // session id → the tail of its write chain
  let writeSeq = 0;

  const fileOf = (sid) => {
    if (!SESSION_ID_RE.test(sid)) throw new ReviewError('invalid session id', 400);
    return path.join(dir, `${sid}.json`);
  };

  async function read(sid) {
    try {
      const data = JSON.parse(await fsp.readFile(fileOf(sid), 'utf8'));
      return { session: sid, threads: Array.isArray(data.threads) ? data.threads : [], reviews: Array.isArray(data.reviews) ? data.reviews : [] };
    } catch (err) {
      if (err?.code === 'ENOENT') return { session: sid, threads: [], reviews: [] };
      throw err;
    }
  }

  async function write(sid, state) {
    await fsp.mkdir(dir, { recursive: true, mode: 0o700 });
    const file = fileOf(sid);
    const tmp = `${file}.${process.pid}.${++writeSeq}.tmp`;
    await fsp.writeFile(tmp, `${JSON.stringify({ session: sid, threads: state.threads, reviews: state.reviews }, null, 2)}\n`, { mode: 0o600 });
    await fsp.rename(tmp, file);
  }

  /** Run `fn(state)` under the session's lock; it returns { state?, result }. */
  function locked(sid, fn) {
    const prev = locks.get(sid) ?? Promise.resolve();
    const run = prev.then(async () => {
      const state = await read(sid);
      const r = await fn(state);
      if (r?.state) await write(sid, r.state);
      return r?.result;
    });
    const tail = run.catch(() => {});
    locks.set(sid, tail);
    tail.then(() => {
      if (locks.get(sid) === tail) locks.delete(sid);
    });
    return run;
  }

  const newId = (state) => {
    for (;;) {
      const id = `c-${randomId()}`;
      if (!state.threads.some((t) => t.id === id)) return id;
    }
  };

  async function deliver(session, text) {
    if (text.length > MAX_PROMPT) throw new ReviewError(`the prompt is too long (${text.length} > ${MAX_PROMPT} chars): shorten the comments or select fewer lines`, 400);
    await send(session, text);
  }

  /**
   * Attach the transcript's answers to each sent entry. An entry remembers the timestamp of
   * the user turn it matched, so it keeps its turn while older ones leave the window; new
   * entries take the next unclaimed turns in order. → whether anything changed.
   */
  function merge(state, msgs) {
    const turns = turnsByThread(msgs ?? []);
    let changed = false;
    for (const t of state.threads) {
      const list = turns.get(t.id) ?? [];
      const claimed = new Set(t.entries.map((e) => e.turnTs).filter(Boolean));
      let cursor = 0;
      for (const e of t.entries) {
        if (e.sentAt == null) continue;
        let turn = e.turnTs ? list.find((x) => x.ts === e.turnTs) : null;
        if (!turn && !e.turnTs) {
          while (cursor < list.length && (claimed.has(list[cursor].ts) || (list[cursor].ts && list[cursor].ts < e.sentAt - 60_000))) cursor++;
          turn = list[cursor] ?? null;
          if (turn) {
            cursor++;
            // A queued prompt gets a new timestamp when Claude takes it: claim it only then.
            if (turn.ts && !turn.queued) {
              e.turnTs = turn.ts;
              claimed.add(turn.ts);
              changed = true;
            }
          }
        }
        // Not in the window: matched before (it scrolled out — keep what was stored), or not
        // in the transcript yet (typed, not registered).
        if (!turn) continue;
        const st = turn.queued ? 'queued' : turn.done ? 'done' : 'working';
        if (st !== e.state || JSON.stringify(turn.replies) !== JSON.stringify(e.replies ?? [])) {
          e.state = st;
          e.replies = turn.replies;
          changed = true;
        }
      }
    }
    return changed;
  }

  const view = (state) => ({
    threads: state.threads,
    reviews: state.reviews,
    drafts: state.threads.filter((t) => t.status === 'draft').length,
  });

  return {
    /** All threads, with the replies read from the transcript now. */
    async list(session) {
      const sid = session.session_id;
      const msgs = await messages(session).catch(() => null);
      return locked(sid, (state) => {
        const changed = merge(state, msgs);
        return { state: changed ? state : null, result: view(state) };
      });
    },

    /** A new thread: `draft: true` waits for submit, else the comment is sent now. */
    async create(session, body) {
      const anchor = parseAnchor(body);
      const text = validBody(body.body);
      const draft = body.draft === true;
      const sid = session.session_id;
      return locked(sid, async (state) => {
        if (state.threads.length >= MAX_THREADS) throw new ReviewError('too many threads for this session', 409);
        const t = {
          id: newId(state),
          ...anchor,
          display: anchor.pr ? `${anchor.path} in PR ${anchor.pr} (no worktree of it here)` : displayPath(anchor.root, anchor.path, session.cwd ?? null),
          base: typeof body.base === 'string' ? body.base.slice(0, 64) : null,
          createdAt: now(),
          status: draft ? 'draft' : 'sent',
          resolved: false,
          reviewId: null,
          entries: [{ body: text, sentAt: draft ? null : now(), state: draft ? 'draft' : 'sent', replies: [] }],
        };
        if (!draft) await deliver(session, commentPrompt(t, text));
        state.threads.push(t);
        return { state, result: { thread: t, ...view(state) } };
      });
    },

    /** A follow-up in a sent thread (always at once). */
    async reply(session, threadId, body) {
      const text = validBody(body?.body);
      return locked(session.session_id, async (state) => {
        const t = state.threads.find((x) => x.id === threadId);
        if (!t) throw new ReviewError(`unknown thread: ${threadId}`, 404);
        if (t.status === 'draft') throw new ReviewError('this comment is still a draft: submit the review first', 409);
        await deliver(session, replyPrompt(t, text));
        t.entries.push({ body: text, sentAt: now(), state: 'sent', replies: [] });
        t.resolved = false;
        return { state, result: { thread: t, ...view(state) } };
      });
    },

    /** Edit a draft's text, or resolve / reopen a thread. */
    async update(session, threadId, body) {
      return locked(session.session_id, async (state) => {
        const t = state.threads.find((x) => x.id === threadId);
        if (!t) throw new ReviewError(`unknown thread: ${threadId}`, 404);
        if (body?.body != null) {
          if (t.status !== 'draft') throw new ReviewError('only a draft can be edited', 409);
          t.entries[0].body = validBody(body.body);
        }
        if (body?.resolved != null) t.resolved = Boolean(body.resolved);
        return { state, result: { thread: t, ...view(state) } };
      });
    },

    /** Drop a thread (only from the review: anything already sent stays in the conversation). */
    async remove(session, threadId) {
      return locked(session.session_id, async (state) => {
        const i = state.threads.findIndex((x) => x.id === threadId);
        if (i < 0) throw new ReviewError(`unknown thread: ${threadId}`, 404);
        state.threads.splice(i, 1);
        return { state, result: view(state) };
      });
    },

    /**
     * Send every draft in one prompt (with an optional overall note). `root`: only the drafts on
     * that checkout (a review scoped to one worktree / PR); a thread without a root is the cwd's.
     */
    async submit(session, body) {
      const note = typeof body?.body === 'string' ? body.body : '';
      if (note.length > MAX_BODY) throw new ReviewError(`body too long (max ${MAX_BODY} chars)`, 400);
      const root = body?.root ?? null;
      if (root != null && (typeof root !== 'string' || !path.isAbsolute(root) || root.length > 4096)) throw new ReviewError('invalid root', 400);
      const cwd = typeof session.cwd === 'string' ? session.cwd : null;
      const inScope = (t) => root == null || t.root === root || (t.root == null && cwd != null && (cwd === root || cwd.startsWith(`${root}/`)));
      return locked(session.session_id, async (state) => {
        const drafts = state.threads.filter((t) => t.status === 'draft' && inScope(t));
        if (!drafts.length) throw new ReviewError('no draft comments to submit', 409);
        await deliver(session, reviewPrompt(drafts, note));
        const at = now();
        const review = { id: `r-${randomId()}`, body: note.trim(), sentAt: at, threads: drafts.map((t) => t.id) };
        for (const t of drafts) {
          t.status = 'sent';
          t.reviewId = review.id;
          t.entries[0].sentAt = at;
          t.entries[0].state = 'sent';
        }
        state.reviews.push(review);
        return { state, result: { review, ...view(state) } };
      });
    },
  };
}

