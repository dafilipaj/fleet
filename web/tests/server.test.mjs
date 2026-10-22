import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import fs from 'node:fs';
import path from 'node:path';
import { PassThrough } from 'node:stream';

import {
  clampLines,
  findSession,
  resolveHost,
  sortSessions,
  statusRank,
  tailLines,
  validateKey,
  validateSendText,
} from '../lib/util.mjs';
import { resolveStaticPath, readJsonBody, contentTypeFor, HttpError } from '../lib/http.mjs';
import { createBackend, BackendError, createScriptProvider, ITERM_SCRIPT } from '../lib/backends.mjs';
import { createFleet, trimTitle, TITLE_MAX } from '../lib/fleet.mjs';
import { createFleetCli } from '../lib/fleet-cli.mjs';
import { ensurePath, ensureUtf8Locale, pathOnly } from '../lib/config.mjs';
import { spawnSync } from 'node:child_process';

// ------------------------------------------------------------------ tailLines

test('tailLines takes the last N lines', () => {
  const text = 'a\nb\nc\nd\ne';
  assert.equal(tailLines(text, 2), 'd\ne');
  assert.equal(tailLines(text, 99), 'a\nb\nc\nd\ne');
});

test('tailLines trims trailing blank lines and per-line trailing whitespace', () => {
  const text = 'one   \ntwo\t\n\n   \n\n';
  assert.equal(tailLines(text, 100), 'one\ntwo');
});

test('tailLines counts from the last non-blank line (short output in a tall pane)', () => {
  const pane = `$ cmd\noutput${'\n'.repeat(40)}`;
  assert.equal(tailLines(pane, 10), '$ cmd\noutput');
  assert.equal(tailLines(pane, 1), 'output');
});

test('tailLines keeps interior blank lines', () => {
  assert.equal(tailLines('a\n\nb\n\n\n', 100), 'a\n\nb');
});

test('tailLines normalises CRLF and handles empty input', () => {
  assert.equal(tailLines('a\r\nb\r\n', 10), 'a\nb');
  assert.equal(tailLines('', 10), '');
  assert.equal(tailLines(undefined, 10), '');
});

test('tailLines slices after dropping the blank bottom, not before', () => {
  assert.equal(tailLines('x\ny\nz\n\n\n', 2), 'y\nz');
});

// ------------------------------------------------------------------ clampLines

test('clampLines defaults and clamps to 10..2000', () => {
  assert.equal(clampLines(undefined), 200);
  assert.equal(clampLines(null), 200);
  assert.equal(clampLines('abc'), 200);
  assert.equal(clampLines('1'), 10);
  assert.equal(clampLines('-5'), 10);
  assert.equal(clampLines('99999'), 2000);
  assert.equal(clampLines('30'), 30);
});

// ------------------------------------------------------------------ sorting

test('statusRank orders waiting > busy > idle > unknown', () => {
  assert.deepEqual(
    ['unknown', 'idle', 'busy', 'waiting'].map(statusRank).sort((a, b) => a - b),
    [0, 1, 2, 3],
  );
  assert.equal(statusRank('bogus'), 3);
  assert.equal(statusRank(undefined), 3);
  // Claude Code's `shell`: idle, a background shell running.
  assert.equal(statusRank('shell'), statusRank('idle'));
});

test('sortSessions: waiting, busy, idle, unknown, then updated_at desc', () => {
  const input = [
    { session_id: 'i1', status: 'idle', updated_at: 100 },
    { session_id: 'b1', status: 'busy', updated_at: 50 },
    { session_id: 'u1', status: 'weird', updated_at: 9999 },
    { session_id: 'w1', status: 'waiting', updated_at: 1 },
    { session_id: 'b2', status: 'busy', updated_at: 500 },
    { session_id: 'i2', status: 'idle', updated_at: 900 },
  ];
  assert.deepEqual(
    sortSessions(input).map((s) => s.session_id),
    ['w1', 'b2', 'b1', 'i2', 'i1', 'u1'],
  );
});

test('sortSessions does not mutate its input and tolerates junk', () => {
  const input = [{ status: 'idle' }, { status: 'waiting' }];
  const copy = [...input];
  sortSessions(input);
  assert.deepEqual(input, copy);
  assert.deepEqual(sortSessions(undefined), []);
});

// ------------------------------------------------------------------ host resolution

const CFG = { self: 'laptop', peers: { workstation: 'http://workstation.example:7777' } };

