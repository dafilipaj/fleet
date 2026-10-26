// Slack links: link parsing, the bridge (link → intro, messages only from the linked thread,
// delivered to an idle session, replies only while linked and only by the linked session,
// expiry), the Socket Mode client's acks and the /api/slack routes. Slack is a fake.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import {
  createSlackApi,
  createSlackBridge,
  createSocketClient,
  linkPrompt,
  messagesPrompt,
  parseMinutes,
  parseSlackUrl,
  plainText,
} from '../lib/slack.mjs';

const SID = 'aaaaaaaa-0000-0000-0000-000000000001';
const OTHER = 'bbbbbbbb-0000-0000-0000-000000000002';
const THREAD = '1700000000.123456';
const URL_ = 'https://team.slack.com/archives/C0123ABCDEF/p1700000000123456';

function fakeSlack() {
  const calls = [];
  let n = 0;
  const handlers = {
    'auth.test': () => ({ user_id: 'UBOT', bot_id: 'B1', team: 'Team' }),
    'users.info': ({ user }) => ({ user: { profile: { display_name: { U1: 'Alex', UOWNER: 'Owner' }[user] ?? '' }, name: user } }),
    'conversations.replies': () => ({
      messages: [
        { ts: THREAD, user: 'UOWNER', text: 'Demo of <https://x.test|the thing>' },
        { ts: '1792777200.000001', thread_ts: THREAD, user: 'U1', text: 'How does it work?' },
        { ts: '1792777201.000001', thread_ts: THREAD, bot_id: 'B9', text: 'a bot' },
      ],
    }),
    'chat.getPermalink': () => ({ permalink: `${URL_}?thread_ts=${THREAD}` }),
    'chat.postMessage': () => ({ ts: `1792779999.${String(++n).padStart(6, '0')}` }),
    'reactions.add': () => ({}),
  };
  const call = async (method, params = {}) => {
    calls.push({ method, params });
    const h = handlers[method];
    if (!h) throw Object.assign(new Error(`unexpected ${method}`), { status: 500 });
    return { ok: true, ...h(params) };
  };
  return { call, calls, handlers };
}

function fixture(t, { status = 'idle' } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-slack-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const slack = fakeSlack();
  const sessions = [
    { session_id: SID, name: 'artifacts-demo', status, handle: '%5', backend: 'tmux' },
    { session_id: OTHER, name: 'other', status: 'idle', handle: '%6', backend: 'tmux' },
  ];
  const sent = [];
  let clock = 1_800_000_000_000;
  const bridge = createSlackBridge({
    settings: { owner: 'UOWNER' },
    call: slack.call,
    listSessions: async () => sessions,
    send: async (s, text) => void sent.push({ id: s.session_id, text }),
    file: path.join(dir, 'slack-links.json'),
    now: () => clock,
  });
  return { bridge, slack, sessions, sent, dir, advance: (ms) => (clock += ms) };
}

test('parseSlackUrl: message, reply and channel links', () => {
  assert.deepEqual(parseSlackUrl(URL_), { channel: 'C0123ABCDEF', threadTs: THREAD });
  assert.deepEqual(parseSlackUrl(`https://team.slack.com/archives/C0123ABCDEF/p1792777999000001?thread_ts=${THREAD}&cid=C0123ABCDEF`), { channel: 'C0123ABCDEF', threadTs: THREAD });
  assert.deepEqual(parseSlackUrl('https://team.slack.com/archives/D0123ABCDEF'), { channel: 'D0123ABCDEF', threadTs: null });
  assert.throws(() => parseSlackUrl('https://evil.test/archives/C0123ABCDEF'), /not a Slack link/);
  assert.throws(() => parseSlackUrl('https://team.slack.com/messages/x'), /not a Slack message or channel link/);
  assert.throws(() => parseSlackUrl('nope'), /not a Slack link/);
});

test('the Web API is called form-encoded (read methods reject JSON bodies)', async () => {
  let seen = null;
  const call = createSlackApi({
    getToken: async () => 'xoxb-test',
    fetch: async (url, opts) => {
      seen = { url, opts };
      return { status: 200, json: async () => ({ ok: true, messages: [] }) };
    },
  });
  await call('conversations.replies', { channel: 'C0123ABCDEF', ts: THREAD, limit: 31, unfurl_links: false, skip: undefined });
  assert.equal(seen.url, 'https://slack.com/api/conversations.replies');
  assert.equal(seen.opts.headers['content-type'], 'application/x-www-form-urlencoded');
  assert.equal(seen.opts.headers.authorization, 'Bearer xoxb-test');
  assert.equal(seen.opts.body, `channel=C0123ABCDEF&ts=${THREAD}&limit=31&unfurl_links=false`);
  const failing = createSlackApi({ getToken: async () => 'x', fetch: async () => ({ status: 200, json: async () => ({ ok: false, error: 'invalid_arguments' }) }) });
  await assert.rejects(failing('auth.test'), (err) => err.slack === 'invalid_arguments' && err.status === 502);
});

