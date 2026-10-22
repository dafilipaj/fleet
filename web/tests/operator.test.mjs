// The voice operator: spoken replies (speechText, the OpenAI speech call, fetch mocked), the
// operator's workspace and find-or-start, and the /api/speak + /api/operator routes.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';

import { createSpeaker, speechText, SPEAK_MAX_CHARS } from '../lib/transcribe.mjs';
import { OPERATOR_NAME, apiBase, apiScript, createOperator, operatorBrief } from '../lib/operator.mjs';
import { createApi } from '../lib/api.mjs';
import { normalizeConfig } from '../lib/config.mjs';

test('speechText drops what cannot be read aloud and unwraps markdown', () => {
  const md = [
    '## Stanje',
    '',
    '- **fleet** radi na `operator` ekranu',
    '- vidi [PR](https://github.com/x/y/pull/1) i https://example.com/a',
    '',
    '```js',
    'const x = 1',
    '```',
    '| a | b |',
    '|---|---|',
    'id aaaaaaaa-0000-0000-0000-000000000001 gotov',
  ].join('\n');
  assert.equal(speechText(md), 'Stanje\nfleet radi na operator ekranu\nvidi PR i\nid gotov');
  assert.equal(speechText(''), '');
  const long = speechText('riječ '.repeat(2000));
  assert.ok(long.length <= SPEAK_MAX_CHARS + 1 && long.endsWith('…'));
});

test('speak posts model, voice and the spoken text; errors map like transcribe', async () => {
  const calls = [];
  const fetch = async (url, init) => (calls.push({ url, init }), new Response(Buffer.from('ID3mp3'), { status: 200 }));
  const sp = createSpeaker({ getKey: async () => 'sk-test', voice: 'nova', fetch });
  assert.equal(await sp.enabled(), true);
  const audio = await sp.speak('**Dvije** sesije te čekaju.');
  assert.equal(audio.toString(), 'ID3mp3');
  assert.equal(calls[0].url, 'https://api.openai.com/v1/audio/speech');
  assert.equal(calls[0].init.headers.authorization, 'Bearer sk-test');
  const body = JSON.parse(calls[0].init.body);
  assert.equal(body.voice, 'nova');
  assert.equal(body.input, 'Dvije sesije te čekaju.');
  assert.equal(body.response_format, 'mp3');

  await assert.rejects(createSpeaker({ getKey: async () => null, fetch }).speak('x'), { status: 501 });
  await assert.rejects(sp.speak('```\ncode\n```'), { status: 400 });
  const err = (status) => async () => new Response(JSON.stringify({ error: { message: 'nope' } }), { status });
  await assert.rejects(createSpeaker({ getKey: async () => 'k', fetch: err(401) }).speak('hi'), (e) => e.status === 502 && /nope/.test(e.message));
  await assert.rejects(createSpeaker({ getKey: async () => 'k', fetch: err(429) }).speak('hi'), { status: 429 });
});

test('apiBase: loopback for a wildcard bind, the address otherwise', () => {
  assert.equal(apiBase({ bind: '0.0.0.0', port: 7777 }), 'http://127.0.0.1:7777/api');
  assert.equal(apiBase({ bind: '127.0.0.1', port: 8 }), 'http://127.0.0.1:8/api');
  assert.equal(apiBase({ bind: '192.0.2.10', port: 8 }), 'http://192.0.2.10:8/api');
  assert.equal(apiBase({ bind: '::1', port: 8 }), 'http://[::1]:8/api');
  assert.match(apiScript('http://127.0.0.1:1/api'), /"http:\/\/127\.0\.0\.1:1\/api\$route"/);
  assert.match(operatorBrief({ self: 'laptop', hosts: ['laptop', 'workstation'] }), /`laptop`, `workstation`/);
});

function tmpConfig(t, extra = {}) {
  const state = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-operator-'));
  t.after(() => fs.rmSync(state, { recursive: true, force: true }));
  return normalizeConfig({ self: 'laptop', web: { operator: { model: 'claude-sonnet-5', ...extra } } }, { env: { XDG_STATE_HOME: state }, home: '/home/tester' });
}