test('resolveHost: self, peer, unknown', () => {
  assert.deepEqual(resolveHost('laptop', CFG), { kind: 'self' });
  assert.deepEqual(resolveHost('workstation', CFG), { kind: 'peer', url: 'http://workstation.example:7777' });
  assert.deepEqual(resolveHost('nope', CFG), { kind: 'unknown' });
  assert.deepEqual(resolveHost('', CFG), { kind: 'unknown' });
  assert.deepEqual(resolveHost(undefined, CFG), { kind: 'unknown' });
});

test('resolveHost ignores prototype pollution style names', () => {
  assert.deepEqual(resolveHost('toString', CFG), { kind: 'unknown' });
  assert.deepEqual(resolveHost('__proto__', CFG), { kind: 'unknown' });
});

// ------------------------------------------------------------------ session lookup

const SESSIONS = [
  { session_id: 'aaaaaaaa-1111-2222-3333-444444444444', name: 'one' },
  { session_id: 'aaaaaaaa-5555-6666-7777-888888888888', name: 'two' },
  { session_id: 'bbbbbbbb-0000-0000-0000-000000000000', name: 'three' },
];

test('findSession matches exact id', () => {
  assert.equal(findSession(SESSIONS, SESSIONS[1].session_id).name, 'two');
});

test('findSession matches a unique prefix of >= 8 chars', () => {
  assert.equal(findSession(SESSIONS, 'bbbbbbbb').name, 'three');
  assert.equal(findSession(SESSIONS, 'aaaaaaaa-5').name, 'two');
});

test('findSession rejects ambiguous or too-short prefixes', () => {
  assert.equal(findSession(SESSIONS, 'aaaaaaaa'), null); // ambiguous
  assert.equal(findSession(SESSIONS, 'bbbbb'), null); // < 8 chars
  assert.equal(findSession(SESSIONS, 'zzzzzzzzzz'), null);
  assert.equal(findSession([], 'whatever'), null);
});

// ------------------------------------------------------------------ input validation

test('validateSendText enforces non-empty string <= 8000 chars', () => {
  assert.equal(validateSendText('hello').ok, true);
  assert.equal(validateSendText('multi\nline\ntext').ok, true, 'newlines are allowed');
  assert.equal(validateSendText('').ok, false);
  assert.equal(validateSendText('   ').ok, false);
  assert.equal(validateSendText(42).ok, false);
  assert.equal(validateSendText(undefined).ok, false);
  assert.equal(validateSendText('x'.repeat(8000)).ok, true);
  assert.equal(validateSendText('x'.repeat(8001)).ok, false);
});

test('validateKey allows only Enter and Escape', () => {
  assert.equal(validateKey('Enter').ok, true);
  assert.equal(validateKey('Escape').ok, true);
  assert.equal(validateKey('escape').ok, false);
  assert.equal(validateKey('C-c').ok, false);
  assert.equal(validateKey(undefined).ok, false);
});

// ------------------------------------------------------------------ static paths

const PUB = '/srv/fleet-web/public';

test('resolveStaticPath maps / to index.html', () => {
  assert.deepEqual(resolveStaticPath(PUB, '/'), { ok: true, file: path.join(PUB, 'index.html') });
  assert.deepEqual(resolveStaticPath(PUB, '/app.js'), { ok: true, file: path.join(PUB, 'app.js') });
  assert.deepEqual(resolveStaticPath(PUB, '/sub/dir/x.css'), { ok: true, file: path.join(PUB, 'sub/dir/x.css') });
});

test('resolveStaticPath rejects traversal, encoded traversal and null bytes', () => {
  for (const p of [
    '/../server.mjs',
    '/foo/../../server.mjs',
    '/%2e%2e/server.mjs',
    '/%2e%2e%2fserver.mjs',
    '/a/%2e%2e/%2e%2e/etc/passwd',
  ]) {
    const r = resolveStaticPath(PUB, p);
    assert.equal(r.ok, false, `expected rejection for ${p}`);
    assert.equal(r.status, 400);
  }
  assert.equal(resolveStaticPath(PUB, '/a\0b').ok, false);
  assert.equal(resolveStaticPath(PUB, '/%ZZ').ok, false);
  assert.equal(resolveStaticPath(PUB, 'relative').ok, false);
});

test('resolveStaticPath keeps files with dots in the name', () => {
  assert.equal(resolveStaticPath(PUB, '/manifest.webmanifest').ok, true);
  assert.equal(resolveStaticPath(PUB, '/a..b.js').ok, true);
});

