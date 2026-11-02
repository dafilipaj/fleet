// Files mentioned in chat: resolution, the $HOME / cwd sandbox, kinds, size caps, the
// stat/raw/open endpoints (local + one peer hop, raw streamed) and the open command.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { PREVIEW_MAX_BYTES, createFiles, kindByName, looksLikeText, parsePathSpec, resolveUserPath } from '../lib/files.mjs';

const PNG = Buffer.from('89504e470d0a1a0a0000000d49484452', 'hex');

/** A fake $HOME with a project, a secret store, a symlink out and a cwd outside home. */
function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-files-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, 'home');
  const proj = path.join(home, 'proj');
  const outside = path.join(root, 'outside');
  const cwdOut = path.join(root, 'scratch');
  for (const d of [proj, path.join(proj, 'docs'), path.join(home, '.ssh'), outside, cwdOut]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(proj, 'README.md'), '# Title\n\nSee [a](docs/a.md).\n');
  fs.writeFileSync(path.join(proj, 'docs', 'a.md'), 'line 1\nline 2\n');
  fs.writeFileSync(path.join(proj, 'main.rs'), 'fn main() {}\n');
  fs.writeFileSync(path.join(proj, 'Makefile'), 'all:\n\techo hi\n');
  fs.writeFileSync(path.join(proj, 'shot.png'), PNG);
  fs.writeFileSync(path.join(proj, 'doc.pdf'), '%PDF-1.4\n');
  fs.writeFileSync(path.join(proj, 'blob.dat'), Buffer.from([1, 2, 0, 3]));
  fs.writeFileSync(path.join(proj, 'big.md'), Buffer.alloc(PREVIEW_MAX_BYTES + 1, 0x61));
  fs.writeFileSync(path.join(proj, '.env'), 'TOKEN=x\n');
  fs.writeFileSync(path.join(proj, 'run.sh'), '#!/bin/sh\necho hi\n', { mode: 0o755 });
  fs.writeFileSync(path.join(home, '.ssh', 'id_ed25519'), 'secret');
  fs.writeFileSync(path.join(outside, 'secret.txt'), 'outside');
  fs.writeFileSync(path.join(cwdOut, 'notes.txt'), 'scratch notes');
  fs.symlinkSync(outside, path.join(home, 'link-out'));
  fs.symlinkSync(path.join(proj, 'docs'), path.join(home, 'link-in'));
  return { root, home, proj, outside, cwdOut };
}

test('parsePathSpec: :line, :line:col, #L12, plain', () => {
  assert.deepEqual(parsePathSpec('a/b.md:12'), { path: 'a/b.md', line: 12 });
  assert.deepEqual(parsePathSpec('a/b.md:12:3'), { path: 'a/b.md', line: 12, col: 3 });
  assert.deepEqual(parsePathSpec('src/x.ts#L40-L52'), { path: 'src/x.ts', line: 40 });
  assert.deepEqual(parsePathSpec(' ~/n.md '), { path: '~/n.md' });
  assert.deepEqual(parsePathSpec(':12'), { path: ':12' });
});

test('resolveUserPath: relative to cwd, ~, absolute, junk', () => {
  const opts = { cwd: '/h/proj', home: '/h' };
  assert.equal(resolveUserPath('docs/a.md', opts), '/h/proj/docs/a.md');
  assert.equal(resolveUserPath('./x/../y.md', opts), '/h/proj/y.md');
  assert.equal(resolveUserPath('~/n.md', opts), '/h/n.md');
  assert.equal(resolveUserPath('~', opts), '/h');
  assert.equal(resolveUserPath('/etc/hosts', opts), '/etc/hosts');
  assert.equal(resolveUserPath('~root/x', opts), null);
  assert.equal(resolveUserPath('a\0b', opts), null);
  assert.equal(resolveUserPath('x.md', { cwd: null, home: '/h' }), '/h/x.md');
});

test('kinds by name + text sniff', () => {
  assert.equal(kindByName('a/B.MD'), 'markdown');
  assert.equal(kindByName('x.svg'), 'image');
  assert.equal(kindByName('x.pdf'), 'pdf');
  assert.equal(kindByName('x.zip'), 'other');
  assert.equal(kindByName('x.ts'), null);
  assert.equal(looksLikeText(Buffer.from('héllo')), true);
  assert.equal(looksLikeText(Buffer.from([0x68, 0, 0x69])), false);
  assert.equal(looksLikeText(Buffer.from('ab€').subarray(0, 4)), true, 'cut mid-character');
  assert.equal(looksLikeText(Buffer.from([0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa])), false);
});

