import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';

import { createHttpServer } from '../lib/app.mjs';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { COOKIE, FAILURE_WINDOW_MS, MAX_FAILURES, createAuth, createFileSecret, createKeychainSecret, createSecret } from '../lib/auth.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { apiScript } from '../lib/operator.mjs';
import { fetchPeerHost, setPeerTokens } from '../lib/peers.mjs';

const TOKEN = 'a'.repeat(64);
const PASSWORD = 'correct horse';

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

function call(base, pathname, { method = 'GET', headers = {}, body = null } = {}) {
  return new Promise((resolve, reject) => {
    const out = http.request(`${base}${pathname}`, { method, headers }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, text: Buffer.concat(chunks).toString('utf8') }));
    });
    out.on('error', reject);
    out.end(body ?? undefined);
  });
}

const form = (v) => ({ method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' }, body: `token=${encodeURIComponent(v)}` });
const authWith = (opts = {}) => createAuth({ enabled: true, getToken: async () => TOKEN, getPassword: async () => PASSWORD, delay: async () => {}, ...opts });

test('createKeychainSecret: security find-generic-password; a miss is retried after 30 s, a short value ignored', async () => {
  let t = 0;
  const calls = [];
  let answer = null;
  const run = async (cmd, args) => {
    calls.push([cmd, ...args]);
    if (answer == null) throw new Error('not found');
    return { stdout: `${answer}\n` };
  };
  const get = createKeychainSecret({ service: 'fleet-web-token', run, minLength: 32, now: () => t });
  assert.equal(await get(), null);
  assert.deepEqual(calls[0], ['security', 'find-generic-password', '-s', 'fleet-web-token', '-w']);
  answer = TOKEN;
  assert.equal(await get(), null, 'still inside the miss window');
  t += 31 * 1000;
  assert.equal(await get(), TOKEN);
  assert.equal(await get(), TOKEN);
  assert.equal(calls.length, 2, 'a hit is cached');

  const short = createKeychainSecret({ service: 's', run: async () => ({ stdout: 'short' }), minLength: 32 });
  assert.equal(await short(), null);
  assert.equal(await createKeychainSecret({ service: null, run })(), null);
});

test('createAuth: off when not enabled; Bearer or cookie when on; /api/health open; fails closed without a token', async () => {
  const u = (p) => new URL(`http://x${p}`);
  assert.equal(await createAuth().check({ headers: {} }, u('/api/fleet')), null);

  const auth = authWith();
  assert.equal(await auth.check({ headers: {} }, u('/api/health')), null);
  assert.equal(await auth.check({ headers: { authorization: `Bearer ${TOKEN}` } }, u('/api/fleet')), null);
  assert.equal((await auth.check({ headers: { authorization: `Bearer ${PASSWORD}` } }, u('/api/fleet'))).status, 401, 'the password only signs in');
  assert.equal((await auth.check({ headers: { cookie: `${COOKIE}=${TOKEN}` } }, u('/api/fleet'))).status, 401, 'the cookie holds a derived value, not the token');
  assert.equal((await auth.check({ method: 'GET', headers: { accept: 'text/html' } }, u('/'))).page, true);

  const locked = authWith({ getToken: async () => null });
  const refused = await locked.check({ headers: { authorization: `Bearer ${TOKEN}` } }, u('/api/fleet'));
  assert.equal(refused.status, 503);
  assert.equal(await locked.check({ headers: {} }, u('/api/health')), null);
});

test('server with web.auth: 401 from loopback, token or password sign in, health stays open', async () => {
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: { ok: true } }), auth: authWith() });
  const base = await listen(server);
  try {
    const denied = await call(base, '/api/fleet');
    assert.equal(denied.status, 401);
    assert.equal(denied.headers['x-fleet-auth'], 'login');
    assert.equal((await call(base, '/api/health')).status, 200);
    assert.equal((await call(base, '/api/fleet', { headers: { authorization: `Bearer ${TOKEN}` } })).status, 200);

    const nav = await call(base, '/', { headers: { accept: 'text/html' } });
    assert.equal(nav.status, 302);
    assert.equal(nav.headers.location, '/login');
    assert.match((await call(base, '/login')).text, /needs its password or token/);

    const wrong = await call(base, '/login', form('nope'));
    assert.equal(wrong.status, 401);
    assert.equal(wrong.headers['set-cookie'], undefined);

    for (const secret of [TOKEN, PASSWORD]) {
      const ok = await call(base, '/login', form(secret));
      assert.equal(ok.status, 303);
      const cookie = ok.headers['set-cookie'][0];
      assert.match(cookie, /HttpOnly/);
      assert.match(cookie, /SameSite=Lax/);
      assert.doesNotMatch(cookie, /Secure/, 'plain http: no Secure flag');
      assert.ok(!cookie.includes(TOKEN) && !cookie.includes(encodeURIComponent(PASSWORD)));
      assert.equal((await call(base, '/api/fleet', { headers: { cookie: cookie.split(';')[0] } })).status, 200);
    }

    assert.match((await call(base, '/logout', { method: 'POST' })).headers['set-cookie'][0], /Max-Age=0/);
  } finally {
    server.close();
  }
});