test('contentTypeFor covers the formats the UI ships', () => {
  assert.match(contentTypeFor('x.html'), /^text\/html/);
  assert.match(contentTypeFor('x.js'), /javascript/);
  assert.match(contentTypeFor('x.css'), /^text\/css/);
  assert.match(contentTypeFor('x.json'), /^application\/json/);
  assert.match(contentTypeFor('x.webmanifest'), /manifest\+json/);
  assert.equal(contentTypeFor('x.svg'), 'image/svg+xml');
  assert.equal(contentTypeFor('x.png'), 'image/png');
  assert.equal(contentTypeFor('x.bin'), 'application/octet-stream');
});

// ------------------------------------------------------------------ body limit

function fakeRequest(chunks) {
  const stream = new PassThrough();
  queueMicrotask(() => {
    for (const c of chunks) stream.write(c);
    stream.end();
  });
  stream.destroy = () => stream.end();
  return stream;
}

test('readJsonBody parses an object body', async () => {
  const body = await readJsonBody(fakeRequest([JSON.stringify({ text: 'hi' })]));
  assert.deepEqual(body, { text: 'hi' });
});

test('readJsonBody treats an empty body as {}', async () => {
  assert.deepEqual(await readJsonBody(fakeRequest([''])), {});
});

test('readJsonBody rejects invalid JSON and non-objects with 400', async () => {
  await assert.rejects(() => readJsonBody(fakeRequest(['{nope'])), (e) => e.status === 400);
  await assert.rejects(() => readJsonBody(fakeRequest(['[1,2]'])), (e) => e.status === 400);
  await assert.rejects(() => readJsonBody(fakeRequest(['"str"'])), (e) => e.status === 400);
});

test('readJsonBody enforces the 64 KB limit with 413', async () => {
  const big = 'x'.repeat(70 * 1024);
  await assert.rejects(
    () => readJsonBody(fakeRequest([big])),
    (e) => e instanceof HttpError && e.status === 413,
  );
});

test('readJsonBody accepts a body just under the limit', async () => {
  const payload = JSON.stringify({ text: 'y'.repeat(60 * 1024) });
  const body = await readJsonBody(fakeRequest([payload]));
  assert.equal(body.text.length, 60 * 1024);
});

// ------------------------------------------------------------------ backends (mocked run)

function recorder(impl) {
  const calls = [];
  const run = async (file, args, opts) => {
    calls.push({ file, args, opts });
    return impl ? impl(file, args, opts) : { stdout: '', stderr: '' };
  };
  return { calls, run };
}

const noSleep = async () => {};
const script = () => '/tmp/fake.applescript';

test('tmux peek uses capture-pane -p -J and tails the output', async () => {
  const { calls, run } = recorder(async () => ({ stdout: 'l1\nl2\nl3\n\n\n', stderr: '' }));
  const backend = createBackend({ run, tmux: '/opt/homebrew/bin/tmux', scriptPath: script, sleep: noSleep });
  const text = await backend.peek({ backend: 'tmux', handle: '%87' }, 30);
  assert.deepEqual(calls[0].args, ['capture-pane', '-p', '-J', '-t', '%87', '-S', '-30']);
  assert.equal(text, 'l1\nl2\nl3');
});

test('the backend has no way to type into a session: typing goes through the CLI and its lock', () => {
  const backend = createBackend({ run: async () => ({ stdout: '', stderr: '' }), scriptPath: script });
  assert.deepEqual(Object.keys(backend).sort(), ['closeIterm', 'peek']);
  assert.doesNotMatch(ITERM_SCRIPT, /write text/);
});

test('iterm backend passes values as argv, never interpolated', async () => {
  const { calls, run } = recorder(async () => ({ stdout: 'hello\nworld\n', stderr: '' }));
  const backend = createBackend({ run, scriptPath: script, sleep: noSleep });
  const session = { backend: 'iterm', handle: 'ABC-123' };
  await backend.peek(session, 10);
  await backend.closeIterm({ backend: 'iterm', handle: 'x"; do shell script "y' });
  assert.deepEqual(calls.map((c) => c.args), [
    ['/tmp/fake.applescript', 'peek', 'ABC-123'],
    ['/tmp/fake.applescript', 'close', 'x"; do shell script "y'],
  ]);
  assert.ok(calls.every((c) => c.file === '/usr/bin/osascript'));
});