test('operator: prepare writes the workspace, ensure starts it once, then finds it', async (t) => {
  const config = tmpConfig(t);
  assert.equal(config.operator.model, 'claude-sonnet-5');
  const dir = config.operator.dir;
  let sessions = [];
  const spawns = [];
  const fleet = { localHost: async () => ({ ok: true, sessions }) };
  const spawner = { spawn: async (req) => (spawns.push(req), { tmuxSession: req.name }) };
  const op = createOperator({ config, fleet, spawner });

  assert.deepEqual(await op.status(), { host: 'laptop', dir, session: null });
  const r = await op.ensure();
  assert.deepEqual(r, { host: 'laptop', dir, session: null, started: true, tmuxSession: OPERATOR_NAME });
  assert.deepEqual(spawns, [{ name: OPERATOR_NAME, dir, prompt: '', nameGiven: true, model: 'claude-sonnet-5', harness: 'claude' }]);
  assert.match(fs.readFileSync(path.join(dir, 'CLAUDE.md'), 'utf8'), /Fleet operator/);
  assert.ok(JSON.parse(fs.readFileSync(path.join(dir, '.claude', 'settings.json'), 'utf8')).permissions.allow.includes('Bash(./api:*)'));
  assert.ok(fs.statSync(path.join(dir, 'api')).mode & 0o100, 'the api helper is executable');

  sessions = [
    { session_id: 'x', cwd: '/elsewhere', harness: 'claude' },
    { session_id: 'op', cwd: dir, harness: 'claude', tmux_session: 'operator' },
  ];
  const again = await op.ensure();
  assert.equal(again.started, false);
  assert.equal(again.session.session_id, 'op');
  assert.equal(spawns.length, 1);
});

test('config: web.operator is validated', () => {
  const norm = (operator) => normalizeConfig({ self: 'a', web: { operator } }, { env: {}, home: '/h' });
  assert.equal(norm(undefined).operator.voice, null);
  assert.equal(norm({ voice: 'nova' }).operator.voice, 'nova');
  assert.throws(() => norm({ voice: 'no spaces' }), /voice/);
  assert.throws(() => norm({ model: 'rm -rf' }), /model/);
  assert.throws(() => norm('x'), /object/);
});

test('routes: /api/speak streams mp3, /api/operator finds or starts, settings report both', async (t) => {
  const config = tmpConfig(t);
  const fleet = { localHost: async () => ({ ok: true, sessions: [] }), invalidate() {} };
  const spawner = { spawn: async (req) => ({ tmuxSession: req.name }) };
  const speaker = { enabled: async () => true, voice: 'alloy', speak: async (text) => Buffer.from(`mp3:${text}`) };
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner, speaker, operator: createOperator({ config, fleet, spawner }), warmFleet: false });
  const jsonReq = (method, body) => Object.assign(Readable.from(body ? [Buffer.from(JSON.stringify(body))] : []), { method, headers: { 'content-type': 'application/json' } });

  const settings = await api(jsonReq('GET'), new URL('http://x/api/settings'));
  assert.deepEqual(settings.body.speak, { enabled: true, voice: 'alloy' });
  assert.deepEqual(settings.body.operator, { enabled: true, hosts: ['laptop'] });

  const spoken = await api(jsonReq('POST', { text: 'bok' }), new URL('http://x/api/speak'));
  assert.equal(spoken.headers['content-type'], 'audio/mpeg');
  const chunks = [];
  for await (const c of spoken.stream) chunks.push(c);
  assert.equal(Buffer.concat(chunks).toString(), 'mp3:bok');
  await assert.rejects(api(jsonReq('POST', { text: ' ' }), new URL('http://x/api/speak')), { status: 400 });

  const started = await api(jsonReq('POST', {}), new URL('http://x/api/operator'));
  assert.equal(started.body.started, true);
  const status = await api(jsonReq('GET'), new URL('http://x/api/operator'));
  assert.equal(status.body.session, null);

  const bare = createApi({ config, fleet, backend: {}, transcripts: {}, spawner, warmFleet: false });
  await assert.rejects(bare(jsonReq('POST', { text: 'x' }), new URL('http://x/api/speak')), { status: 501 });
  await assert.rejects(bare(jsonReq('GET'), new URL('http://x/api/operator')), { status: 501 });
});

test('createKeySource reads the key it is named for', async () => {
  const { createKeySource, SPEECH_KEY_NAME } = await import('../lib/transcribe.mjs');
  const env = { [SPEECH_KEY_NAME]: 'sk-tts', OPENAI_SPEECH_TO_TEXT_API_KEY: 'sk-stt' };
  assert.equal(await createKeySource({ name: SPEECH_KEY_NAME, env, platform: 'linux' })(), 'sk-tts');
  assert.equal(await createKeySource({ env, platform: 'linux' })(), 'sk-stt');
});

