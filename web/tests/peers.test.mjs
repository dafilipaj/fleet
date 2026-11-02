import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';

import { fetchPeerHost, proxyToPeer, setPeerConnects, setPeerProxies, streamFromPeer } from '../lib/peers.mjs';
import { normalizeConfig } from '../lib/config.mjs';

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

// A peer fleet web server and a forward proxy that counts what goes through it.
async function fixtures() {
  const peer = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (url.pathname === '/api/fleet') {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ hosts: [{ name: 'studio', ok: true, fetchedAt: 1, sessions: [{ name: 's1' }] }] }));
    } else if (url.pathname === '/api/echo') {
      let body = '';
      req.on('data', (c) => (body += c));
      req.on('end', () => res.end(JSON.stringify({ method: req.method, local: url.searchParams.get('local'), body })));
    } else res.end('raw');
  });
  const seen = [];
  const proxy = http.createServer((req, res) => {
    seen.push(req.url);
    const out = http.request(req.url, { method: req.method, headers: req.headers }, (r) => {
      res.writeHead(r.statusCode, r.headers);
      r.pipe(res);
    });
    req.pipe(out);
  });
  const peerUrl = await listen(peer);
  const proxyUrl = await listen(proxy);
  const close = () => Promise.all([peer, proxy].map((s) => new Promise((r) => { s.closeAllConnections(); s.close(r); })));
  return { peerUrl, proxyUrl, seen, close };
}

test('a peer with webProxy is reached through the proxy, others directly', async () => {
  const { peerUrl, proxyUrl, seen, close } = await fixtures();
  try {
    setPeerProxies({ [new URL(peerUrl).origin]: proxyUrl });
    const host = await fetchPeerHost('studio', peerUrl);
    assert.equal(host.ok, true);
    assert.deepEqual(host.sessions.map((s) => s.name), ['s1']);

    const r = await proxyToPeer(peerUrl, { method: 'POST', pathname: '/api/echo', body: '{"a":1}' });
    assert.deepEqual(r, { status: 200, body: { method: 'POST', local: '1', body: '{"a":1}' } });

    const s = await streamFromPeer(peerUrl, { pathname: '/raw' });
    assert.equal(s.status, 200);
    s.stream.resume();
    assert.equal(seen.length, 3);
    assert.match(seen[0], /\/api\/fleet\?local=1$/);

    setPeerProxies({});
    assert.equal((await fetchPeerHost('studio', peerUrl)).ok, true);
    assert.equal(seen.length, 3, 'no proxy once unset');
  } finally {
    setPeerProxies({});
    await close();
  }
});

test('an unreachable proxy makes the peer unreachable, not a crash', async () => {
  const { peerUrl, close } = await fixtures();
  try {
    setPeerProxies({ [new URL(peerUrl).origin]: 'http://127.0.0.1:1' });
    const host = await fetchPeerHost('studio', peerUrl, { timeoutMs: 2000 });
    assert.equal(host.ok, false);
    assert.match(host.error, /^unreachable:/);
  } finally {
    setPeerProxies({});
    await close();
  }
});

test('config: hosts.<name>.webProxy maps the peer origin to the proxy and is validated', () => {
  const raw = {
    self: 'laptop',
    hosts: {
      laptop: { web: 'http://100.x.y.z:7777' },
      studio: { ssh: 'studio', web: 'https://fleet.studio.example/', webProxy: 'http://localhost:1056/' },
    },
  };
  const cfg = normalizeConfig(raw, { home: '/home/tester', log: { warn() {} } });
  assert.deepEqual(cfg.peerProxies, { 'https://fleet.studio.example': 'http://localhost:1056' });
  raw.hosts.studio.webProxy = 'localhost:1056';
  assert.throws(() => normalizeConfig(raw, { home: '/home/tester', log: { warn() {} } }), /webProxy must be an http\(s\) url/);
});

test('a peer with webConnect is dialled at that address, keeping its own name as Host', async () => {
  const hosts = [];
  const peer = http.createServer((req, res) => {
    hosts.push(req.headers.host);
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ hosts: [{ name: 'studio', ok: true, fetchedAt: 1, sessions: [] }], local: new URL(req.url, 'http://x').searchParams.get('local') }));
  });
  const peerUrl = await listen(peer);
  const port = new URL(peerUrl).port;
  // The web url names a host that does not resolve; webConnect points at the real listener.
  const base = `http://fleet.studio.invalid:${port}`;
  setPeerConnects({ [base]: `127.0.0.1:${port}` });
  try {
    const host = await fetchPeerHost('studio', base);
    assert.equal(host.ok, true);
    const proxied = await proxyToPeer(base, { pathname: '/api/fleet' });
    assert.equal(proxied.status, 200);
    assert.equal(proxied.body.local, '1');
    const streamed = await streamFromPeer(base, { pathname: '/api/fleet' });
    assert.equal(streamed.status, 200);
    streamed.stream.resume();
    assert.deepEqual(hosts, Array(3).fill(`fleet.studio.invalid:${port}`));
  } finally {
    setPeerConnects({});
    peer.closeAllConnections();
    await new Promise((r) => peer.close(r));
  }
});

test('config: hosts.<name>.webConnect maps the peer origin to the address and is validated', () => {
  const raw = {
    self: 'laptop',
    hosts: {
      laptop: { web: 'http://100.x.y.z:7777' },
      studio: { ssh: 'studio', web: 'https://fleet.studio.example/', webConnect: 'studio.tailnet.example' },
    },
  };
  const cfg = normalizeConfig(raw, { home: '/home/tester', log: { warn() {} } });
  assert.deepEqual(cfg.peerConnects, { 'https://fleet.studio.example': 'studio.tailnet.example' });
  raw.hosts.studio.webConnect = 'https://studio.tailnet.example';
  assert.throws(() => normalizeConfig(raw, { home: '/home/tester', log: { warn() {} } }), /webConnect must be a host or host:port/);
});
