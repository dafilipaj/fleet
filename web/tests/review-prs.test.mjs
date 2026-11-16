// The diff reviewer's entries: a session's checkouts joined with its PRs (by repo + branch),
// brief PRs without a checkout diffed from a clone, gh lookups cached. No network: gh and git
// are fakes.
import test from 'node:test';
import assert from 'node:assert/strict';

import { githubSlug } from '../lib/diff.mjs';
import { createReviewPrs, prOf } from '../lib/review-prs.mjs';

const ghPr = (number, head, extra = {}) => ({ number, title: `PR ${number}`, url: `https://github.com/acme/api/pull/${number}`, state: 'OPEN', isDraft: false, headRefName: head, baseRefName: 'main', headRefOid: 'a'.repeat(40), ...extra });

function fakes({ checkouts, worktrees = {}, prs = [], clones = [] }) {
  const calls = [];
  const gh = async (args) => {
    calls.push(args.join(' '));
    if (args[1] === 'view') {
      const pr = prs.find((p) => String(p.number) === args[2]);
      if (!pr) throw new Error('not found');
      return { stdout: JSON.stringify(pr) };
    }
    const head = args[args.indexOf('--head') + 1];
    return { stdout: JSON.stringify(prs.filter((p) => p.headRefName === head)) };
  };
  const differ = {
    remote: async (root) => checkouts[root]?.remote ?? clones.find((c) => c.path === root)?.remote ?? null,
    branch: async (root) => checkouts[root]?.branch ?? null,
    defaultBranch: async () => 'main',
    worktreeOn: async (clone, branch) => worktrees[`${clone}:${branch}`] ?? null,
  };
  return { gh, differ, calls, clones: async () => clones.map(({ name, path }) => ({ name, path })) };
}

test('githubSlug: https, ssh and scp-like remotes; anything else is null', () => {
  assert.equal(githubSlug('git@github.com:acme/api.git'), 'acme/api');
  assert.equal(githubSlug('https://github.com/acme/api'), 'acme/api');
  assert.equal(githubSlug('ssh://git@github.com/acme/api.git'), 'acme/api');
  assert.equal(githubSlug('https://gitlab.com/acme/api'), null);
  assert.equal(githubSlug(''), null);
});

test('prOf: states as GitHub shows them', () => {
  assert.equal(prOf(ghPr(1, 'x', { isDraft: true }), 'acme/api').state, 'draft');
  assert.equal(prOf(ghPr(1, 'x', { state: 'MERGED' }), 'acme/api').state, 'merged');
  assert.equal(prOf({}, 'acme/api'), null);
});

test('plan: a checkout gets the PR on its branch; the default branch is never looked up', async () => {
  const f = fakes({
    checkouts: { '/home': { remote: 'me/home', branch: 'main' }, '/wt': { remote: 'acme/api', branch: 'fix' } },
    prs: [ghPr(7, 'fix')],
  });
  const rp = createReviewPrs({ gh: f.gh, differ: f.differ, waitMs: 1000 });
  const plan = await rp.plan(['/home', '/wt']);
  assert.deepEqual(plan.roots, ['/home', '/wt']);
  assert.equal(plan.info.get('/wt').pr.number, 7);
  assert.equal(plan.info.get('/home').pr, null);
  assert.ok(!f.calls.some((c) => c.includes('me/home')), 'no lookup for main');
  // Cached: a second plan asks gh nothing.
  const n = f.calls.length;
  await rp.plan(['/home', '/wt']);
  assert.equal(f.calls.length, n);
});

test('plan: a brief PR with no checkout → a worktree on its branch if one exists, else a diff from a clone, else missing', async () => {
  const f = fakes({
    checkouts: { '/home': { remote: 'me/home', branch: 'main' } },
    worktrees: { '/code/api:has-wt': '/wts/api-has-wt' },
    clones: [
      { name: 'api', path: '/code/api', remote: 'acme/api' },
      { name: 'api', path: '/code/other-api', remote: 'someone/api' },
    ],
    prs: [ghPr(1, 'has-wt'), ghPr(2, 'no-wt'), { ...ghPr(3, 'gone'), url: 'https://github.com/acme/web/pull/3' }],
  });
  const rp = createReviewPrs({ gh: f.gh, differ: f.differ, clones: f.clones, waitMs: 1000 });
  const plan = await rp.plan(['/home'], ['https://github.com/acme/api/pull/1', 'https://github.com/acme/api/pull/2', 'https://github.com/acme/web/pull/3']);
  assert.deepEqual(plan.roots, ['/home', '/wts/api-has-wt']);
  assert.equal(plan.info.get('/wts/api-has-wt').pr.number, 1);
  assert.deepEqual(
    plan.prs.map((p) => [p.key, p.clone, p.headBranch]),
    [['pr:acme/api#2', '/code/api', 'no-wt']],
  );
  assert.deepEqual(plan.missing.map((m) => m.key), ['pr:acme/web#3']);
});

test('plan: a brief PR whose branch a session checkout is on is that checkout (no second entry)', async () => {
  const f = fakes({ checkouts: { '/wt': { remote: 'acme/api', branch: 'fix' } }, prs: [ghPr(5, 'fix')] });
  const rp = createReviewPrs({ gh: f.gh, differ: f.differ, waitMs: 1000 });
  const plan = await rp.plan(['/wt'], ['https://github.com/acme/api/pull/5']);
  assert.deepEqual(plan.roots, ['/wt']);
  assert.equal(plan.prs.length, 0);
  assert.equal(plan.info.get('/wt').pr.number, 5);
});