test('stat: exists / kinds / rel / line, sandbox and secrets', async (t) => {
  const fx = fixture(t);
  const files = createFiles({ home: fx.home, run: async () => ({}) });
  const r = await files.stat(
    ['README.md', 'docs/a.md:2', 'main.rs', 'Makefile', 'shot.png', 'doc.pdf', 'blob.dat', 'missing.md', '~/proj/main.rs', path.join(fx.proj, 'docs'), '/etc/hosts', '../../outside/secret.txt', '~/link-out/secret.txt', '~/link-in/a.md', '~/.ssh/id_ed25519', '.env', 'README.md'],
    fx.proj,
  );
  const by = Object.fromEntries(r.files.map((f) => [f.input, f]));
  assert.equal(r.files.length, 16, 'deduplicated');
  assert.equal(r.home, fx.home);
  assert.equal(by['README.md'].kind, 'markdown');
  assert.equal(by['README.md'].rel, 'README.md');
  assert.equal(by['README.md'].path, path.join(fx.proj, 'README.md'));
  assert.equal(by['README.md'].isFile, true);
  assert.ok(by['README.md'].size > 0 && by['README.md'].mtime > 0);
  assert.equal(by['docs/a.md:2'].line, 2);
  assert.equal(by['docs/a.md:2'].path, path.join(fx.proj, 'docs', 'a.md'));
  assert.equal(by['main.rs'].kind, 'text');
  assert.equal(by['Makefile'].kind, 'text');
  assert.equal(by['shot.png'].kind, 'image');
  assert.equal(by['doc.pdf'].kind, 'pdf');
  assert.equal(by['blob.dat'].kind, 'other');
  assert.equal(by['missing.md'].exists, false);
  assert.equal(by['missing.md'].forbidden, undefined);
  assert.equal(by['~/proj/main.rs'].rel, 'main.rs');
  assert.equal(by[path.join(fx.proj, 'docs')].isDir, true);
  assert.equal(by[path.join(fx.proj, 'docs')].isFile, false);
  for (const k of ['/etc/hosts', '../../outside/secret.txt', '~/link-out/secret.txt', '~/.ssh/id_ed25519', '.env']) {
    assert.equal(by[k].exists, false, k);
    assert.equal(by[k].forbidden, true, k);
    assert.equal(by[k].size, undefined, k);
  }
  assert.equal(by['~/link-in/a.md'].exists, true, 'a symlink that stays inside home is fine');

  // The session cwd is allowed even outside home; its parent is not.
  const out = await files.stat(['notes.txt', '../outside/secret.txt'], fx.cwdOut);
  assert.equal(out.files[0].exists, true);
  assert.equal(out.files[0].rel, 'notes.txt');
  assert.equal(out.files[1].forbidden, true);

  await assert.rejects(files.stat('x', fx.proj), /array/);
  await assert.rejects(files.stat(Array.from({ length: 201 }, (_, i) => `f${i}`), fx.proj), /too many/);
});

test('stat results are cached briefly', async (t) => {
  const fx = fixture(t);
  let clock = 1000;
  const files = createFiles({ home: fx.home, run: async () => ({}), now: () => clock });
  assert.equal((await files.stat(['later.md'], fx.proj)).files[0].exists, false);
  fs.writeFileSync(path.join(fx.proj, 'later.md'), 'x');
  assert.equal((await files.stat(['later.md'], fx.proj)).files[0].exists, false, 'cached');
  clock += 6000;
  assert.equal((await files.stat(['later.md'], fx.proj)).files[0].exists, true, 'expired');
});