test('server with web.auth: wrong logins lock /login out for a while', async () => {
  let t = 0;
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: {} }), auth: authWith({ now: () => t }) });
  const base = await listen(server);
  try {
    for (let i = 0; i < MAX_FAILURES; i++) assert.equal((await call(base, '/login', form(`wrong${i}`))).status, 401);
    const locked = await call(base, '/login', form(PASSWORD));
    assert.equal(locked.status, 429, 'even the right password waits');
    t += FAILURE_WINDOW_MS + 1;
    assert.equal((await call(base, '/login', form(PASSWORD))).status, 303);
  } finally {
    server.close();
  }
});

test('server with web.auth and a locked Keychain: 503, health open', async () => {
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: {} }), auth: authWith({ getToken: async () => null }) });
  const base = await listen(server);
  try {
    const r = await call(base, '/api/fleet', { headers: { authorization: `Bearer ${TOKEN}` } });
    assert.equal(r.status, 503);
    assert.equal(r.headers['x-fleet-auth'], undefined);
    assert.equal((await call(base, '/api/health')).status, 200);
  } finally {
    server.close();
  }
});

test('server without web.auth: unchanged', async () => {
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: { ok: true } }), auth: createAuth() });
  const base = await listen(server);
  try {
    assert.equal((await call(base, '/api/fleet')).status, 200);
  } finally {
    server.close();
  }
});

test('config: web.auth Keychain services and hosts.<name>.webTokenKeychain', () => {
  const cfg = normalizeConfig(
    { self: 'laptop', web: { auth: { keychain: 'fleet-web-token', passwordKeychain: 'fleet-web-password' } }, hosts: { laptop: {}, workstation: { web: 'http://100.x.y.z:7777', webTokenKeychain: 'fleet-web-token.workstation' } } },
    { env: {}, home: '/h' },
  );
  assert.equal(cfg.authKeychain, 'fleet-web-token');
  assert.equal(cfg.authPasswordKeychain, 'fleet-web-password');
  assert.deepEqual(cfg.peerTokenKeychains, { 'http://100.x.y.z:7777': 'fleet-web-token.workstation' });

  const none = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/h' });
  assert.equal(none.authKeychain, null);
  assert.deepEqual(none.peerTokenKeychains, {});

  assert.throws(() => normalizeConfig({ self: 'laptop', web: { auth: { passwordFile: '/x' } } }, { env: {}, home: '/h' }), /web.auth.keychain \(or tokenFile\) is required/);
  assert.throws(() => normalizeConfig({ self: 'laptop', web: { auth: 'x' } }, { env: {}, home: '/h' }), /web.auth must be an object/);
});

test('peers: a peer with a token gets it as a Bearer header', async () => {
  let seen = null;
  const peer = http.createServer((req, res) => {
    seen = req.headers.authorization ?? null;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ hosts: [{ name: 'workstation', ok: true, sessions: [] }] }));
  });
  const base = await listen(peer);
  try {
    setPeerTokens({ [new URL(base).origin]: TOKEN });
    const host = await fetchPeerHost('workstation', base);
    assert.equal(host.ok, true);
    assert.equal(seen, `Bearer ${TOKEN}`);
    setPeerTokens({});
    await fetchPeerHost('workstation', base);
    assert.equal(seen, null);
  } finally {
    setPeerTokens({});
    peer.close();
  }
});

