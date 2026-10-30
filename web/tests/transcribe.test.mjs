// Voice prompts: key lookup (env → Keychain), request validation and the OpenAI call (fetch mocked).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';

import { KEY_NAME, audioType, buildPrompt, createKeySource, createTranscriber, promptEcho } from '../lib/transcribe.mjs';

function audioReq(body, type = 'audio/webm;codecs=opus') {
  const req = Readable.from([Buffer.from(body)]);
  req.headers = { 'content-type': type, 'content-length': String(Buffer.byteLength(body)) };
  return req;
}

function okFetch(calls, text = 'Pozdrav, fleet.') {
  return async (url, init) => {
    calls.push({ url, init });
    return new Response(JSON.stringify({ text }), { status: 200, headers: { 'content-type': 'application/json' } });
  };
}

test('audioType keeps known audio types, drops codecs and anything else', () => {
  assert.equal(audioType('audio/webm;codecs=opus'), 'audio/webm');
  assert.equal(audioType('Audio/MP4'), 'audio/mp4');
  assert.equal(audioType('application/json'), null);
  assert.equal(audioType(undefined), null);
});

test('buildPrompt appends the hint on one line, capped', () => {
  assert.match(buildPrompt(''), /latinica/);
  assert.match(buildPrompt('fix-auth,\n  api'), /latinica\. .* fix-auth, api$/);
  assert.ok(buildPrompt('x'.repeat(5000)).length < 700);
});

test('createKeySource: env first, then the Keychain; a hit is cached, a miss retried after a minute', async () => {
  assert.equal(await createKeySource({ env: { [KEY_NAME]: ' sk-env ' }, run: () => assert.fail('no keychain') })(), 'sk-env');

  let calls = 0;
  let t = 0;
  let stored = null;
  const run = async (bin, args) => {
    calls++;
    assert.equal(bin, 'security');
    assert.deepEqual(args, ['find-generic-password', '-s', KEY_NAME, '-w']);
    if (!stored) throw new Error('not found');
    return { stdout: `${stored}\n` };
  };
  const getKey = createKeySource({ env: {}, platform: 'darwin', run, now: () => t });
  assert.equal(await getKey(), null);
  assert.equal(await getKey(), null);
  assert.equal(calls, 1, 'a miss is cached');
  stored = 'sk-keychain';
  t = 61 * 1000;
  assert.equal(await getKey(), 'sk-keychain');
  assert.equal(await getKey(), 'sk-keychain');
  assert.equal(calls, 2, 'a hit is cached');

  assert.equal(await createKeySource({ env: {}, platform: 'linux', run })(), null);
});

test('transcribe sends model, prompt and the file to OpenAI and returns the text', async () => {
  const calls = [];
  const tr = createTranscriber({ getKey: async () => 'sk-test', fetch: okFetch(calls) });
  assert.equal(await tr.enabled(), true);
  const out = await tr.transcribe(audioReq('RIFFfake'), 'my-session, fleet');
  assert.deepEqual(out, { text: 'Pozdrav, fleet.', model: 'gpt-4o-transcribe' });
  assert.equal(calls.length, 1);
  const { url, init } = calls[0];
  assert.equal(url, 'https://api.openai.com/v1/audio/transcriptions');
  assert.equal(init.headers.authorization, 'Bearer sk-test');
  const form = init.body;
  assert.equal(form.get('model'), 'gpt-4o-transcribe');
  assert.match(form.get('prompt'), /my-session, fleet$/);
  const file = form.get('file');
  assert.equal(file.name, 'voice.webm');
  assert.equal(file.type, 'audio/webm');
  assert.equal(await file.text(), 'RIFFfake');
});

test('promptEcho catches the prompt coming back instead of speech', () => {
  const prompt = buildPrompt('operator, fix-auth-login, api-rate-limits, web-dark-mode, em-weekly');
  // The prompt itself, whole or cut off.
  assert.ok(promptEcho(prompt, prompt));
  assert.ok(promptEcho(prompt.slice(0, 200), prompt));
  // Prose strung from its session names.
  assert.ok(promptEcho('Treba poboljšati fix-auth-login, a zatim api-rate-limits i web-dark-mode.', prompt));
  // Real speech, naming a session or two.
  assert.equal(promptEcho('Pogledaj fix-auth-login i javi što je s api-rate-limits.', prompt), false);
  assert.equal(promptEcho('Claude', prompt), false);
  assert.equal(promptEcho('', prompt), false);
});

test('transcribe returns no text for a prompt echo', async () => {
  const logs = [];
  const echo = buildPrompt('my-session, other-session');
  const tr = createTranscriber({ getKey: async () => 'sk-test', fetch: okFetch([], echo), log: (m) => logs.push(m) });
  assert.deepEqual(await tr.transcribe(audioReq('RIFFfake'), 'my-session, other-session'), { text: '', model: 'gpt-4o-transcribe' });
  assert.match(logs[0], /prompt echo/);
});

test('a recording that fails to transcribe is kept in keepDir', async () => {
  const keepDir = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-voice-')), 'failed');
  const fail = async () => new Response(JSON.stringify({ error: { message: 'Audio file might be corrupted or unsupported' } }), { status: 400 });
  const tr = createTranscriber({ getKey: async () => 'sk-test', fetch: fail, keepDir, now: () => new Date('2026-10-30T08:30:00Z') });
  await assert.rejects(tr.transcribe(audioReq('RIFFlong'), ''), (err) => err.status === 502 && /kept on the server/.test(err.message));
  assert.deepEqual(fs.readdirSync(keepDir), ['2026-10-30T08-30-00-000Z.webm']);
  assert.equal(fs.readFileSync(path.join(keepDir, '2026-10-30T08-30-00-000Z.webm'), 'utf8'), 'RIFFlong');
});

test('transcribe refuses without a key, non-audio, empty and oversized bodies', async () => {
  const noKey = createTranscriber({ getKey: async () => null, fetch: () => assert.fail('no call') });
  assert.equal(await noKey.enabled(), false);
  await assert.rejects(noKey.transcribe(audioReq('x'), ''), { status: 501 });

  const tr = createTranscriber({ getKey: async () => 'sk', maxBytes: 4, fetch: () => assert.fail('no call') });
  await assert.rejects(tr.transcribe(audioReq('x', 'text/plain'), ''), { status: 415 });
  await assert.rejects(tr.transcribe(audioReq(''), ''), { status: 400 });
  await assert.rejects(tr.transcribe(audioReq('too long'), ''), { status: 413 });
});

test('upstream errors become 502 (429 stays 429), never a 401 the UI would misread', async () => {
  const failing = (status) => async () => new Response(JSON.stringify({ error: { message: 'Incorrect API key provided' } }), { status });
  const tr401 = createTranscriber({ getKey: async () => 'sk', fetch: failing(401) });
  await assert.rejects(tr401.transcribe(audioReq('abc'), ''), (err) => err.status === 502 && /Incorrect API key/.test(err.message));
  const tr429 = createTranscriber({ getKey: async () => 'sk', fetch: failing(429) });
  await assert.rejects(tr429.transcribe(audioReq('abc'), ''), { status: 429 });
  const down = createTranscriber({ getKey: async () => 'sk', fetch: async () => { throw new Error('ECONNRESET'); } });
  await assert.rejects(down.transcribe(audioReq('abc'), ''), { status: 502 });
});
