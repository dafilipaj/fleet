// The diff reviewer's source: name-status / patch parsing, and the worktree diff against a real
// git repo in a temp dir (merge-base with the default branch, untracked files, renames, the
// tree fingerprint, the real index left alone), plus the …/diff route.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { EMPTY_TREE, createDiffer, parseNameStatus, parseNumstat, parsePatch, parseRaw, validRef } from '../lib/diff.mjs';
import { run } from '../lib/run.mjs';

function repo(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-diff-')));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_CONFIG_SYSTEM: '/dev/null',
    GIT_AUTHOR_NAME: 't',
    GIT_AUTHOR_EMAIL: 't@example.com',
    GIT_COMMITTER_NAME: 't',
    GIT_COMMITTER_EMAIL: 't@example.com',
  };
  const git = (...args) => execFileSync('git', ['-C', dir, ...args], { env, encoding: 'utf8' });
  const write = (rel, text) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  };
  git('init', '-q', '-b', 'main');
  return { dir, git, write };
}

test('parseNameStatus: adds, deletes, renames with their old path', () => {
  const out = 'M\0a.txt\0A\0new file.txt\0D\0gone.txt\0R087\0old.txt\0moved.txt\0';
  assert.deepEqual(parseNameStatus(out), [
    { status: 'modified', path: 'a.txt', oldPath: 'a.txt' },
    { status: 'added', path: 'new file.txt', oldPath: null },
    { status: 'deleted', path: 'gone.txt', oldPath: 'gone.txt' },
    { status: 'renamed', path: 'moved.txt', oldPath: 'old.txt' },
  ]);
});

test('parsePatch: line numbers on both sides, binary files, no-newline markers', () => {
  const text = [
    'diff --git a/a.txt b/a.txt',
    'index 1..2 100644',
    '--- a/a.txt',
    '+++ b/a.txt',
    '@@ -1,3 +1,3 @@ function x() {',
    ' one',
    '-two',
    '+TWO',
    ' three',
    '\\ No newline at end of file',
    'diff --git a/img.png b/img.png',
    'Binary files a/img.png and b/img.png differ',
    '',
  ].join('\n');
  const [a, img] = parsePatch(text);
  assert.equal(a.hunks.length, 1);
  assert.equal(a.hunks[0].section, 'function x() {');
  assert.deepEqual(
    a.hunks[0].lines.map((l) => [l.t, l.o, l.n, l.text]),
    [[' ', 1, 1, 'one'], ['-', 2, null, 'two'], ['+', null, 2, 'TWO'], [' ', 3, 3, 'three']],
  );
  assert.equal(a.hunks[0].lines[3].noEol, true);
  assert.equal(img.binary, true);
  assert.deepEqual(img.hunks, []);
});

test('validRef refuses options and ranges', () => {
  assert.ok(validRef('origin/main'));
  assert.ok(validRef('HEAD~2'));
  assert.ok(!validRef('--output=/tmp/x'));
  assert.ok(!validRef('a..b'));
  assert.ok(!validRef('a b'));
});