test('operator ./api: reads the token from the Keychain and keeps it out of argv', () => {
  const script = apiScript('http://127.0.0.1:7777/api', "fleet-web-'token");
  assert.match(script, /token=\$\(security find-generic-password -s 'fleet-web-'\\''token' -w\)/);
  assert.match(script, /curl -sS -K - /);
  assert.doesNotMatch(apiScript('http://127.0.0.1:7777/api'), /token=/);
});

test('config: web.auth.tokenFile / passwordFile and hosts.<name>.webTokenFile (~ expanded)', () => {
  const cfg = normalizeConfig(
    { self: 'laptop', web: { auth: { tokenFile: '~/.config/fleet/web-token', passwordFile: '/etc/fleet/pw' } }, hosts: { laptop: {}, workstation: { web: 'http://100.x.y.z:7777', webTokenFile: '~/.config/fleet/peer-token' } } },
    { env: {}, home: '/h' },
  );
  assert.equal(cfg.authKeychain, null);
  assert.equal(cfg.authTokenFile, '/h/.config/fleet/web-token');
  assert.equal(cfg.authPasswordFile, '/etc/fleet/pw');
  assert.deepEqual(cfg.peerTokenFiles, { 'http://100.x.y.z:7777': '/h/.config/fleet/peer-token' });
  assert.deepEqual(cfg.peerTokenKeychains, {});
  assert.throws(() => normalizeConfig({ self: 'laptop', hosts: { workstation: { web: 'http://100.x.y.z:7777', webTokenFile: '' } } }, { env: {}, home: '/h' }), /webTokenFile must be a file path/);
});

test('createFileSecret: trimmed, cached, refuses a file its group or others can read', async (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-web-secret-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'token');
  const logs = [];
  let clock = 0;
  const get = createFileSecret({ file, minLength: 32, now: () => clock, log: (m) => logs.push(m) });
  assert.equal(await get(), null, 'no file yet');
  fs.writeFileSync(file, `  ${TOKEN}\n`, { mode: 0o644 });
  fs.chmodSync(file, 0o644);
  clock += 31 * 1000;
  assert.equal(await get(), null, 'group/other-readable: refused');
  assert.match(logs.join('\n'), /readable or writable by its group or others/);
  fs.chmodSync(file, 0o600);
  assert.equal(await get(), null, 'a miss is retried only after 30 s');
  clock += 31 * 1000;
  assert.equal(await get(), TOKEN, 'trimmed');
  fs.writeFileSync(file, 'changed-but-cached');
  assert.equal(await get(), TOKEN, 'a hit is cached for good');

  const tilde = createFileSecret({ file: '~/token', home: dir, minLength: 32, log: (m) => logs.push(m) });
  assert.equal(await tilde(), null, '~ expanded; a value shorter than minLength is ignored');
  assert.match(logs.join('\n'), /shorter than 32 characters/);
});

test('createSecret: the file wins over the Keychain item when both are set', async (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-web-secret-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'token');
  fs.writeFileSync(file, TOKEN, { mode: 0o600 });
  const calls = [];
  const run = async (...a) => (calls.push(a), { stdout: 'b'.repeat(64) });
  assert.equal(await createSecret({ file, service: 'svc', run, minLength: 32 })(), TOKEN);
  assert.equal(calls.length, 0, 'no Keychain call');
  assert.equal(await createSecret({ file: null, service: 'svc', run, minLength: 32 })(), 'b'.repeat(64));
});

test('operator ./api: reads the token from web.auth.tokenFile when set', () => {
  const script = apiScript('http://127.0.0.1:7777/api', 'svc', "/h/.config/fleet/web-'token");
  assert.match(script, /token=\$\(tr -d '\[:space:\]' < '\/h\/\.config\/fleet\/web-'\\''token'\)/);
  assert.doesNotMatch(script, /security/);
});
