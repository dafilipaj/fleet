import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';

import { createRelay } from '../lib/relay.mjs';
import { normalizeConfig } from '../lib/config.mjs';

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
}
function close(...servers) {
  return Promise.all(servers.map((s) => new Promise((r) => { s.closeAllConnections?.(); s.close(() => r()); })));
}

// A TCP echo server: every byte comes back, prefixed with "echo:" once.
async function echoServer() {
  const sockets = new Set();
  const server = net.createServer((s) => {
    sockets.add(s);
    s.on('close', () => sockets.delete(s));
    s.write('echo:');
    s.pipe(s);
  });
  const port = await listen(server);
  server.closeAllConnections = () => sockets.forEach((s) => s.destroy());
  return { server, port };
}

// A fake upstream HTTP proxy: records each CONNECT; `refuse` answers 403 instead of tunnelling.
async function upstreamProxy({ refuse = false } = {}) {
  const seen = [];
  const server = http.createServer((req, res) => res.writeHead(405).end());
  server.on('connect', (req, client, head) => {
    seen.push(req.url);
    if (refuse) return client.end('HTTP/1.1 403 Forbidden\r\n\r\n');
    const [host, port] = req.url.split(':');
    // The test targets resolve to loopback; anything else would be a leak.
    const out = net.connect(Number(port), host === 'echo.test' ? '127.0.0.1' : host, () => {
      client.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      if (head.length) out.write(head);
      out.pipe(client);
      client.pipe(out);
    });
    out.on('error', () => client.destroy());
    client.on('error', () => out.destroy());
  });
  const port = await listen(server);
  return { server, port, seen };
}

// Send a raw CONNECT to the relay; resolve with { status, socket, rest } once the head is in.
function connect(relayPort, target) {
  return new Promise((resolve, reject) => {
    const socket = net.connect(relayPort, '127.0.0.1', () => socket.write(`CONNECT ${target} HTTP/1.1\r\nHost: ${target}\r\n\r\n`));
    let buf = Buffer.alloc(0);
    const onData = (c) => {
      buf = Buffer.concat([buf, c]);
      const end = buf.indexOf('\r\n\r\n');
      if (end < 0) return;
      socket.off('data', onData);
      resolve({ status: Number(/^HTTP\/1\.1 (\d{3})/.exec(buf.toString('latin1'))?.[1]), socket, rest: buf.subarray(end + 4) });
    };
    socket.on('data', onData);
    socket.on('error', reject);
  });
}

function readUntil(socket, initial, want) {
  return new Promise((resolve) => {
    let got = initial.toString();
    if (got.includes(want)) return resolve(got);
    socket.on('data', (c) => {
      got += c.toString();
      if (got.includes(want)) resolve(got);
    });
  });
}

test('relay: an allowed target is tunnelled end to end through the upstream proxy', async () => {
  const echo = await echoServer();
  const up = await upstreamProxy();
  const logs = [];
  const relay = createRelay({ allow: [`echo.test:${echo.port}`], via: { host: '127.0.0.1', port: up.port }, log: (l) => logs.push(l) });
  const relayPort = await listen(relay);
  try {
    // Case-insensitive host match.
    const { status, socket, rest } = await connect(relayPort, `ECHO.test:${echo.port}`);
    assert.equal(status, 200);
    socket.write('hello relay');
    const got = await readUntil(socket, rest, 'hello relay');
    assert.equal(got, 'echo:hello relay');
    assert.deepEqual(up.seen, [`echo.test:${echo.port}`]);
    socket.destroy();
    assert.deepEqual(logs, []);
  } finally {
    await close(relay, up.server, echo.server);
  }
});

test('relay: without via the target is dialled directly', async () => {
  const echo = await echoServer();
  const relay = createRelay({ allow: [`127.0.0.1:${echo.port}`] });
  const relayPort = await listen(relay);
  try {
    const { status, socket, rest } = await connect(relayPort, `127.0.0.1:${echo.port}`);
    assert.equal(status, 200);
    socket.write('direct');
    assert.equal(await readUntil(socket, rest, 'direct'), 'echo:direct');
    socket.destroy();
  } finally {
    await close(relay, echo.server);
  }
});

test('relay: a target not on the allowlist → 403, nothing dialled', async () => {
  const echo = await echoServer();
  const up = await upstreamProxy();
  const logs = [];
  const relay = createRelay({ allow: [`echo.test:${echo.port}`], via: { host: '127.0.0.1', port: up.port }, log: (l) => logs.push(l) });
  const relayPort = await listen(relay);
  try {
    for (const target of [`echo.test:${echo.port + 1}`, `other.test:${echo.port}`, `echo.test.:${echo.port}`, `x.echo.test:${echo.port}`, `echo.test:0${echo.port}`, `echo.test`]) {
      const { status, socket } = await connect(relayPort, target);
      assert.equal(status, 403, target);
      socket.destroy();
    }
    assert.deepEqual(up.seen, []);
    assert.equal(logs.length, 6);
    assert.match(logs[0], /refused CONNECT .* not allowed/);
  } finally {
    await close(relay, up.server, echo.server);
  }
});

