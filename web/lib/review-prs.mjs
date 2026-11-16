// What the diff reviewer shows for a session is one entry per (repo, branch): a checkout the
// session works in, the PR on its branch, or both. This joins them: each checkout's GitHub repo
// and branch → its PR (`gh pr list --head`), and the PRs the session's brief lists → the
// checkout that has their head branch, else a diff straight from a clone of the repo (no
// worktree needed). PR lookups go through this host's `gh`, cached: open PRs are re-read every
// few minutes, merged / closed ones rarely, failures retried after a while; a lookup still in
// flight is not waited for long (the next poll picks it up).
import { parsePrUrl } from './github.mjs';

const OPEN_TTL_MS = 3 * 60_000;
const DONE_TTL_MS = 6 * 60 * 60_000;
const NONE_TTL_MS = 5 * 60_000;
const ERROR_TTL_MS = 5 * 60_000;
const WAIT_MS = 1000;
const MAX_CACHE = 500;
/** Brief PRs looked at per session at most. */
export const MAX_SESSION_PRS = 8;
const FIELDS = 'number,title,url,state,isDraft,headRefName,baseRefName,headRefOid';

/** `gh pr view/list --json …` → the reviewer's PR, or null. */
export function prOf(json, slug) {
  if (!json || !Number.isInteger(json.number)) return null;
  const s = String(json.state ?? '').toUpperCase();
  const state = s === 'MERGED' ? 'merged' : s === 'CLOSED' ? 'closed' : s === 'OPEN' ? (json.isDraft ? 'draft' : 'open') : null;
  const [owner, repo] = slug.split('/');
  return {
    owner,
    repo,
    number: json.number,
    title: typeof json.title === 'string' ? json.title : '',
    url: typeof json.url === 'string' ? json.url : `https://github.com/${slug}/pull/${json.number}`,
    state,
    headBranch: typeof json.headRefName === 'string' ? json.headRefName : null,
    baseBranch: typeof json.baseRefName === 'string' ? json.baseRefName : null,
    headOid: typeof json.headRefOid === 'string' ? json.headRefOid : null,
  };
}

export const prRef = (pr) => `${pr.owner}/${pr.repo}#${pr.number}`;
const lc = (s) => String(s ?? '').toLowerCase();

/**
 * @param {object} deps
 *   gh       (args, opts) → { stdout }
 *   differ   lib/diff.mjs#createDiffer (remote, branch, worktreeOn, defaultBranch)
 *   clones   async () → [{ name, path }] the repos under this host's roots (`fleet repos`); optional
 */