test('diff: branch commits + uncommitted + untracked against the merge-base; fingerprint; real index untouched', async (t) => {
  const r = repo(t);
  r.write('a.txt', 'one\ntwo\nthree\n');
  r.write('old.txt', 'a\nb\nc\nd\ne\nf\n');
  r.write('.gitignore', 'ignored/\n');
  r.git('add', '-A');
  r.git('commit', '-qm', 'init');
  r.git('checkout', '-qb', 'feature');
  r.write('a.txt', 'one\nTWO\nthree\n');
  r.git('commit', '-qam', 'on branch');
  r.git('mv', 'old.txt', 'moved.txt'); // staged rename
  r.write('new.txt', 'hello\n'); // untracked
  r.write('ignored/x.txt', 'nope\n');
  const indexBefore = fs.readFileSync(path.join(r.dir, '.git/index'));
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-diff-idx-'));
  t.after(() => fs.rmSync(tmpDir, { recursive: true, force: true }));
  const differ = createDiffer({ run, tmpDir });

  const d = await differ.diff(path.join(r.dir));
  assert.equal(d.root, r.dir);
  assert.equal(d.branch, 'feature');
  assert.equal(d.base.ref, 'main');
  assert.equal(d.base.sha, r.git('rev-parse', 'main').trim());
  assert.equal(d.unchanged, false);
  const by = Object.fromEntries(d.files.map((f) => [f.path, f]));
  assert.deepEqual(Object.keys(by).sort(), ['a.txt', 'moved.txt', 'new.txt']);
  assert.equal(by['a.txt'].status, 'modified');
  assert.equal(by['a.txt'].additions, 1);
  assert.equal(by['a.txt'].deletions, 1);
  assert.equal(by['moved.txt'].status, 'renamed');
  assert.equal(by['moved.txt'].oldPath, 'old.txt');
  assert.equal(by['new.txt'].status, 'added');
  assert.deepEqual(by['new.txt'].hunks[0].lines, [{ t: '+', o: null, n: 1, text: 'hello' }]);
  assert.deepEqual(d.stats, { files: 3, additions: 2, deletions: 1 });
  assert.deepEqual(fs.readFileSync(path.join(r.dir, '.git/index')), indexBefore, 'the real index is left alone');
  assert.deepEqual(fs.readdirSync(tmpDir), [], 'the throwaway index is removed');

  // Nothing moved: the poller gets a short answer.
  const same = await differ.diff(r.dir, { since: d.fingerprint });
  assert.deepEqual([same.unchanged, same.files], [true, undefined]);
  // An edit changes the fingerprint.
  r.write('new.txt', 'hello\nworld\n');
  const next = await differ.diff(r.dir, { since: d.fingerprint });
  assert.equal(next.unchanged, false);
  assert.notEqual(next.fingerprint, d.fingerprint);

  // On the base branch itself: only what is not committed.
  r.git('add', '-A');
  r.git('commit', '-qm', 'more');
  r.git('checkout', '-q', 'main');
  r.write('a.txt', 'one\ntwo\nthree\nfour\n');
  const onMain = await differ.diff(r.dir);
  assert.deepEqual(onMain.files.map((f) => [f.path, f.additions, f.deletions]), [['a.txt', 1, 0]]);
  // An explicit base.
  const vsFeature = await differ.diff(r.dir, { base: 'feature' });
  assert.equal(vsFeature.base.ref, 'feature');
  await assert.rejects(differ.diff(r.dir, { base: '--output=x' }), { status: 400 });
});

test('diff: a repo without commits diffs against the empty tree; outside git is a 409', async (t) => {
  const r = repo(t);
  r.write('x.txt', 'x\n');
  const differ = createDiffer({ run });
  const d = await differ.diff(r.dir);
  assert.equal(d.base.sha, EMPTY_TREE);
  assert.equal(d.head, null);
  assert.deepEqual(d.files.map((f) => f.path), ['x.txt']);
  const plain = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-nogit-')));
  t.after(() => fs.rmSync(plain, { recursive: true, force: true }));
  await assert.rejects(differ.diff(plain), { status: 409 });
});

test('HTTP: …/diff for a session, 404 unknown session, 405 non-GET, 501 without a differ', async (t) => {
  const r = repo(t);
  r.write('x.txt', 'x\n');
  const id = 'aaaaaaaa-0000-0000-0000-000000000001';
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [{ session_id: id, cwd: r.dir, status: 'idle', backend: 'tmux' }] }) };
  const handles = {};
  for (const [k, differ] of [['on', createDiffer({ run })], ['off', null]]) {
    handles[k] = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, differ, warmFleet: false });
    t.after(() => handles[k].stop());
  }
  const urls = {};
  for (const k of ['on', 'off']) {
    const server = createHttpServer({ handleApi: handles[k], uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    urls[k] = `http://127.0.0.1:${server.address().port}/api/hosts/laptop/sessions`;
    t.after(() => server.close());
  }
  const res = await fetch(`${urls.on}/aaaaaaaa/diff`);
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.equal(body.host, 'laptop');
  assert.equal(body.id, id);
  assert.deepEqual(body.files.map((f) => f.path), ['x.txt']);
  const again = await (await fetch(`${urls.on}/aaaaaaaa/diff?since=${body.fingerprint}`)).json();
  assert.equal(again.unchanged, true);
  assert.equal((await fetch(`${urls.on}/zzzzzzzz/diff`)).status, 404);
  assert.equal((await fetch(`${urls.on}/aaaaaaaa/diff?base=--x`)).status, 400);
  assert.equal((await fetch(`${urls.on}/aaaaaaaa/diff`, { method: 'POST' })).status, 405);
  assert.equal((await fetch(`${urls.off}/aaaaaaaa/diff`)).status, 501);
});