test('relay: any request other than CONNECT → 405', async () => {
  const logs = [];
  const relay = createRelay({ allow: ['example.test:443'], log: (l) => logs.push(l) });
  const relayPort = await listen(relay);
  try {
    for (const method of ['GET', 'POST']) {
      const res = await fetch(`http://127.0.0.1:${relayPort}/http://example.test/`, { method, ...(method === 'POST' ? { body: 'x' } : {}) });
      assert.equal(res.status, 405);
      assert.equal(res.headers.get('allow'), 'CONNECT');
    }
    assert.equal(logs.length, 2);
  } finally {
    await close(relay);
  }
});

test('relay: an upstream proxy that refuses (or is down) → 502', async () => {
  const up = await upstreamProxy({ refuse: true });
  const relay = createRelay({ allow: ['echo.test:443'], via: { host: '127.0.0.1', port: up.port } });
  const relayPort = await listen(relay);
  const dead = createRelay({ allow: ['echo.test:443'], via: { host: '127.0.0.1', port: 1 } });
  const deadPort = await listen(dead);
  try {
    const r1 = await connect(relayPort, 'echo.test:443');
    assert.equal(r1.status, 502);
    r1.socket.destroy();
    assert.deepEqual(up.seen, ['echo.test:443']);
    const r2 = await connect(deadPort, 'echo.test:443');
    assert.equal(r2.status, 502);
    r2.socket.destroy();
  } finally {
    await close(relay, dead, up.server);
  }
});

test('relay: closing the relay ends the tunnels in flight', async () => {
  const echo = await echoServer();
  const relay = createRelay({ allow: [`127.0.0.1:${echo.port}`] });
  const relayPort = await listen(relay);
  const { status, socket } = await connect(relayPort, `127.0.0.1:${echo.port}`);
  assert.equal(status, 200);
  const closed = new Promise((r) => socket.on('close', r));
  await close(relay, echo.server);
  await closed;
});

test('config: web.relay is normalized and validated', () => {
  const base = (relay) => ({ self: 'laptop', hosts: { laptop: { web: 'http://100.x.y.z:7777' } }, web: { relay } });
  const norm = (relay) => normalizeConfig(base(relay), { home: '/home/tester', log: { warn() {} } }).relay;
  assert.equal(norm(undefined), null);
  assert.deepEqual(norm({ listen: '100.64.0.1:1057', via: 'http://localhost:1056/', allow: ['Studio.Tailnet.Example:443', ' other.example:8443 '] }), {
    host: '100.64.0.1',
    port: 1057,
    via: { host: 'localhost', port: 1056, url: 'http://localhost:1056' },
    allow: ['studio.tailnet.example:443', 'other.example:8443'],
  });
  assert.deepEqual(norm({ listen: '[fd7a::1]:1057', allow: ['a.example:443'] }), { host: 'fd7a::1', port: 1057, via: null, allow: ['a.example:443'] });

  const bad = [
    ['x', /must be an object/],
    [{ allow: ['a.example:443'] }, /listen must be "<ip>:<port>"/],
    [{ listen: 'studio:1057', allow: ['a.example:443'] }, /listen must be "<ip>:<port>"/],
    [{ listen: '100.64.0.1', allow: ['a.example:443'] }, /listen must be "<ip>:<port>"/],
    [{ listen: '0.0.0.0:1057', allow: ['a.example:443'] }, /specific address/],
    [{ listen: '100.64.0.1:70000', allow: ['a.example:443'] }, /port invalid/],
    [{ listen: '100.64.0.1:1057', via: 'localhost:1056', allow: ['a.example:443'] }, /via must be an http:\/\/host:port/],
    [{ listen: '100.64.0.1:1057', via: 'http://u:p@localhost:1056', allow: ['a.example:443'] }, /via must be/],
    [{ listen: '100.64.0.1:1057', via: 'https://localhost:1056', allow: ['a.example:443'] }, /via must be/],
    [{ listen: '100.64.0.1:1057' }, /allow must be a non-empty array/],
    [{ listen: '100.64.0.1:1057', allow: [] }, /allow must be a non-empty array/],
    ...['*', '*.example:443', 'a.example', 'a.example:*', 'https://a.example:443', 'a.example:0', 'a.example:99999', 'a.example:0443', 'a..example:443', 42].map((t) => [
      { listen: '100.64.0.1:1057', allow: [t] },
      /allow entries must be exact "host:port"|port of .* invalid/,
    ]),
  ];
  for (const [relay, re] of bad) assert.throws(() => norm(relay), re, JSON.stringify(relay));
});
