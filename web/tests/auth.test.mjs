import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

import { createHttpServer } from '../lib/app.mjs';
import { COOKIE, createAuth, readTokenFile } from '../lib/auth.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { apiScript } from '../lib/operator.mjs';
import { fetchPeerHost, setPeerTokens } from '../lib/peers.mjs';

const TOKEN = 'a'.repeat(64);

function tmp() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-auth-'));
}

function tokenFile(dir, value = TOKEN, mode = 0o600) {
  const f = path.join(dir, 'token');
  fs.writeFileSync(f, `${value}\n`);
  fs.chmodSync(f, mode);
  return f;
}

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

test('readTokenFile: trimmed token; refuses group/other bits, short tokens, a missing file', () => {
  const dir = tmp();
  assert.equal(readTokenFile(tokenFile(dir)), TOKEN);
  assert.throws(() => readTokenFile(tokenFile(dir, TOKEN, 0o644)), /readable by others/);
  assert.throws(() => readTokenFile(tokenFile(dir, 'short')), /at least 32/);
  assert.throws(() => readTokenFile(path.join(dir, 'nope')), /not found/);
});

test('createAuth: off without a token, Bearer or cookie when on, /api/health open', () => {
  const off = createAuth({ token: null });
  assert.equal(off.enabled, false);
  assert.equal(off.check({ headers: {} }, new URL('http://x/api/fleet')), null);

  const auth = createAuth({ token: TOKEN });
  const u = (p) => new URL(`http://x${p}`);
  assert.equal(auth.check({ headers: {} }, u('/api/health')), null);
  assert.equal(auth.check({ headers: { authorization: `Bearer ${TOKEN}` } }, u('/api/fleet')), null);
  assert.equal(auth.check({ headers: { authorization: 'Bearer nope' } }, u('/api/fleet')).status, 401);
  assert.equal(auth.check({ headers: { cookie: `${COOKIE}=${TOKEN}` } }, u('/api/fleet')).status, 401, 'the cookie holds a derived value, not the token');
  const page = auth.check({ method: 'GET', headers: { accept: 'text/html' } }, u('/'));
  assert.equal(page.page, true);
  assert.equal(auth.check({ method: 'GET', headers: { accept: 'application/json' } }, u('/api/fleet')).page, false);
});

test('server with web.auth: 401 from loopback, login sets a cookie that works, health stays open', async () => {
  const auth = createAuth({ token: TOKEN, delay: async () => {} });
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: { ok: true } }), auth });
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
    assert.match((await call(base, '/login')).text, /<form method="post" action="\/login">/);

    const wrong = await call(base, '/login', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' }, body: 'token=nope' });
    assert.equal(wrong.status, 401);
    assert.equal(wrong.headers['set-cookie'], undefined);

    const ok = await call(base, '/login', { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' }, body: `token=${TOKEN}` });
    assert.equal(ok.status, 303);
    const cookie = ok.headers['set-cookie'][0];
    assert.match(cookie, /HttpOnly/);
    assert.match(cookie, /SameSite=Lax/);
    assert.doesNotMatch(cookie, /Secure/, 'plain http: no Secure flag');
    assert.ok(!cookie.includes(TOKEN));
    const withCookie = await call(base, '/api/fleet', { headers: { cookie: cookie.split(';')[0] } });
    assert.equal(withCookie.status, 200);

    const out = await call(base, '/logout', { method: 'POST' });
    assert.match(out.headers['set-cookie'][0], /Max-Age=0/);
  } finally {
    server.close();
  }
});

test('server without web.auth: unchanged', async () => {
  const server = createHttpServer({ handleApi: async () => ({ status: 200, body: { ok: true } }), auth: createAuth({ token: null }) });
  const base = await listen(server);
  try {
    assert.equal((await call(base, '/api/fleet')).status, 200);
  } finally {
    server.close();
  }
});

test('config: web.auth.tokenFile and hosts.<name>.webTokenFile', () => {
  const dir = tmp();
  const file = tokenFile(dir);
  const cfg = normalizeConfig(
    { self: 'laptop', web: { auth: { tokenFile: file } }, hosts: { laptop: {}, workstation: { web: 'http://100.x.y.z:7777', webTokenFile: file } } },
    { env: {}, home: dir },
  );
  assert.equal(cfg.authToken, TOKEN);
  assert.equal(cfg.authTokenFile, file);
  assert.deepEqual(cfg.peerTokens, { 'http://100.x.y.z:7777': TOKEN });

  const none = normalizeConfig({ self: 'laptop' }, { env: {}, home: dir });
  assert.equal(none.authToken, null);
  assert.deepEqual(none.peerTokens, {});

  assert.throws(() => normalizeConfig({ self: 'laptop', web: { auth: { tokenFile: tokenFile(dir, TOKEN, 0o640) } } }, { env: {}, home: dir }), /readable by others/);
  assert.throws(() => normalizeConfig({ self: 'laptop', web: { auth: 'x' } }, { env: {}, home: dir }), /web.auth must be an object/);
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

test('operator ./api: reads the token file and keeps it out of argv', () => {
  const script = apiScript('http://127.0.0.1:7777/api', "/h/it's/token");
  assert.match(script, /token=\$\(cat '\/h\/it'\\''s\/token'\)/);
  assert.match(script, /curl -sS -K - /);
  assert.doesNotMatch(apiScript('http://127.0.0.1:7777/api'), /token=/);
});