export function createReviewPrs({ gh, differ, clones = async () => [], now = Date.now, waitMs = WAIT_MS }) {
  const cache = new Map(); // key → { at, ttl, value }
  const inflight = new Map();

  function cached(key, load) {
    const hit = cache.get(key);
    const fresh = hit && now() - hit.at <= hit.ttl;
    if (!fresh && !inflight.has(key)) {
      const p = load()
        .then(
          (value) => ({ value, ttl: value == null ? NONE_TTL_MS : value.state === 'merged' || value.state === 'closed' ? DONE_TTL_MS : OPEN_TTL_MS }),
          () => ({ value: hit?.value ?? null, ttl: ERROR_TTL_MS }),
        )
        .then(({ value, ttl }) => {
          if (cache.size >= MAX_CACHE) cache.delete(cache.keys().next().value);
          cache.set(key, { at: now(), ttl, value });
          inflight.delete(key);
          return value;
        });
      inflight.set(key, p);
    }
    if (hit) return Promise.resolve(hit.value); // stale is served, refreshed behind it
    let t;
    return Promise.race([inflight.get(key), new Promise((r) => (t = setTimeout(() => r(undefined), waitMs)))]).finally(() => clearTimeout(t));
  }

  /** A PR by number (undefined: not known yet). */
  const byNumber = (slug, number) =>
    cached(`n:${lc(slug)}#${number}`, async () => prOf(JSON.parse((await gh(['pr', 'view', String(number), '--repo', slug, '--json', FIELDS], { timeout: 15000 })).stdout), slug));

  /** The newest PR whose head is `branch` (open ones first), or null. */
  const byBranch = (slug, branch) =>
    cached(`b:${lc(slug)}:${branch}`, async () => {
      const list = JSON.parse((await gh(['pr', 'list', '--repo', slug, '--head', branch, '--state', 'all', '--limit', '5', '--json', FIELDS], { timeout: 15000 })).stdout);
      const prs = (Array.isArray(list) ? list : []).map((j) => prOf(j, slug)).filter(Boolean);
      return prs.find((p) => p.state === 'open' || p.state === 'draft') ?? prs[0] ?? null;
    });

  // The repos under the roots (`fleet repos`, ~1 s): loaded at start, served stale while a
  // refresh runs, so a review never waits for it after the first time.
  let clonesAt = 0;
  let clonesList = null;
  let clonesLoading = null;
  function loadClones() {
    if (!clonesLoading)
      clonesLoading = clones()
        .then(
          (list) => {
            clonesList = Array.isArray(list) ? list : [];
            clonesAt = now();
          },
          () => {
            clonesList ??= [];
            clonesAt = now();
          },
        )
        .finally(() => (clonesLoading = null));
    return clonesLoading;
  }
  loadClones();
  async function cloneOf(slug) {
    if (!clonesList) await loadClones();
    else if (now() - clonesAt > 5 * 60_000) loadClones();
    const name = lc(slug.split('/')[1]);
    for (const c of clonesList ?? []) {
      if (lc(c?.name) !== name || typeof c.path !== 'string') continue;
      if (lc(await differ.remote(c.path)) === lc(slug)) return c.path;
    }
    return null;
  }

  /**
   * roots: the checkouts the session works in (cwd's first); prUrls: the PR links in its brief.
   * → { roots: the checkouts to diff (+ worktrees found on a brief PR's branch),
   *     info: Map(root → { remote, pr }),
   *     prs: [{ key, clone, number, baseBranch, headBranch, pr }] diffed from a clone,
   *     missing: [{ key, pr, error }] brief PRs with no clone on this host }
   */
  async function plan(roots, prUrls = []) {
    const info = new Map();
    const out = [...roots];
    await Promise.all(
      roots.map(async (root) => {
        const [remote, branch, def] = await Promise.all([differ.remote(root), differ.branch(root), differ.defaultBranch(root)]);
        info.set(root, { remote, branch, def, pr: null });
      }),
    );
    const prs = [];
    const missing = [];
    const seen = new Set();
    const urls = [...new Set(prUrls)].map(parsePrUrl).filter(Boolean).slice(0, MAX_SESSION_PRS);
    // Looked up in parallel, applied in the brief's order: the same brief gives the same entries
    // in the same order (and the same fingerprint) on every poll.
    const found = await Promise.all(
      urls.map(async (u) => {
        const pr = await byNumber(u.repo, u.number);
        if (!pr?.headBranch) return null;
        const slug = `${pr.owner}/${pr.repo}`;
        const key = lc(prRef(pr));
        // A checkout of the session already on its branch.
        for (const [root, i] of info) if (i.remote && lc(i.remote) === lc(slug) && i.branch === pr.headBranch) return { key, pr, root };
        // A worktree of the same repo on its branch (made later, e.g. "Set up worktree").
        const sameRepo = roots.find((r) => info.get(r)?.remote && lc(info.get(r).remote) === lc(slug)) ?? null;
        const clone = sameRepo ?? (await cloneOf(slug));
        if (!clone) return { key, pr, missing: `no local clone of ${slug} on this host` };
        const wt = await differ.worktreeOn(clone, pr.headBranch);
        return wt ? { key, pr, worktree: wt } : { key, pr, clone };
      }),
    );
    for (const f of found) {
      if (!f || seen.has(f.key)) continue;
      seen.add(f.key);
      const { key, pr } = f;
      if (f.root) {
        info.get(f.root).pr = pr;
      } else if (f.missing) {
        missing.push({ key: `pr:${key}`, pr, error: f.missing });
      } else if (f.worktree) {
        if (!out.includes(f.worktree)) {
          out.push(f.worktree);
          info.set(f.worktree, { remote: `${pr.owner}/${pr.repo}`, branch: pr.headBranch, def: null, pr });
        } else info.get(f.worktree).pr = pr;
      } else {
        prs.push({ key: `pr:${key}`, clone: f.clone, number: pr.number, baseBranch: pr.baseBranch, headBranch: pr.headBranch, pr });
      }
    }
    // Checkouts with no brief PR: the PR on their branch, if any (never for the default branch).
    await Promise.all(
      [...info].map(async ([, i]) => {
        if (i.pr || !i.remote || !i.branch || i.branch === i.def || i.branch === 'main' || i.branch === 'master') return;
        i.pr = (await byBranch(i.remote, i.branch)) ?? null;
      }),
    );
    return { roots: out, info, prs, missing };
  }

  return { plan, byNumber, byBranch };
}