test('roots + diffAll: a meta repo session sees every worktree it edited; one fingerprint for all', async (t) => {
  const meta = repo(t);
  meta.write('.gitignore', 'worktrees/\n');
  meta.write('README.md', 'meta\n');
  meta.git('add', '-A');
  meta.git('commit', '-qm', 'meta');
  // Two checkouts under the ignored worktrees/ (the session edits them without cd'ing in).
  const sub = (name) => {
    const dir = path.join(meta.dir, 'worktrees', name);
    fs.mkdirSync(dir, { recursive: true });
    const g = (...args) => execFileSync('git', ['-C', dir, ...args], { env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com' }, encoding: 'utf8' });
    g('init', '-q', '-b', 'main');
    fs.writeFileSync(path.join(dir, 'a.txt'), 'one\n');
    g('add', '-A');
    g('commit', '-qm', 'init');
    return dir;
  };
  const api = sub('api-fix');
  const ui = sub('ui-feature');
  fs.writeFileSync(path.join(api, 'a.txt'), 'one\ntwo\n');
  fs.writeFileSync(path.join(ui, 'b.txt'), 'new\n');
  const differ = createDiffer({ run, home: '/nonexistent-home' });

  const worked = [path.join(ui, 'b.txt'), path.join(api, 'a.txt'), path.join(api, 'a.txt'), '/etc/hosts', path.join(meta.dir, 'README.md')];
  const roots = await differ.roots(meta.dir, worked);
  assert.deepEqual(roots, [meta.dir, ui, api], 'cwd first, then by recency, deduped, non-git paths skipped');

  const all = await differ.diffAll(roots);
  assert.equal(all.unchanged, false);
  assert.deepEqual(
    all.repos.map((r) => [r.root, r.files.map((f) => f.path)]),
    [
      [meta.dir, []],
      [ui, ['b.txt']],
      [api, ['a.txt']],
    ],
  );
  assert.deepEqual(await differ.diffAll(roots, { since: all.fingerprint }), { fingerprint: all.fingerprint, unchanged: true });
  fs.writeFileSync(path.join(ui, 'b.txt'), 'changed\n');
  assert.notEqual((await differ.diffAll(roots, { since: all.fingerprint })).fingerprint, all.fingerprint);

  // A worktree that went away is reported, not fatal.
  const gone = await differ.diffAll([...roots, path.join(meta.dir, 'worktrees', 'missing')]);
  assert.match(gone.repos.at(-1).error, /./);
});

test('HTTP: …/diffs — the cwd checkout plus those of the files the session worked on, labelled from the cwd', async (t) => {
  const meta = repo(t);
  meta.write('.gitignore', 'wt/\n');
  meta.git('add', '-A');
  meta.git('commit', '-qm', 'meta');
  const wt = path.join(meta.dir, 'wt', 'one');
  fs.mkdirSync(wt, { recursive: true });
  execFileSync('git', ['-C', wt, 'init', '-q', '-b', 'main']);
  fs.writeFileSync(path.join(wt, 'x.txt'), 'x\n');
  const id = 'aaaaaaaa-0000-0000-0000-000000000001';
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [{ session_id: id, cwd: meta.dir, status: 'idle', backend: 'tmux' }] }) };
  const touched = { get: async () => ({ version: '1', paths: [], worked: [path.join(wt, 'x.txt')] }) };
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, differ: createDiffer({ run, home: '/nonexistent-home' }), touched, warmFleet: false });
  t.after(() => api.stop());
  const server = createHttpServer({ handleApi: api, uiDir: null });
  await new Promise((res) => server.listen(0, '127.0.0.1', res));
  t.after(() => server.close());
  const base = `http://127.0.0.1:${server.address().port}/api/hosts/laptop/sessions/aaaaaaaa/diffs`;
  const body = await (await fetch(base)).json();
  assert.equal(body.cwd, meta.dir);
  assert.deepEqual(body.repos.map((r) => [r.label, r.files.map((f) => f.path)]), [[meta.dir.replace(os.homedir(), '~'), []], ['wt/one', ['x.txt']]]);
  assert.equal((await (await fetch(`${base}?since=${body.fingerprint}`)).json()).unchanged, true);
});