test('parseMinutes', () => {
  assert.equal(parseMinutes(30), 30);
  assert.equal(parseMinutes('45'), 45);
  assert.equal(parseMinutes('2h'), 120);
  assert.equal(parseMinutes('1h30m'), 90);
  assert.equal(parseMinutes('soon'), null);
  assert.equal(parseMinutes(''), null);
});

test('plainText: mentions, links and entities', () => {
  assert.equal(plainText('hi <@U1> see <https://a.test|docs> &amp; <https://b.test>', { U1: 'Alex' }), 'hi @Alex see docs (https://a.test) & https://b.test');
});

test('link: reads the thread, types the intro into the idle session, then messages with 👀', async (t) => {
  const fx = fixture(t);
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: '30m', task: 'artifact testing', host: 'workstation' });
  assert.match(link.id, /^sl-[0-9a-f]{6}$/);
  assert.equal(link.threadTs, THREAD);
  assert.equal(link.permalink, `${URL_}?thread_ts=${THREAD}`);

  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1);
  const intro = fx.sent[0].text;
  assert.match(intro, new RegExp(`^\\[slack ${link.id}\\] You are linked to a Slack thread until`));
  assert.match(intro, /Task you may help with there: artifact testing\./);
  assert.match(intro, new RegExp(`fleet slack reply ${link.id}`));
  assert.match(intro, /Owner \(UOWNER, the owner of this session\)/);
  assert.match(intro, /Alex \(U1\)[^\n]*\n\s*How does it work\?/);
  assert.doesNotMatch(intro, /a bot/);

  // A message in another thread, an edit, the bot itself, an old one: none delivered.
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778000.000001', thread_ts: '1792770000.000001', user: 'U1', text: 'elsewhere' });
  await fx.bridge.handleEvent({ type: 'message', subtype: 'message_changed', channel: 'C0123ABCDEF', ts: '1792778000.000002', thread_ts: THREAD });
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778000.000003', thread_ts: THREAD, user: 'UBOT', text: 'me' });
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792777200.000001', thread_ts: THREAD, user: 'U1', text: 'How does it work?' });
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778000.000004', thread_ts: THREAD, user: 'U1', text: 'Is <@UBOT> there?' });
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778000.000005', thread_ts: THREAD, user: 'U1', text: 'And this?' });
  assert.equal((await fx.bridge.list()).links[0].pending, 2);

  // Delivered together, after the gap that lets the session show as busy.
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1);
  fx.advance(10_000);
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 2);
  assert.match(fx.sent[1].text, /Alex \(U1\) at \d\d:\d\d:\n\s*Is @UBOT there\?/);
  assert.match(fx.sent[1].text, /And this\?/);
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(fx.slack.calls.filter((c) => c.method === 'reactions.add').map((c) => c.params.timestamp), ['1792778000.000004', '1792778000.000005']);
  assert.equal((await fx.bridge.list()).links[0].pending, 0);
});

test('delivery waits while the session is busy or waiting', async (t) => {
  const fx = fixture(t, { status: 'busy' });
  await fx.bridge.link({ session: SID, url: URL_, minutes: 30, host: 'h' });
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 0);
  fx.sessions[0].status = 'waiting';
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 0);
  fx.sessions[0].status = 'idle';
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1);
});