test('open: execFile args, reveal for runnable files, 501 without an opener, 403 outside', async (t) => {
  const fx = fixture(t);
  const calls = [];
  const run = async (bin, args, opts) => (calls.push([bin, args, opts?.timeout]), { stdout: '', stderr: '' });
  const mac = createFiles({ home: fx.home, run, platform: 'darwin' });
  const r = await mac.open('README.md', fx.proj);
  assert.deepEqual(r, { ok: true, path: path.join(fx.proj, 'README.md'), revealed: false, command: 'open' });
  assert.deepEqual(calls.at(-1), ['open', [path.join(fx.proj, 'README.md')], 10000]);
  assert.equal((await mac.open('run.sh', fx.proj)).revealed, true);
  assert.deepEqual(calls.at(-1), ['open', ['-R', path.join(fx.proj, 'run.sh')], 10000]);

  const linux = createFiles({ home: fx.home, run, platform: 'linux', hasBinary: (n) => n === 'xdg-open' });
  await linux.open('docs/a.md:2', fx.proj);
  assert.deepEqual(calls.at(-1), ['xdg-open', [path.join(fx.proj, 'docs', 'a.md')], 10000]);
  await linux.open('run.sh', fx.proj);
  assert.deepEqual(calls.at(-1), ['xdg-open', [fx.proj], 10000], 'runnable → its folder');

  const none = createFiles({ home: fx.home, run, platform: 'linux', hasBinary: () => false });
  await assert.rejects(none.open('README.md', fx.proj), (e) => e.status === 501);
  const n = calls.length;
  await assert.rejects(mac.open('/etc/hosts', fx.proj), (e) => e.status === 403);
  await assert.rejects(mac.open('~/link-out/secret.txt', fx.proj), (e) => e.status === 403);
  await assert.rejects(mac.open('nope.md', fx.proj), (e) => e.status === 404);
  assert.equal(calls.length, n, 'nothing ran');
  const failing = createFiles({ home: fx.home, run: async () => { throw new Error('boom'); }, platform: 'darwin' });
  await assert.rejects(failing.open('README.md', fx.proj), (e) => e.status === 502);
});

// --- over HTTP: laptop proxies to workstation ------------------------------------------

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

async function startPair(t, fx) {
  const urls = {};
  const handles = {};
  const opened = [];
  for (const name of ['laptop', 'workstation']) {
    const server = createHttpServer({ handleApi: (req, url) => handles[name](req, url), uiDir: null });
    urls[name] = await listen(server);
    t.after(() => server.close());
  }
  const sessions = {
    laptop: [{ session_id: 'aaaaaaaa-0000-0000-0000-000000000001', name: 'one', cwd: fx.proj, status: 'idle', backend: 'tmux' }],
    workstation: [{ session_id: 'bbbbbbbb-0000-0000-0000-000000000002', name: 'two', cwd: fx.proj, status: 'idle', backend: 'tmux' }],
  };
  for (const name of ['laptop', 'workstation']) {
    const config = normalizeConfig(
      { self: name, hosts: { laptop: { web: urls.laptop }, workstation: { web: urls.workstation } } },
      { env: {}, home: '/home/tester' },
    );
    const fleet = { localHost: async () => ({ name, ok: true, sessions: sessions[name] }), invalidate() {} };
    const files = createFiles({ home: fx.home, platform: 'darwin', run: async (bin, args) => (opened.push([name, bin, args]), { stdout: '', stderr: '' }) });
    const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, files, warmFleet: false });
    t.after(() => api.stop());
    handles[name] = api;
  }
  return { urls, opened };
}

const post = async (url, body) => {
  const res = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  return { status: res.status, body: await res.json() };
};