test('diff: a .plain spec carries both whole texts; other files do not', async (t) => {
  const r = repo(t);
  r.write('spec/a.plain', '***functional specs***\n\n- one\n');
  r.write('b.txt', 'x\n');
  r.git('add', '-A');
  r.git('commit', '-qm', 'init');
  r.write('spec/a.plain', '***functional specs***\n\n- one\n- two\n');
  r.write('spec/new.plain', '- fresh\n');
  r.write('b.txt', 'y\n');
  const d = await createDiffer({ run }).diff(r.dir);
  const by = Object.fromEntries(d.files.map((f) => [f.path, f]));
  assert.deepEqual(by['spec/a.plain'].plain, { old: '***functional specs***\n\n- one\n', new: '***functional specs***\n\n- one\n- two\n' });
  assert.deepEqual(by['spec/new.plain'].plain, { old: null, new: '- fresh\n' });
  assert.equal(by['b.txt'].plain, undefined);
});

test('prDiff: a PR from a clone, no checkout — refs fetched privately, diffed from the merge-base', async (t) => {
  const origin = repo(t);
  origin.write('a.txt', 'one\ntwo\n');
  origin.write('docs/x.plain', '***definitions***\n\n- A thing.\n');
  origin.git('add', '-A');
  origin.git('commit', '-qm', 'init');
  origin.git('checkout', '-qb', 'feature');
  origin.write('a.txt', 'one\nTWO\n');
  origin.write('docs/x.plain', '***definitions***\n\n- A better thing.\n');
  origin.git('commit', '-qam', 'change');
  origin.git('update-ref', 'refs/pull/7/head', 'feature');
  origin.git('checkout', '-q', 'main');
  // main moves on after the branch: not part of the PR.
  origin.write('later.txt', 'x\n');
  origin.git('add', '-A');
  origin.git('commit', '-qm', 'later');

  const clone = repo(t);
  clone.git('remote', 'add', 'origin', origin.dir);
  clone.git('fetch', '-q', 'origin');
  clone.git('checkout', '-q', '-b', 'main', 'origin/main');
  clone.write('local.txt', 'my own uncommitted work\n');
  const before = clone.git('status', '--porcelain');

  const differ = createDiffer({ run, home: '/nonexistent' });
  const d = await differ.prDiff(clone.dir, { number: 7, baseBranch: 'main', headBranch: 'feature' });
  assert.equal(d.unchanged, false);
  assert.deepEqual(d.files.map((f) => f.path).sort(), ['a.txt', 'docs/x.plain']);
  assert.equal(d.head, origin.git('rev-parse', 'feature').trim());
  assert.equal(d.base.sha, origin.git('rev-parse', 'main~1').trim());
  assert.equal(d.branch, 'feature');
  assert.match(d.files.find((f) => f.path === 'docs/x.plain').plain.new, /better/);
  // The clone's checkout, index and branches are untouched; only refs/fleet/pr/… are new.
  assert.equal(clone.git('status', '--porcelain'), before);
  assert.equal(clone.git('rev-parse', '--abbrev-ref', 'HEAD').trim(), 'main');
  assert.deepEqual(clone.git('for-each-ref', '--format=%(refname)', 'refs/fleet').trim().split('\n').sort(), ['refs/fleet/pr/7/base', 'refs/fleet/pr/7/head']);
  // Unchanged since → no files; a new commit on the PR shows after a refetch.
  assert.equal((await differ.prDiff(clone.dir, { number: 7, baseBranch: 'main', since: d.fingerprint })).unchanged, true);
  origin.git('checkout', '-q', 'feature');
  origin.write('b.txt', 'new\n');
  origin.git('add', '-A');
  origin.git('commit', '-qm', 'more');
  origin.git('update-ref', 'refs/pull/7/head', 'feature');
  assert.equal((await differ.prDiff(clone.dir, { number: 7, baseBranch: 'main', since: d.fingerprint })).unchanged, true, 'no refetch within the minute');
  const again = await differ.prDiff(clone.dir, { number: 7, baseBranch: 'main', since: d.fingerprint, refetch: true });
  assert.equal(again.unchanged, false);
  assert.ok(again.files.some((f) => f.path === 'b.txt'));
  await assert.rejects(differ.prDiff(clone.dir, { number: 8, baseBranch: 'main' }), { status: 502 });
  await assert.rejects(differ.prDiff(clone.dir, { number: 7, baseBranch: '--upload-pack=x' }), { status: 400 });
});