test('operator: stamp marks its row; tick clears an operator idle for an hour, once', async (t) => {
  const config = tmpConfig(t, { clearIdleMinutes: 60 });
  const dir = config.operator.dir;
  let t0 = 10 * 3600 * 1000;
  const op = { session_id: 'op-0000-1111', cwd: dir, harness: 'claude', status: 'idle', updated_at: t0, context: { used: 5000 } };
  const sessions = [{ session_id: 'x', cwd: '/w' }, op];
  const sent = [];
  const operator = createOperator({
    config,
    fleet: { localHost: async () => ({ ok: true, sessions }) },
    spawner: {},
    cli: { send: async ({ target, text }) => void sent.push([target, text]) },
    now: () => t0,
  });
  await operator.prepare();
  const stamped = operator.stamp({ name: 'laptop', sessions });
  assert.deepEqual(stamped.sessions.map((s) => s.operator ?? false), [false, true]);
  assert.equal(operator.stamp({ sessions: [sessions[0]] }).sessions[0].operator, undefined);

  assert.equal(await operator.tick(), null, 'not idle long enough');
  t0 += 61 * 60 * 1000;
  op.status = 'busy';
  assert.equal(await operator.tick(), null, 'busy');
  op.status = 'idle';
  assert.equal(await operator.tick(), 'op-0000-1111');
  assert.deepEqual(sent, [['id:op-0000-1111', '/clear']]);
  // Claude Code starts a new session: no context until it is used.
  Object.assign(op, { session_id: 'op-2222', context: null, updated_at: t0 });
  t0 += 2 * 3600 * 1000;
  assert.equal(await operator.tick(), null, 'already empty');
  assert.equal(sent.length, 1);
});

test('operator: tick never clears a `shell` operator (a background Bash is running), only plain idle', async (t) => {
  const config = tmpConfig(t, { clearIdleMinutes: 60 });
  const t0 = 10 * 3600 * 1000;
  const op = { session_id: 'op-0000-1111', cwd: config.operator.dir, harness: 'claude', status: 'shell', updated_at: t0 - 2 * 3600 * 1000, context: { used: 5000 } };
  const sent = [];
  const operator = createOperator({
    config,
    fleet: { localHost: async () => ({ ok: true, sessions: [op] }) },
    spawner: {},
    cli: { send: async ({ target, text }) => void sent.push([target, text]) },
    now: () => t0,
  });
  await operator.prepare();
  assert.equal(await operator.tick(), null, 'shell is left alone');
  assert.deepEqual(sent, []);
  op.status = 'idle';
  assert.equal(await operator.tick(), 'op-0000-1111', 'control: the same session, plain idle');
  assert.deepEqual(sent, [['id:op-0000-1111', '/clear']]);
});

test('/api/operator: the first answering host of web.operator.hosts', async (t) => {
  const state = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-operator-'));
  t.after(() => fs.rmSync(state, { recursive: true, force: true }));
  const config = normalizeConfig(
    { self: 'officeshed', hosts: { laptop: { web: 'http://laptop:1' }, homeshed: { web: 'http://homeshed:1' } }, web: { operator: { hosts: ['laptop', 'homeshed', 'officeshed'] } } },
    { env: { XDG_STATE_HOME: state }, home: '/home/tester' },
  );
  const calls = [];
  const proxyToPeer = async (base, opts) => {
    calls.push([base, opts.method, opts.pathname]);
    if (base === 'http://laptop:1') return { status: 502, body: { error: 'unreachable' } };
    return { status: 200, body: { host: 'homeshed', session: { session_id: 'h' } } };
  };
  const fleet = { localHost: async () => ({ ok: true, sessions: [] }), invalidate() {} };
  const operator = createOperator({ config, fleet, spawner: {} });
  const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, operator, proxyToPeer, warmFleet: false });
  const req = (method) => Object.assign(Readable.from([]), { method, headers: {} });

  const r = await api(req('GET'), new URL('http://x/api/operator'));
  assert.equal(r.body.host, 'homeshed');
  assert.deepEqual(calls.map((c) => c[0]), ['http://laptop:1', 'http://homeshed:1']);
  const local = await api(req('GET'), new URL('http://x/api/operator?local=1'));
  assert.equal(local.body.host, 'officeshed');
  const s = await api(req('GET'), new URL('http://x/api/settings'));
  assert.deepEqual(s.body.operator.hosts, ['laptop', 'homeshed', 'officeshed']);
});