test('HTTP: stat / raw / open, locally and through a peer (raw streamed)', async (t) => {
  const fx = fixture(t);
  const { urls, opened } = await startPair(t, fx);
  const base = `${urls.laptop}/api/hosts`;
  const lap = `${base}/laptop/sessions/aaaaaaaa`;
  const ws = `${base}/workstation/sessions/bbbbbbbb`;

  for (const s of [lap, ws]) {
    const st = await post(`${s}/files/stat`, { paths: ['README.md', 'nope.md', '/etc/hosts'] });
    assert.equal(st.status, 200);
    assert.equal(st.body.files[0].kind, 'markdown');
    assert.equal(st.body.files[1].exists, false);
    assert.equal(st.body.files[2].forbidden, true);
  }
  assert.equal((await post(`${ws}/files/stat`, { paths: 'x' })).status, 400);
  assert.equal((await fetch(`${lap}/files/stat`)).status, 405);
  assert.equal((await post(`${base}/laptop/sessions/zzzzzzzz/files/stat`, { paths: [] })).status, 404);

  for (const s of [lap, ws]) {
    const res = await fetch(`${s}/files/raw?path=${encodeURIComponent('README.md')}`);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('content-type'), 'text/plain; charset=utf-8');
    assert.equal(res.headers.get('x-content-type-options'), 'nosniff');
    assert.match(res.headers.get('content-security-policy'), /^sandbox/);
    assert.match(res.headers.get('content-disposition'), /^inline; filename="README.md"/);
    assert.equal(res.headers.get('x-fleet-kind'), 'markdown');
    assert.equal(await res.text(), fs.readFileSync(path.join(fx.proj, 'README.md'), 'utf8'));

    const img = await fetch(`${s}/files/raw?path=shot.png`);
    assert.equal(img.headers.get('content-type'), 'image/png');
    assert.ok(Buffer.from(await img.arrayBuffer()).equals(PNG));

    const pdf = await fetch(`${s}/files/raw?path=doc.pdf`);
    assert.equal(pdf.headers.get('content-type'), 'application/pdf');
    assert.equal(pdf.headers.get('content-security-policy'), null);
    await pdf.arrayBuffer();

    const big = await fetch(`${s}/files/raw?path=big.md`);
    assert.equal(big.status, 413);
    assert.match((await big.json()).error, /too large to preview/);
    const dl = await fetch(`${s}/files/raw?path=big.md&download=1`);
    assert.equal(dl.status, 200);
    assert.match(dl.headers.get('content-disposition'), /^attachment; filename="big.md"/);
    assert.equal(Number(dl.headers.get('content-length')), PREVIEW_MAX_BYTES + 1);
    assert.equal((await dl.arrayBuffer()).byteLength, PREVIEW_MAX_BYTES + 1);

    for (const bad of ['/etc/hosts', '../../outside/secret.txt', '~/link-out/secret.txt', '~/.ssh/id_ed25519']) {
      const r = await fetch(`${s}/files/raw?path=${encodeURIComponent(bad)}`);
      assert.equal(r.status, 403, bad);
      assert.match((await r.json()).error, /outside|secrets/);
    }
    assert.equal((await fetch(`${s}/files/raw?path=nope.md`)).status, 404);
    assert.equal((await fetch(`${s}/files/raw`)).status, 400);
    assert.equal((await fetch(`${s}/files/raw?path=docs`)).status, 400, 'a directory');
  }

  const o = await post(`${ws}/files/open`, { path: 'docs/a.md:2' });
  assert.equal(o.status, 200);
  assert.equal(o.body.host, 'workstation');
  assert.deepEqual(opened, [['workstation', 'open', [path.join(fx.proj, 'docs', 'a.md')]]], 'opened on the peer only');
  assert.equal((await post(`${lap}/files/open`, { path: '/etc/hosts' })).status, 403);
  assert.equal((await post(`${lap}/files/open`, {})).status, 400);

  // ?local=1 never chains.
  assert.equal((await fetch(`${urls.workstation}/api/hosts/laptop/sessions/aaaaaaaa/files/raw?path=README.md&local=1`)).status, 404);
});

test('HTTP: a server without the files service answers 501; an unreachable peer 502', async (t) => {
  const server = createHttpServer({ handleApi: (req, url) => api(req, url), uiDir: null });
  const url = await listen(server);
  t.after(() => server.close());
  const config = normalizeConfig({ self: 'laptop', hosts: { gone: { web: 'http://127.0.0.1:9' } } }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [{ session_id: 'aaaaaaaa-1', cwd: '/tmp' }] }) };
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, warmFleet: false });
  t.after(() => api.stop());
  assert.equal((await post(`${url}/api/hosts/laptop/sessions/aaaaaaaa-1/files/stat`, { paths: [] })).status, 501);
  const r = await fetch(`${url}/api/hosts/gone/sessions/x/files/raw?path=a`);
  assert.equal(r.status, 502);
  assert.match((await r.json()).error, /unreachable/);
});

test('plainModules: every .plain of the checkout (git), else of the directory', async (t) => {
  const fx = fixture(t);
  const specs = path.join(fx.proj, 'specs');
  fs.mkdirSync(path.join(specs, 'sub'), { recursive: true });
  for (const f of ['a.plain', 'b.plain', 'sub/c.plain']) fs.writeFileSync(path.join(specs, f), '- x\n');
  const calls = [];
  const gitRun = async (bin, args) => {
    calls.push(args.slice(2, 3)[0]);
    if (args.includes('rev-parse')) return { stdout: `${fx.proj}\n` };
    return { stdout: ['specs/a.plain', 'specs/b.plain', 'specs/sub/c.plain', ''].join('\0') };
  };
  const r = await createFiles({ home: fx.home, run: gitRun }).plainModules(path.join(specs, 'a.plain'), fx.proj);
  assert.equal(r.root, fx.proj);
  assert.deepEqual(r.files, ['a.plain', 'b.plain', 'sub/c.plain'].map((f) => path.join(specs, f)));
  assert.deepEqual(calls, ['rev-parse', 'ls-files']);

  // No git: the file's own directory.
  const noGit = createFiles({ home: fx.home, run: async () => { throw new Error('not a repo'); } });
  const d = await noGit.plainModules(path.join(specs, 'a.plain'), fx.proj);
  assert.deepEqual(d.files, ['a.plain', 'b.plain'].map((f) => path.join(specs, f)));
  // The file itself must be servable.
  await assert.rejects(noGit.plainModules('/etc/hosts', fx.proj), { status: 403 });
});