test('HTTP: …/diffs?root= — just that checkout; a path that is not a checkout root is a 400', async (t) => {
  const meta = repo(t);
  meta.write('.gitignore', 'wt/\n');
  meta.write('m.txt', 'm\n');
  meta.git('add', '-A');
  meta.git('commit', '-qm', 'meta');
  meta.write('m.txt', 'changed\n');
  const wt = path.join(meta.dir, 'wt', 'one');
  fs.mkdirSync(wt, { recursive: true });
  execFileSync('git', ['-C', wt, 'init', '-q', '-b', 'main']);
  fs.writeFileSync(path.join(wt, 'x.txt'), 'x\n');
  const id = 'aaaaaaaa-0000-0000-0000-000000000002';
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [{ session_id: id, cwd: meta.dir, status: 'idle', backend: 'tmux' }] }) };
  const touched = { get: async () => ({ version: '1', paths: [], worked: [path.join(wt, 'x.txt')] }) };
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, differ: createDiffer({ run, home: '/nonexistent-home' }), touched, warmFleet: false });
  t.after(() => api.stop());
  const server = createHttpServer({ handleApi: api, uiDir: null });
  await new Promise((res) => server.listen(0, '127.0.0.1', res));
  t.after(() => server.close());
  const base = `http://127.0.0.1:${server.address().port}/api/hosts/laptop/sessions/aaaaaaaa/diffs`;
  const one = await (await fetch(`${base}?root=${encodeURIComponent(wt)}`)).json();
  assert.deepEqual(one.repos.map((r) => [r.root, r.files.map((f) => f.path)]), [[wt, ['x.txt']]]);
  const main = await (await fetch(`${base}?root=${encodeURIComponent(meta.dir)}`)).json();
  assert.deepEqual(main.repos.map((r) => [r.root, r.files.map((f) => f.path)]), [[meta.dir, ['m.txt']]]);
  assert.equal((await fetch(`${base}?root=${encodeURIComponent(path.join(wt, 'sub'))}`)).status, 400);
  assert.equal((await fetch(`${base}?root=relative/path`)).status, 400);
  assert.equal((await fetch(`${base}?root=${encodeURIComponent('/')}`)).status, 400);
});

test('diffAll with prs: a checkout carries what is local only; a PR without a worktree diffs from the clone under its own key', async (t) => {
  const origin = repo(t);
  origin.write('a.txt', 'one\n');
  origin.git('add', '-A');
  origin.git('commit', '-qm', 'init');
  origin.git('checkout', '-qb', 'feature');
  origin.write('a.txt', 'ONE\n');
  origin.git('commit', '-qam', 'change');
  origin.git('update-ref', 'refs/pull/3/head', 'feature');
  origin.git('checkout', '-q', 'main');

  const clone = repo(t);
  clone.git('remote', 'add', 'origin', origin.dir);
  clone.git('fetch', '-q', 'origin');
  clone.git('checkout', '-q', '-b', 'work', 'origin/main');
  clone.git('branch', '-q', '--set-upstream-to', 'origin/main');
  clone.write('b.txt', 'b\n');
  clone.git('add', '-A');
  clone.git('commit', '-qm', 'local commit');
  clone.write('c.txt', 'uncommitted\n');

  const differ = createDiffer({ run, home: '/nonexistent' });
  const prs = [{ key: 'pr:acme/api#3', clone: clone.dir, number: 3, baseBranch: 'main', headBranch: 'feature' }];
  // The first answer does not wait for the network: the PR is pending until its fetch lands.
  const first = await differ.diffAll([clone.dir], { prs, extra: 'x' });
  assert.deepEqual(first.repos.map((r) => [r.key ?? null, r.pending ?? false]), [[null, false], ['pr:acme/api#3', true]]);
  let all = first;
  for (let i = 0; i < 100 && all.repos[1].pending; i++) {
    await new Promise((r) => setTimeout(r, 30));
    all = await differ.diffAll([clone.dir], { prs, extra: 'x' });
  }
  assert.notEqual(all.fingerprint, first.fingerprint);
  assert.equal(all.repos.length, 2);
  const [wt, pr] = all.repos;
  assert.deepEqual(wt.local, { uncommitted: 1, unpushed: 1 });
  assert.equal(pr.source, 'pr');
  assert.equal(pr.key, 'pr:acme/api#3');
  assert.equal(pr.local, undefined);
  assert.deepEqual(pr.files.map((f) => f.path), ['a.txt']);
  // The fingerprint covers the annotations: same diff, other `extra` → not unchanged.
  assert.equal((await differ.diffAll([clone.dir], { prs, extra: 'x', since: all.fingerprint })).unchanged, true);
  assert.equal((await differ.diffAll([clone.dir], { prs, extra: 'y', since: all.fingerprint })).unchanged, false);
  assert.equal(await differ.branch(clone.dir), 'work');
  assert.equal(await differ.worktreeOn(clone.dir, 'work'), clone.dir);
  assert.equal(await differ.worktreeOn(clone.dir, 'nope'), null);
});

