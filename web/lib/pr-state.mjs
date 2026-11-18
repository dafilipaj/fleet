// PR states for the brief's PR resources: draft, open (in review), merged or closed, as this
// host's `gh` sees them. One `gh pr view` per PR, cached: open ones are re-read every few
// minutes, merged / closed ones rarely (they seldom change), failures retried after a while.
// A brief GET waits a short moment for missing states; what is not in by then comes on the
// next poll.
import { parsePrUrl, prKey } from './github.mjs';

const OPEN_TTL_MS = 3 * 60_000;
const DONE_TTL_MS = 6 * 60 * 60_000;
const ERROR_TTL_MS = 5 * 60_000;
const WAIT_MS = 3000;
const MAX_PRS = 40;
const MAX_PARALLEL = 6; // `gh` calls at once (a long brief's first read)
const MAX_CACHE = 500;

/** `gh pr view --json state,isDraft` output → 'draft' | 'open' | 'merged' | 'closed' | null. */
export function stateOf(json) {
  const s = String(json?.state ?? '').toUpperCase();
  if (s === 'MERGED') return 'merged';
  if (s === 'CLOSED') return 'closed';
  if (s === 'OPEN') return json?.isDraft ? 'draft' : 'open';
  return null;
}

/**
 * `gh(args, opts)` → { stdout }. Returns `states(urls)` → Map(url → state | null) for the urls
 * that are GitHub PR links; `states.headOf(url)`: the PR's head branch from the cache (null
 * until known) — the brief matches it to a worktree for a review of just that PR.
 */
export function createPrStates({ gh, now = Date.now, waitMs = WAIT_MS }) {
  const cache = new Map(); // prKey → { at, ttl, state, head }
  const inflight = new Map(); // prKey → Promise<state | null>
  let running = 0;
  const queue = [];
  const slot = () => (running < MAX_PARALLEL ? (running++, Promise.resolve()) : new Promise((r) => queue.push(r)));
  const release = () => {
    const next = queue.shift();
    if (next) next();
    else running--;
  };

  function fetchState(pr, key) {
    let p = inflight.get(key);
    if (p) return p;
    let head = null;
    p = slot()
      .then(() => gh(['pr', 'view', pr.url, '--json', 'state,isDraft,headRefName'], { timeout: 15000 }).finally(release))
      .then(({ stdout }) => {
        const json = JSON.parse(stdout);
        head = typeof json?.headRefName === 'string' && json.headRefName ? json.headRefName : null;
        return stateOf(json);
      })
      .catch(() => undefined)
      .then((state) => {
        const ttl = state === undefined ? ERROR_TTL_MS : state === 'merged' || state === 'closed' ? DONE_TTL_MS : OPEN_TTL_MS;
        if (cache.size >= MAX_CACHE) cache.delete(cache.keys().next().value);
        cache.set(key, { at: now(), ttl, state: state ?? null, head });
        inflight.delete(key);
        return state ?? null;
      });
    inflight.set(key, p);
    return p;
  }

  async function states(urls) {
    const out = new Map();
    const pending = [];
    const prs = [];
    for (const url of new Set(urls)) {
      const pr = parsePrUrl(url);
      if (pr && prs.length < MAX_PRS) prs.push({ url, pr });
    }
    for (const { url, pr } of prs) {
      const key = prKey(pr.repo, pr.number);
      const hit = cache.get(key);
      if (hit) out.set(url, hit.state);
      if (!hit || now() - hit.at > hit.ttl) {
        const p = fetchState(pr, key);
        // A stale value is served at once and refreshed behind it; a missing one is waited for.
        if (!hit) pending.push(p.then((s) => out.set(url, s)));
      }
    }
    if (pending.length) {
      let t;
      await Promise.race([Promise.all(pending), new Promise((r) => (t = setTimeout(r, waitMs)))]);
      clearTimeout(t);
    }
    return new Map(out); // late answers land in the cache, not in a returned map
  }
  states.headOf = (url) => {
    const pr = parsePrUrl(url);
    return (pr && cache.get(prKey(pr.repo, pr.number))?.head) || null;
  };
  return states;
}