test('reply: posts in the thread, only for the linked session, only while linked', async (t) => {
  const fx = fixture(t);
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: 5, host: 'h' });
  const r = await fx.bridge.reply({ link: link.id, text: 'Bok! Here to help.', pane: '%5' });
  assert.equal(r.threadTs, THREAD);
  const post = fx.slack.calls.findLast((c) => c.method === 'chat.postMessage');
  assert.deepEqual(post.params, { channel: 'C0123ABCDEF', text: 'Bok! Here to help.', thread_ts: THREAD, unfurl_links: false, unfurl_media: false });

  await assert.rejects(fx.bridge.reply({ link: link.id, text: 'hi', pane: '%6' }), { status: 403 });
  await assert.rejects(fx.bridge.reply({ link: link.id, text: '  ' }), { status: 400 });
  await assert.rejects(fx.bridge.reply({ link: 'sl-000000', text: 'hi' }), { status: 404 });
  await assert.rejects(fx.bridge.reply({ link: 'nope', text: 'hi' }), { status: 400 });

  fx.advance(6 * 60 * 1000);
  await assert.rejects(fx.bridge.reply({ link: link.id, text: 'late' }), { status: 410 });
  const { links, ended } = await fx.bridge.list();
  assert.equal(links.length, 0);
  assert.equal(ended[0].endReason, 'expired');
  // It had replied, so the thread is told it left.
  assert.match(fx.slack.calls.findLast((c) => c.method === 'chat.postMessage').params.text, /left the conversation/);
});

test('expiry: messages after the end are not delivered; a link never used leaves quietly', async (t) => {
  const fx = fixture(t, { status: 'busy' });
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: 1, host: 'h' });
  fx.advance(61_000);
  await fx.bridge.tick();
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778000.000009', thread_ts: THREAD, user: 'U1', text: 'late' });
  fx.sessions[0].status = 'idle';
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 0);
  assert.equal((await fx.bridge.list()).ended[0].id, link.id);
  assert.equal(fx.slack.calls.filter((c) => c.method === 'chat.postMessage').length, 0);
});

test('link: validation, renewal, unlink, and a conversation the app cannot see', async (t) => {
  const fx = fixture(t);
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: 0, host: 'h' }), /minutes must be/);
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: 'forever', host: 'h' }), /minutes must be/);
  await assert.rejects(fx.bridge.link({ session: 'cccccccc-0000', url: URL_, minutes: 5, host: 'h' }), { status: 404 });
  await assert.rejects(fx.bridge.link({ url: URL_, minutes: 5, host: 'h' }), /session \(or pane\) is required/);

  const a = await fx.bridge.link({ pane: '%5', url: URL_, minutes: 5, host: 'h' });
  const b = await fx.bridge.link({ session: SID, url: URL_, minutes: 60, task: 'new task', host: 'h' });
  assert.equal(b.id, a.id);
  assert.equal(b.renewed, true);
  assert.equal(b.task, 'new task');
  assert.equal((await fx.bridge.list()).links.length, 1);

  assert.equal((await fx.bridge.unlink(a.id)).endReason, 'unlinked');
  await assert.rejects(fx.bridge.unlink(a.id), { status: 404 });

  fx.slack.handlers['conversations.replies'] = () => {
    throw Object.assign(new Error('Slack conversations.replies: not_in_channel'), { slack: 'not_in_channel', status: 502 });
  };
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: 5, host: 'h' }), /invite it there first/);
});

test('a channel link: every thread in it, replies take --thread', async (t) => {
  const fx = fixture(t);
  fx.slack.handlers['conversations.history'] = () => ({ messages: [] });
  const link = await fx.bridge.link({ session: SID, url: 'https://team.slack.com/archives/D0123ABCDEF', minutes: 5, host: 'h' });
  assert.equal(link.threadTs, null);
  await fx.bridge.tick();
  assert.match(fx.sent[0].text, /a Slack conversation/);
  assert.match(fx.sent[0].text, /--thread <ts>/);
  await fx.bridge.handleEvent({ type: 'message', channel: 'D0123ABCDEF', ts: '1792778000.000001', user: 'U1', text: 'top' });
  fx.advance(10_000);
  await fx.bridge.tick();
  assert.match(fx.sent[1].text, /thread 1792778000\.000001/);
  await fx.bridge.reply({ link: link.id, text: 'answer', thread: '1792778000.000001' });
  assert.equal(fx.slack.calls.findLast((c) => c.method === 'chat.postMessage').params.thread_ts, '1792778000.000001');
  await fx.bridge.reply({ link: link.id, text: 'top-level' });
  assert.equal(fx.slack.calls.findLast((c) => c.method === 'chat.postMessage').params.thread_ts, undefined);
});

test('prompts stay under the send cap', () => {
  const link = { id: 'sl-abcdef', threadTs: THREAD, until: 1_800_000_000_000, permalink: URL_, task: '', owner: null };
  const many = Array.from({ length: 50 }, (_, i) => ({ ts: `17927772${String(i).padStart(2, '0')}.000001`, threadTs: THREAD, user: 'U1', text: 'x'.repeat(2000), files: [] }));
  assert.ok(linkPrompt(link, { history: many }).length < 8000);
  assert.match(linkPrompt(link, { history: many }), /earlier messages left out/);
  assert.ok(messagesPrompt(link, many).length < 8000);
});