test('parseRaw / parseNumstat: blob ids, renames by their new path, binary files', () => {
  const a = 'a'.repeat(40);
  const b = 'b'.repeat(40);
  const z = '0'.repeat(40);
  const raw = `:100644 100644 ${a} ${b} M\0x.txt\0:000000 100644 ${z} ${b} A\0new.txt\0:100644 100644 ${a} ${b} R090\0old.txt\0moved.txt\0`;
  assert.deepEqual(parseRaw(raw), [
    { status: 'modified', path: 'x.txt', oldPath: 'x.txt', sig: `${a}:${b}` },
    { status: 'added', path: 'new.txt', oldPath: null, sig: `${z}:${b}` },
    { status: 'renamed', path: 'moved.txt', oldPath: 'old.txt', sig: `${a}:${b}` },
  ]);
  const num = parseNumstat('3\t1\tx.txt\0-\t-\timg.png\0' + '2\t0\t\0old.txt\0moved.txt\0');
  assert.deepEqual(num.get('x.txt'), { additions: 3, deletions: 1, binary: false });
  assert.deepEqual(num.get('img.png'), { additions: 0, deletions: 0, binary: true });
  assert.deepEqual(num.get('moved.txt'), { additions: 2, deletions: 0, binary: false });
});

test('lite: the list without lines, the same files and counts as the full diff; filesAt fills them in', async (t) => {
  const r = repo(t);
  r.write('a.txt', 'one\ntwo\n');
  r.write('old.txt', 'stays the same\nacross the rename\nmostly\n');
  r.write('spec.plain', '***definitions***\n\n- A thing.\n');
  r.git('add', '-A');
  r.git('commit', '-qm', 'init');
  r.write('a.txt', 'one\nTWO\nthree\n');
  r.git('mv', 'old.txt', 'new.txt');
  r.write('spec.plain', '***definitions***\n\n- A better thing.\n');
  r.write('untracked.txt', 'u\n');
  const differ = createDiffer({ run, home: '/nonexistent' });
  const full = await differ.diffAll([r.dir]);
  const lite = await differ.diffAll([r.dir], { lite: true });
  assert.equal(lite.fingerprint, full.fingerprint);
  const [lr] = lite.repos;
  const [fr] = full.repos;
  assert.deepEqual(lr.stats, fr.stats);
  assert.deepEqual(
    lr.files.map((f) => [f.path, f.oldPath, f.status, f.additions, f.deletions, f.lazy, f.hunks.length, f.plain]),
    fr.files.map((f) => [f.path, f.oldPath, f.status, f.additions, f.deletions, true, 0, undefined]),
  );
  assert.ok(lr.files.every((f) => /^[0-9a-f]{40}:[0-9a-f]{40}$/.test(f.sig)));
  const some = await differ.filesAt(r.dir, { base: lr.base.sha, tree: lr.tree, paths: ['new.txt', 'old.txt', 'spec.plain'] });
  assert.deepEqual(some.files.map((f) => [f.path, f.status]), [['new.txt', 'renamed'], ['spec.plain', 'modified']]);
  assert.match(some.files[1].plain.new, /better/);
  assert.deepEqual(some.files[1].hunks, fr.files.find((f) => f.path === 'spec.plain').hunks);
  // A path is literal (no globs), the ids must be ids, and the count is capped.
  assert.deepEqual((await differ.filesAt(r.dir, { base: lr.base.sha, tree: lr.tree, paths: ['*.txt'] })).files, []);
  await assert.rejects(differ.filesAt(r.dir, { base: 'HEAD', tree: lr.tree, paths: ['a.txt'] }), { status: 400 });
  await assert.rejects(differ.filesAt(r.dir, { base: lr.base.sha, tree: lr.tree, paths: Array.from({ length: 41 }, (_, i) => `f${i}`) }), { status: 400 });
});

