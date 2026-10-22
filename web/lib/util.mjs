// Pure helpers — no I/O, easy to unit test.

/** Status ordering: things that need you first. `shell` is Claude Code's idle with a background shell running. */
export const STATUS_RANK = { waiting: 0, busy: 1, idle: 2, shell: 2, unknown: 3 };

export function statusRank(status) {
  const r = STATUS_RANK[String(status ?? '').toLowerCase()];
  return r === undefined ? STATUS_RANK.unknown : r;
}

/** Sort sessions: waiting > busy > idle > unknown, then updated_at desc. Returns a new array. */
export function sortSessions(sessions) {
  return [...(sessions ?? [])].sort((a, b) => {
    const ra = statusRank(a?.status);
    const rb = statusRank(b?.status);
    if (ra !== rb) return ra - rb;
    const ua = Number(a?.updated_at) || 0;
    const ub = Number(b?.updated_at) || 0;
    return ub - ua;
  });
}

/**
 * Take the last `n` lines of raw terminal text: strip trailing
 * whitespace per line and drop trailing blank lines *before* counting, so a pane
 * taller than its output (a fresh shell, a cleared session) does not tail to nothing.
 */
export function tailLines(text, n) {
  if (typeof text !== 'string' || text.length === 0) return '';
  const all = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split('\n');
  const count = Math.max(0, Math.floor(Number(n) || 0));
  const trimmed = all.map((l) => l.replace(/[ \t ]+$/, ''));
  while (trimmed.length > 0 && trimmed[trimmed.length - 1].trim() === '') trimmed.pop();
  return (count > 0 ? trimmed.slice(-count) : trimmed).join('\n');
}

export function clampLines(value, def = 200, min = 10, max = 2000) {
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n)) return def;
  return Math.min(max, Math.max(min, n));
}

/**
 * Resolve a host name against self + peers.
 * → { kind: 'self' } | { kind: 'peer', url } | { kind: 'unknown' }
 */
export function resolveHost(host, { self, peers = {} } = {}) {
  if (!host || typeof host !== 'string') return { kind: 'unknown' };
  if (host === self) return { kind: 'self' };
  const url = peers[host];
  if (typeof url === 'string' && url) return { kind: 'peer', url };
  return { kind: 'unknown' };
}

/**
 * Find a session by exact session_id, or by a unique prefix of >= 8 chars.
 * → session object or null.
 */
export function findSession(sessions, id) {
  if (!Array.isArray(sessions) || typeof id !== 'string' || id === '') return null;
  const exact = sessions.find((s) => s?.session_id === id);
  if (exact) return exact;
  if (id.length < 8) return null;
  const matches = sessions.filter((s) => typeof s?.session_id === 'string' && s.session_id.startsWith(id));
  return matches.length === 1 ? matches[0] : null;
}

/** Validate the text payload for /send. Throws HttpError-ish {status, message} shape via return. */
export function validateSendText(text) {
  if (typeof text !== 'string') return { ok: false, error: 'text must be a string' };
  if (text.length === 0) return { ok: false, error: 'text must not be empty' };
  if (text.trim().length === 0) return { ok: false, error: 'text must not be blank' };
  if (text.length > 8000) return { ok: false, error: 'text too long (max 8000 chars)' };
  return { ok: true, text };
}

/** Same cap as the CLI (`fleet rename`, core::title::MAX_TITLE). */
export const MAX_TITLE = 64;

/** A session title for `rename`: trimmed, single line, 1..64 chars. */
export function validateTitle(title) {
  if (typeof title !== 'string') return { ok: false, error: 'title must be a string' };
  const t = title.trim();
  if (!t) return { ok: false, error: 'title must not be blank' };
  if (/[\r\n]/.test(t)) return { ok: false, error: 'title must be a single line' };
  if ([...t].length > MAX_TITLE) return { ok: false, error: `title too long (max ${MAX_TITLE} chars)` };
  return { ok: true, title: t };
}

export const ALLOWED_KEYS = ['Enter', 'Escape', 'Up', 'Down'];

export function validateKey(key) {
  if (typeof key !== 'string' || !ALLOWED_KEYS.includes(key)) {
    return { ok: false, error: `key must be one of ${ALLOWED_KEYS.join(', ')}` };
  }
  return { ok: true, key };
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