test('socket client: opens, acks every envelope, hands events on, reconnects on disconnect', async () => {
  const sockets = [];
  class FakeWs {
    constructor(url) {
      this.url = url;
      this.sent = [];
      this.listeners = {};
      sockets.push(this);
    }
    addEventListener(type, fn) {
      this.listeners[type] = fn;
    }
    send(data) {
      this.sent.push(JSON.parse(data));
    }
    close() {
      this.closed = true;
    }
    emit(msg) {
      this.listeners.message({ data: JSON.stringify(msg) });
    }
  }
  const events = [];
  let opens = 0;
  const client = createSocketClient({
    getAppToken: async () => 'xapp-test',
    onEvent: (e) => events.push(e),
    fetch: async (url, opts) => {
      assert.equal(url, 'https://slack.com/api/apps.connections.open');
      assert.equal(opts.headers.authorization, 'Bearer xapp-test');
      opens++;
      return { json: async () => ({ ok: true, url: `wss://x.test/${opens}` }) };
    },
    WebSocketImpl: FakeWs,
  });
  client.start();
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(sockets.length, 1);
  sockets[0].emit({ type: 'hello' });
  assert.equal(client.status().connected, true);
  sockets[0].emit({ type: 'events_api', envelope_id: 'e1', payload: { event: { type: 'message', text: 'hi' } } });
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(sockets[0].sent, [{ envelope_id: 'e1' }]);
  assert.deepEqual(events, [{ type: 'message', text: 'hi' }]);
  sockets[0].emit({ type: 'disconnect', reason: 'refresh_requested' });
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(sockets[0].closed, true);
  assert.equal(sockets.length, 2);
  client.stop();
  assert.equal(sockets[1].closed, true);
});

test('HTTP: /api/slack on the bridge host, proxied elsewhere, off without slack.host', async (t) => {
  const fx = fixture(t);
  const servers = {};
  const start = async (k, raw, slack) => {
    const config = normalizeConfig(raw, { env: {}, home: '/home/tester' });
    const fleet = { localHost: async () => ({ name: raw.self, ok: true, sessions: fx.sessions }) };
    const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, slack, warmFleet: false });
    t.after(() => api.stop());
    const server = createHttpServer({ handleApi: api, uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    t.after(() => server.close());
    servers[k] = `http://127.0.0.1:${server.address().port}`;
  };
  await start('bridge', { self: 'workstation', slack: { host: 'workstation', owner: 'UOWNER' } }, fx.bridge);
  await start('peer', { self: 'laptop', hosts: { workstation: { web: servers.bridge } }, slack: { host: 'workstation' } }, null);
  await start('off', { self: 'laptop' }, null);
  const call = async (u, method = 'GET', body) => {
    const res = await fetch(u, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
    return { status: res.status, body: await res.json() };
  };

  const off = await call(`${servers.off}/api/slack`);
  assert.equal(off.body.enabled, false);
  assert.equal((await call(`${servers.off}/api/slack/link`, 'POST', {})).status, 409);

  const linked = await call(`${servers.peer}/api/slack/link`, 'POST', { host: 'workstation', session: SID, url: URL_, minutes: 30 });
  assert.equal(linked.status, 200, JSON.stringify(linked.body));
  assert.equal((await call(`${servers.bridge}/api/slack/link`, 'POST', { host: 'laptop', session: SID, url: URL_, minutes: 30 })).status, 400);
  const status = await call(`${servers.peer}/api/slack`);
  assert.equal(status.body.enabled, true);
  assert.equal(status.body.links[0].id, linked.body.id);
  assert.equal((await call(`${servers.bridge}/api/slack/reply`, 'POST', { link: linked.body.id, text: 'hi' })).status, 200);
  assert.equal((await call(`${servers.bridge}/api/slack/link`, 'POST', { session: SID, url: 'https://x.test', minutes: 30 })).status, 400);
  assert.equal((await call(`${servers.peer}/api/slack/links/${linked.body.id}/unlink`, 'POST', {})).status, 200);
  assert.equal((await call(`${servers.bridge}/api/slack/links/bad/unlink`, 'POST', {})).status, 400);
  assert.equal((await call(`${servers.bridge}/api/slack/reply`, 'GET')).status, 405);
});