test('filesAt full: the whole file as one hunk, its changes in place', async (t) => {
  const r = repo(t);
  const lines = Array.from({ length: 40 }, (_, i) => `line ${i + 1}`);
  r.write('a.txt', lines.join('\n') + '\n');
  r.git('add', '-A');
  r.git('commit', '-qm', 'init');
  lines[4] = 'FIVE';
  lines[34] = 'THIRTY-FIVE';
  r.write('a.txt', lines.join('\n') + '\n');
  const differ = createDiffer({ run, home: '/nonexistent' });
  const [lr] = (await differ.diffAll([r.dir], { lite: true })).repos;
  const at = { base: lr.base.sha, tree: lr.tree, paths: ['a.txt'] };
  const plain = (await differ.filesAt(r.dir, at)).files[0];
  assert.equal(plain.hunks.length, 2);
  const [f] = (await differ.filesAt(r.dir, { ...at, full: true })).files;
  assert.equal(f.hunks.length, 1);
  const ls = f.hunks[0].lines;
  assert.equal(ls.filter((l) => l.t !== '-').length, 40);
  assert.deepEqual(ls.filter((l) => l.t !== ' ').map((l) => [l.t, l.text]), [['-', 'line 5'], ['+', 'FIVE'], ['-', 'line 35'], ['+', 'THIRTY-FIVE']]);
  assert.deepEqual([f.additions, f.deletions], [2, 2]);
});

test('HTTP: …/diffs?lite=1 and …/diffs/files — the list, then some files by base + tree', async (t) => {
  const r = repo(t);
  r.write('a.txt', 'one\n');
  r.git('add', '-A');
  r.git('commit', '-qm', 'init');
  r.write('a.txt', 'ONE\n');
  const id = 'aaaaaaaa-0000-0000-0000-000000000003';
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [{ session_id: id, cwd: r.dir, status: 'idle', backend: 'tmux' }] }) };
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, differ: createDiffer({ run, home: '/nonexistent-home' }), touched: { get: async () => ({ worked: [] }) }, warmFleet: false });
  t.after(() => api.stop());
  const server = createHttpServer({ handleApi: api, uiDir: null });
  await new Promise((res) => server.listen(0, '127.0.0.1', res));
  t.after(() => server.close());
  const base = `http://127.0.0.1:${server.address().port}/api/hosts/laptop/sessions/aaaaaaaa`;
  const lite = await (await fetch(`${base}/diffs?lite=1`)).json();
  const [e] = lite.repos;
  assert.deepEqual(e.files.map((f) => [f.path, f.lazy, f.hunks.length]), [['a.txt', true, 0]]);
  const q = `root=${encodeURIComponent(e.root)}&base=${e.base.sha}&tree=${e.tree}`;
  const got = await (await fetch(`${base}/diffs/files?${q}&path=a.txt`)).json();
  assert.deepEqual(got.files.map((f) => [f.path, f.hunks[0].lines.map((l) => l.t + l.text)]), [['a.txt', ['-one', '+ONE']]]);
  assert.equal((await fetch(`${base}/diffs/files?${q}`)).status, 400, 'no path');
  assert.equal((await fetch(`${base}/diffs/files?root=%2F&base=${e.base.sha}&tree=${e.tree}&path=a.txt`)).status, 400, 'not a checkout');
});