test('iterm closeIterm runs the close mode with the session id as argv', async () => {
  const { calls, run } = recorder(async () => ({ stdout: 'ok', stderr: '' }));
  const backend = createBackend({ run, scriptPath: script, sleep: noSleep });
  await backend.closeIterm({ backend: 'iterm', handle: 'ABC-123' });
  assert.deepEqual(calls.at(-1).args, ['/tmp/fake.applescript', 'close', 'ABC-123']);
  assert.match(ITERM_SCRIPT, /theMode is "close"/);
  await assert.rejects(() => backend.closeIterm({ backend: 'iterm', handle: '' }), (e) => e.status === 409);
});

test('unknown backend and missing handle are 409', async () => {
  const { run } = recorder();
  const backend = createBackend({ run, scriptPath: script, sleep: noSleep });
  await assert.rejects(
    () => backend.peek({ backend: 'unknown', handle: 'x' }, 10),
    (e) => e instanceof BackendError && e.status === 409,
  );
  await assert.rejects(
    () => backend.peek({ backend: 'tmux', handle: '' }, 10),
    (e) => e instanceof BackendError && e.status === 409,
  );
});

test('createScriptProvider writes the AppleScript once and reuses it', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-web-test-'));
  try {
    const provider = createScriptProvider({ tmpdir: dir });
    const a = provider();
    const b = provider();
    assert.equal(a, b);
    assert.equal(fs.readFileSync(a, 'utf8'), ITERM_SCRIPT);
    assert.match(ITERM_SCRIPT, /on findSession\(theId\)/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// ------------------------------------------------------------------ fleet discovery

const FAKE_LIST = JSON.stringify([
  { session_id: 'a', status: 'idle', updated_at: 2 },
  { session_id: 'b', status: 'waiting', updated_at: 1 },
]);

test('fleet sorts, tags host and caches for the TTL', async () => {
  let clock = 1000;
  let calls = 0;
  const run = async () => {
    calls += 1;
    return { stdout: FAKE_LIST, stderr: '' };
  };
  const fleet = createFleet({ cli: createFleetCli({ run }), self: 'laptop', ttlMs: 2000, now: () => clock });
  const first = await fleet.localHost();
  assert.equal(first.ok, true);
  assert.equal(first.name, 'laptop');
  assert.deepEqual(first.sessions.map((s) => s.session_id), ['b', 'a']);
  assert.ok(first.sessions.every((s) => s.host === 'laptop'));

  clock += 500;
  await fleet.localHost();
  assert.equal(calls, 1, 'served from cache');

  clock += 2500;
  await fleet.localHost();
  assert.equal(calls, 2, 'cache expired');
});

test('trimTitle caps long first prompts for transport', () => {
  assert.equal(trimTitle('short'), 'short');
  assert.equal(trimTitle(null), null);
  assert.equal(trimTitle(undefined), null);
  const long = trimTitle('x'.repeat(TITLE_MAX + 50));
  assert.equal(long.length, TITLE_MAX + 1);
  assert.ok(long.endsWith('…'));
});

test('fleet trims session titles', async () => {
  const run = async () => ({ stdout: JSON.stringify([{ session_id: 'a', status: 'idle', title: 'y'.repeat(5000) }]), stderr: '' });
  const host = await createFleet({ cli: createFleetCli({ run }), self: 'laptop' }).localHost();
  assert.equal(host.sessions[0].title.length, TITLE_MAX + 1);
});

test('fleet passes context usage through untouched', async () => {
  const context = { used: 124000, window: 200000, pct: 62, model: 'claude-x' };
  const rows = [
    { session_id: 'a', status: 'idle', context },
    { session_id: 'b', status: 'idle', context: null },
  ];
  const run = async () => ({ stdout: JSON.stringify(rows), stderr: '' });
  const host = await createFleet({ cli: createFleetCli({ run }), self: 'laptop' }).localHost();
  const byId = Object.fromEntries(host.sessions.map((s) => [s.session_id, s]));
  assert.deepEqual(byId.a.context, context);
  assert.equal(byId.b.context, null);
});

test('fleet CLI is invoked as `<bin> list --json`', async () => {
  const calls = [];
  const cli = createFleetCli({
    bin: '/opt/fleet/bin/fleet',
    run: async (file, args) => {
      calls.push([file, ...args]);
      return { stdout: '[]', stderr: '' };
    },
  });
  assert.deepEqual(await cli.list(), []);
  assert.deepEqual(calls, [['/opt/fleet/bin/fleet', 'list', '--json']]);
});

test('fleet de-duplicates concurrent discovery', async () => {
  let calls = 0;
  const run = async () => {
    calls += 1;
    await new Promise((r) => setTimeout(r, 10));
    return { stdout: FAKE_LIST, stderr: '' };
  };
  const fleet = createFleet({ cli: createFleetCli({ run }), self: 'laptop' });
  await Promise.all([fleet.localHost(), fleet.localHost(), fleet.localHost()]);
  assert.equal(calls, 1);
});

test('fleet never throws: garbled output, non-array and child errors', async () => {
  const garbled = createFleet({ cli: createFleetCli({ run: async () => ({ stdout: 'not json', stderr: '' }) }), self: 'l' });
  const g = await garbled.localHost();
  assert.equal(g.ok, false);
  assert.match(g.error, /non-JSON/);

  const notArray = createFleet({ cli: createFleetCli({ run: async () => ({ stdout: '{"a":1}', stderr: '' }) }), self: 'l' });
  assert.equal((await notArray.localHost()).ok, false);

  const boom = createFleet({
    cli: createFleetCli({
      bin: '/nope/fleet',
      run: async () => {
        throw Object.assign(new Error('spawn ENOENT'), { code: 'ENOENT' });
      },
    }),
    self: 'l',
  });
  const b = await boom.localHost();
  assert.equal(b.ok, false);
  assert.match(b.error, /fleet binary not found \(\/nope\/fleet\)/);
  assert.deepEqual(b.sessions, []);

  const timedOut = createFleet({
    cli: createFleetCli({
      run: async () => {
        throw Object.assign(new Error('killed'), { killed: true });
      },
    }),
    self: 'l',
  });
  assert.match((await timedOut.localHost()).error, /timed out/);

  const empty = createFleet({ cli: createFleetCli({ run: async () => ({ stdout: '   ', stderr: '' }) }), self: 'l' });
  const e = await empty.localHost();
  assert.equal(e.ok, true);
  assert.deepEqual(e.sessions, []);
});

// ------------------------------------------------------------------ path

test('ensurePath prepends the homebrew/local bins once', () => {
  const env = { PATH: '/usr/bin:/bin' };
  ensurePath(env);
  assert.match(env.PATH, /^\/opt\/homebrew\/bin:/);
  const once = env.PATH;
  ensurePath(env);
  assert.equal(env.PATH, once, 'idempotent');
});

test('pathOnly reads FLEET_PATH_ONLY the way the CLI does', () => {
  for (const v of ['1', 'true', 'YES', ' yes ']) assert.equal(pathOnly({ FLEET_PATH_ONLY: v }), true, v);
  for (const v of [undefined, '', '0', 'no', 'false']) assert.equal(pathOnly({ FLEET_PATH_ONLY: v }), false, String(v));
});

// The server reaches past PATH (ensurePath, the iTerm osascript), so under FLEET_PATH_ONLY it
// refuses to start: exit 2 before reading the config (an unreadable one here would fail
// differently) or listening.
test('the server refuses to start under FLEET_PATH_ONLY', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-web-path-only-'));
  const config = path.join(dir, 'config.json');
  fs.writeFileSync(config, 'not json');
  const server = new URL('../server.mjs', import.meta.url).pathname;
  const r = spawnSync(process.execPath, [server], {
    env: { PATH: '/usr/bin:/bin', HOME: dir, FLEET_CONFIG: config, FLEET_PATH_ONLY: '1' },
    encoding: 'utf8',
    timeout: 10_000,
  });
  fs.rmSync(dir, { recursive: true, force: true });
  assert.equal(r.status, 2, r.stderr);
  assert.match(r.stderr, /not available with FLEET_PATH_ONLY set/);
});

test('ensureUtf8Locale gives children a UTF-8 locale, keeping one that is already set', () => {
  assert.equal(ensureUtf8Locale({}).LC_CTYPE, 'en_US.UTF-8');
  assert.equal(ensureUtf8Locale({ LANG: 'hr_HR.UTF-8' }).LC_CTYPE, undefined);
  assert.equal(ensureUtf8Locale({ LC_ALL: 'C' }).LC_ALL, 'en_US.UTF-8');
});

test('etagMatches: exact, weak/strong, lists and *', async () => {
  const { etagMatches } = await import('../lib/app.mjs');
  const tag = 'W/"1a-2b"';
  assert.equal(etagMatches(tag, tag), true);
  assert.equal(etagMatches('"1a-2b"', tag), true);
  assert.equal(etagMatches('W/"x", W/"1a-2b"', tag), true);
  assert.equal(etagMatches('*', tag), true);
  assert.equal(etagMatches('W/"other"', tag), false);
  assert.equal(etagMatches(undefined, tag), false);
});
