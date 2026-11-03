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
  createSlackDownload,
  SlackError,
  createSocketClient,
  linkPrompt,
  matchPeople,
  messagesPrompt,
  parseMinutes,
  parsePerson,
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
    'users.info': ({ user }) => ({ user: { profile: { display_name: { U1: 'Alex', UOWNER: 'Owner' }[user] ?? '' }, name: user, id: user } }),
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

function fixture(t, { status = 'idle', download = null, settings = { owner: 'UOWNER' }, spawn = null, self = null } = {}) {
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
    settings,
    self,
    call: slack.call,
    listSessions: async () => sessions,
    send: async (s, text) => void sent.push({ id: s.session_id, text }),
    spawn,
    file: path.join(dir, 'slack-links.json'),
    download,
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
  assert.equal(parseMinutes('2d'), 2880);
  assert.equal(parseMinutes('1d12h'), 2160);
  assert.equal(parseMinutes('1w'), 10080);
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
  assert.match(intro, /The owner's instructions for it: artifact testing/);
  assert.match(intro, /a thread you have posted in[^]*not necessarily for you/);
  assert.match(fx.sent[0].text, /Stay free to talk[^]*background subagent/);
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
  assert.match(fx.sent[1].text, /Alex \(U1\) at \d\d:\d\d → mentions you:\nIs @you there\?/);
  assert.match(fx.sent[1].text, /And this\?/);
  await new Promise((r) => setImmediate(r));
  // 👀 only on what was addressed to it.
  assert.deepEqual(fx.slack.calls.filter((c) => c.method === 'reactions.add').map((c) => c.params.timestamp), ['1792778000.000004']);
  assert.equal((await fx.bridge.list()).links[0].pending, 0);
});

const msg = (n, text, extra = {}) => ({ type: 'message', channel: 'C0123ABCDEF', ts: `1792778100.00000${n}`, thread_ts: THREAD, user: 'U1', text, ...extra });

test('mention mode: chatter is kept as context, a mention wakes the session with it, then answers follow', async (t) => {
  const fx = fixture(t);
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: 60, host: 'h' });
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1);
  fx.advance(10_000);
  await fx.bridge.handleEvent(msg(1, 'just chatting'));
  await fx.bridge.handleEvent(msg(2, 'more chatter'));
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1, 'nothing for it yet');
  await fx.bridge.handleEvent(msg(3, '<@UBOT> what broke?'));
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 2);
  assert.match(fx.sent[1].text, /something for you/);
  assert.match(fx.sent[1].text, /just chatting[\s\S]*more chatter[\s\S]*→ mentions you:\n@you what broke\?/);

  // Once it has posted, every message in its thread wakes it — unmarked: whether it is for it is
  // its own call — however long after.
  await fx.bridge.reply({ link: link.id, text: 'Looking.' });
  fx.advance(60 * 60 * 1000 - 30_000);
  await fx.bridge.handleEvent(msg(4, 'thanks, and the other one?'));
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 3);
  assert.match(fx.sent[2].text, /in a thread you posted in — answer only if it is for you/);
  assert.match(fx.sent[2].text, /:\nthanks, and the other one\?/);
  assert.doesNotMatch(fx.sent[2].text, /→/);
});

test('proactive mode: every message goes in, marked as not addressed', async (t) => {
  const fx = fixture(t);
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: 60, mode: 'proactive', task: 'jump in when the demo breaks', host: 'h' });
  assert.equal(link.mode, 'proactive');
  await fx.bridge.tick();
  assert.match(fx.sent[0].text, /Step in when you can really help/);
  assert.match(fx.sent[0].text, /jump in when the demo breaks/);
  fx.advance(10_000);
  await fx.bridge.handleEvent(msg(1, 'the page is blank'));
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 2);
  assert.match(fx.sent[1].text, /step in only if you should/);
  assert.equal(fx.slack.calls.filter((c) => c.method === 'reactions.add').length, 0);
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: 60, mode: 'loud', host: 'h' }), /mode must be one of/);
});

test('a session on another host: listed and typed into on its host; replies only from there', async (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-slack-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const slack = fakeSlack();
  const byHost = {
    workstation: [{ session_id: OTHER, name: 'local', status: 'idle', handle: '%5', backend: 'tmux' }],
    laptop: [{ session_id: SID, name: 'remote', status: 'idle', handle: '%5', backend: 'tmux' }],
  };
  const asked = [];
  const sent = [];
  const bridge = createSlackBridge({
    settings: { owner: 'UOWNER' },
    self: 'workstation',
    call: slack.call,
    listSessions: async (host) => (asked.push(host), byHost[host] ?? null),
    send: async (s, text, host) => void sent.push({ id: s.session_id, host, text }),
    file: path.join(dir, 'slack-links.json'),
  });
  // Not on the bridge's host: found by its pane on its own host.
  await assert.rejects(bridge.link({ pane: '%9', url: URL_, minutes: 30, host: 'laptop' }), /on laptop/);
  const link = await bridge.link({ pane: '%5', url: URL_, minutes: 30, host: 'laptop' });
  assert.equal(link.host, 'laptop');
  assert.equal(link.session, SID);
  await bridge.tick();
  assert.equal(sent.length, 1);
  assert.deepEqual([sent[0].id, sent[0].host], [SID, 'laptop']);
  assert.ok(asked.includes('laptop'));
  // The same pane id on the bridge's host is another session: refused.
  await assert.rejects(bridge.reply({ link: link.id, text: 'hi', pane: '%5', host: 'workstation' }), (err) => err.status === 403);
  const r = await bridge.reply({ link: link.id, text: 'hi', pane: '%5', host: 'laptop' });
  assert.equal(r.ok, true);
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
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: 'always', host: 'h' }), /minutes must be/);
  await assert.rejects(fx.bridge.link({ session: SID, url: URL_, minutes: '1w1m', host: 'h' }), /minutes must be 1–10080 .*or forever/);
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

test('a channel link in mention mode: the threads it posted in wake it, others stay context', async (t) => {
  const fx = fixture(t);
  fx.slack.handlers['conversations.history'] = () => ({ messages: [] });
  const link = await fx.bridge.link({ session: SID, url: 'https://team.slack.com/archives/C0123ABCDEF', minutes: 60, host: 'h' });
  await fx.bridge.tick();
  const said = (ts, text, thread_ts) => fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts, user: 'U1', text, ...(thread_ts ? { thread_ts } : {}) });
  await fx.bridge.reply({ link: link.id, text: 'answer', thread: '1792778000.000001' });
  const top = (await fx.bridge.reply({ link: link.id, text: 'top-level' })).ts;
  fx.advance(10_000);
  await said('1792778100.000001', 'elsewhere', '1792778050.000001');
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 1, 'a thread it never posted in is context');
  await said('1792778200.000001', 'on its top-level post', top);
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 2);
  assert.match(fx.sent[1].text, /answer only if it is for you[\s\S]*elsewhere[\s\S]*on its top-level post/);
  fx.advance(10_000);
  await said('1792778300.000001', 'in the answered thread', '1792778000.000001');
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 3);
});

const MEMBERS = [
  { id: 'U1', name: 'alex', real_name: 'Alex Šimić', profile: { display_name: 'Alex', real_name: 'Alex Šimić' } },
  { id: 'U2', name: 'alexandra', real_name: 'Alexandra Kos', profile: { display_name: '', real_name: 'Alexandra Kos' } },
  { id: 'U0SAM0001', name: 'sam', real_name: 'Sam Lee', profile: { display_name: 'Sam', real_name: 'Sam Lee' } },
  { id: 'U4', name: 'gone', real_name: 'Gone Person', deleted: true, profile: {} },
  { id: 'UB', name: 'somebot', is_bot: true, profile: {} },
];

test('parsePerson / matchPeople: ids, names, exact before prefix, no deleted or bots', () => {
  assert.deepEqual(parsePerson('U0123ABC'), { id: 'U0123ABC' });
  assert.deepEqual(parsePerson('<@U0123ABC|alex>'), { id: 'U0123ABC' });
  assert.deepEqual(parsePerson(' @Alex Šimić '), { name: 'alex simic' });
  assert.throws(() => parsePerson('@'), /name the person/);
  assert.deepEqual(matchPeople(MEMBERS, 'alex').map((u) => u.id), ['U1']);
  assert.deepEqual(matchPeople(MEMBERS, 'simic').map((u) => u.id), ['U1']);
  assert.deepEqual(matchPeople(MEMBERS, 'al').map((u) => u.id), ['U1', 'U2']);
  assert.deepEqual(matchPeople(MEMBERS, 'gone'), []);
  assert.deepEqual(matchPeople(MEMBERS, 'somebot'), []);
});

test('a DM link: a person instead of a URL — the app\'s DM with them, every message for the session', async (t) => {
  const fx = fixture(t);
  fx.slack.handlers['users.list'] = ({ cursor }) => (cursor ? { members: MEMBERS.slice(2) } : { members: MEMBERS.slice(0, 2), response_metadata: { next_cursor: 'p2' } });
  fx.slack.handlers['auth.test'] = () => ({ user_id: 'UBOT', bot_id: 'B1', team: 'Team', url: 'https://team.slack.com/' });
  fx.slack.handlers['conversations.open'] = ({ users }) => ({ channel: { id: `D0${users}XYZ` } });
  fx.slack.handlers['conversations.history'] = () => ({ messages: [] });

  await assert.rejects(fx.bridge.link({ session: SID, url: '@al', minutes: 30, host: 'h' }), /matches 2 people — name one: Alex Šimić \(@alex, U1\); Alexandra Kos/);
  await assert.rejects(fx.bridge.link({ session: SID, url: '@nobody', minutes: 30, host: 'h' }), (e) => e.status === 404);
  const link = await fx.bridge.link({ session: SID, url: '@Sam', minutes: 30, task: 'greet Sam and explain the demo', host: 'h' });
  assert.equal(link.channel, 'D0U0SAM0001XYZ');
  assert.equal(link.threadTs, null);
  assert.deepEqual(link.dm, { user: 'U0SAM0001', name: 'Sam' });
  assert.equal(link.permalink, 'https://team.slack.com/team/U0SAM0001');
  assert.deepEqual(fx.slack.calls.find((c) => c.method === 'conversations.open').params, { users: 'U0SAM0001' });

  await fx.bridge.tick();
  assert.match(fx.sent[0].text, /linked to a Slack DM with Sam/);
  assert.match(fx.sent[0].text, /if the instructions say to start, post a short opener now/);
  assert.doesNotMatch(fx.sent[0].text, /--thread/);
  fx.advance(10_000);
  await fx.bridge.handleEvent({ type: 'message', channel: 'D0U0SAM0001XYZ', channel_type: 'im', ts: '1792778000.000001', user: 'U0SAM0001', text: 'hi, what is this?' });
  await fx.bridge.tick();
  assert.equal(fx.sent.length, 2);
  assert.match(fx.sent[1].text, /New in the DM with Sam — something for you/);
  assert.match(fx.sent[1].text, /Sam \(U0SAM0001\) at \d\d:\d\d → to you:\nhi, what is this\?/);

  // By id, again: the same DM — renewed.
  const again = await fx.bridge.link({ session: SID, user: 'U0SAM0001', minutes: 60, host: 'h' });
  assert.equal(again.renewed, true);
});

test('a DM link without the im:write scope says so', async (t) => {
  const fx = fixture(t);
  fx.slack.handlers['conversations.open'] = () => {
    throw Object.assign(new Error('missing_scope'), { slack: 'missing_scope' });
  };
  await assert.rejects(fx.bridge.link({ session: SID, user: 'U0ALEX001', minutes: 30, host: 'h' }), /im:write/);
});

test('prompts stay under the send cap', () => {
  const link = { id: 'sl-abcdef', threadTs: THREAD, until: 1_800_000_000_000, permalink: URL_, task: '', owner: null };
  const many = Array.from({ length: 50 }, (_, i) => ({ ts: `17927772${String(i).padStart(2, '0')}.000001`, threadTs: THREAD, user: 'U1', text: 'x'.repeat(2000), files: [] }));
  assert.ok(linkPrompt(link, { history: many }).length < 8000);
  assert.match(linkPrompt(link, { history: many }), /earlier messages left out/);
  assert.ok(messagesPrompt(link, many).length < 8000);
  // A link that ends another day says which.
  assert.match(linkPrompt({ ...link, createdAt: link.until - 2 * 24 * 3600 * 1000 }), /until [A-Z][a-z]{2} \d{1,2} [A-Z][a-z]{2} \d\d:\d\d:/);
  assert.match(linkPrompt({ ...link, createdAt: link.until - 60 * 1000 }), /until \d\d:\d\d:/);
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
  const again = await call(`${servers.peer}/api/slack/links/${linked.body.id}/relink`, 'POST', { minutes: 'forever' });
  assert.equal(again.status, 200, JSON.stringify(again.body));
  assert.equal(again.body.until, null);
  assert.equal((await call(`${servers.bridge}/api/slack/links/sl-000000/relink`, 'POST', {})).status, 404);
  assert.equal((await call(`${servers.bridge}/api/slack/reply`, 'GET')).status, 405);
});

test('history / thread / file: the linked session reads its own conversation, a page at a time', async (t) => {
  const got = [];
  const fx = fixture(t, { download: async (u) => (got.push(u), { bytes: Buffer.from('PNGDATA'), type: 'image/png' }) });
  const CH = 'C0123ABCDEF';
  const TOP = '1792777100.000001';
  fx.slack.handlers['conversations.history'] = (p) => ({
    messages: p.cursor
      ? [{ ts: '1792777000.000001', user: 'U1', text: 'older' }]
      : [
          { ts: TOP, user: 'U1', text: 'plan in <@UOWNER>\'s doc', reply_count: 3, latest_reply: '1792777150.000001', files: [{ id: 'F0FILE00001', name: 'plan.png', mimetype: 'image/png', size: 7 }] },
          { ts: '1792777090.000001', bot_id: 'B1', user: 'UBOT', text: 'from the app' },
        ],
    response_metadata: { next_cursor: p.cursor ? '' : 'CUR1' },
  });
  fx.slack.handlers['files.info'] = ({ file }) => {
    if (file === 'F0ELSEWHERE') return { file: { id: file, name: 'x', shares: { public: { COTHER0000: [{ ts: TOP }] } }, url_private_download: 'https://files.slack.com/x' } };
    return { file: { id: file, name: 'plan.png', mimetype: 'image/png', size: 7, shares: { public: { [CH]: [{ ts: TOP }] } }, url_private_download: 'https://files.slack.com/files-pri/T1-F0FILE00001/download/plan.png' } };
  };
  const ch = await fx.bridge.link({ session: SID, url: `https://team.slack.com/archives/${CH}`, minutes: 60 });

  const page = await fx.bridge.history({ link: ch.id, session: SID });
  assert.equal(page.order, 'newest first');
  assert.equal(page.nextCursor, 'CUR1');
  const [top, own] = page.messages;
  assert.equal(top.name, 'Alex');
  assert.equal(top.text, "plan in @Owner's doc");
  assert.equal(top.replies, 3);
  assert.deepEqual(top.files, [{ id: 'F0FILE00001', name: 'plan.png', mimetype: 'image/png', size: 7 }]);
  assert.equal(own.you, true);
  const older = await fx.bridge.history({ link: ch.id, session: SID, cursor: 'CUR1', limit: 10 });
  assert.equal(older.messages[0].text, 'older');
  assert.equal(older.nextCursor, null);
  assert.equal(fx.slack.calls.filter((c) => c.method === 'conversations.history').at(-1).params.limit, 10);
  await assert.rejects(fx.bridge.history({ link: ch.id, limit: 500 }), /limit must be/);
  await assert.rejects(fx.bridge.history({ link: ch.id, oldest: 'yesterday' }), /oldest must be a Slack timestamp/);

  // Another session may not read this link.
  await assert.rejects(fx.bridge.history({ link: ch.id, session: OTHER }), (e) => e.status === 403);

  const th = await fx.bridge.thread({ link: ch.id, ts: TOP, session: SID });
  assert.equal(fx.slack.calls.at(-1).params.ts, TOP);
  assert.equal(th.order, 'oldest first');
  await assert.rejects(fx.bridge.thread({ link: ch.id }), /ts is required/);

  const f = await fx.bridge.file({ link: ch.id, file: 'F0FILE00001', session: SID });
  assert.equal(Buffer.from(f.data, 'base64').toString(), 'PNGDATA');
  assert.equal(f.name, 'plan.png');
  assert.match(got[0], /^https:\/\/files\.slack\.com\//);
  await assert.rejects(fx.bridge.file({ link: ch.id, file: 'F0ELSEWHERE' }), (e) => e.status === 403);
  await assert.rejects(fx.bridge.file({ link: ch.id, file: '../etc' }), /file must be a Slack file id/);

  // A thread link: history is its thread; another thread or a file shared elsewhere in the channel is refused.
  const tl = await fx.bridge.link({ session: OTHER, url: URL_, minutes: 60 });
  const own2 = await fx.bridge.history({ link: tl.id, session: OTHER });
  assert.equal(own2.order, 'oldest first');
  assert.equal(fx.slack.calls.at(-1).method, 'conversations.replies');
  assert.equal(fx.slack.calls.at(-1).params.ts, THREAD);
  await assert.rejects(fx.bridge.thread({ link: tl.id, ts: TOP }), (e) => e.status === 403);

  // Errors Slack gives are said plainly; an ended link reads nothing.
  fx.slack.handlers['conversations.history'] = () => {
    throw Object.assign(new SlackError('Slack conversations.history: ratelimited', 502), { slack: 'ratelimited' });
  };
  await assert.rejects(fx.bridge.history({ link: ch.id }), (e) => e.status === 429);
  await fx.bridge.unlink(ch.id);
  await assert.rejects(fx.bridge.history({ link: ch.id }), (e) => e.status === 410);
});

test('a file needs a download (files:read): none set up says so', async (t) => {
  const fx = fixture(t);
  const l = await fx.bridge.link({ session: SID, url: URL_, minutes: 60 });
  await assert.rejects(fx.bridge.file({ link: l.id, file: 'F0FILE00001' }), (e) => e.status === 501);
});

test('createSlackDownload: only https on slack.com, with the bot token, within the size cap', async () => {
  const seen = [];
  const fetch = async (u, o) => (seen.push([String(u), o.headers.authorization]), { ok: true, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer, headers: new Map([['content-type', 'image/png']]) });
  const dl = createSlackDownload({ getToken: async () => 'xoxb-1', fetch, maxBytes: 2 });
  await assert.rejects(dl('https://evil.test/x'), /not a Slack file URL/);
  await assert.rejects(dl('http://files.slack.com/x'), /not a Slack file URL/);
  await assert.rejects(dl('https://files.slack.com/x'), (e) => e.status === 413);
  assert.deepEqual(seen, [['https://files.slack.com/x', 'Bearer xoxb-1']]);
  const ok = await createSlackDownload({ getToken: async () => 'xoxb-1', fetch })('https://files.slack.com/x');
  assert.deepEqual([...ok.bytes], [1, 2, 3]);
});

test('two bridges: slack.bridges sends a host\'s sessions to its own bridge; status, unlink and relink span both', async (t) => {
  const home = fixture(t);
  const studio = fixture(t);
  const start = async (raw, slack, sessions) => {
    const config = normalizeConfig(raw, { env: {}, home: '/home/tester' });
    const fleet = { localHost: async () => ({ name: raw.self, ok: true, sessions }) };
    const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, slack, warmFleet: false });
    t.after(() => api.stop());
    const server = createHttpServer({ handleApi: api, uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    t.after(() => server.close());
    return `http://127.0.0.1:${server.address().port}`;
  };
  const studioUrl = await start({ self: 'studio', slack: { host: 'studio', owner: 'UOWNER' } }, studio.bridge, studio.sessions);
  const routes = { host: 'workstation', bridges: { studio: 'studio' } };
  const ws = await start({ self: 'workstation', hosts: { studio: { web: studioUrl } }, slack: routes }, home.bridge, home.sessions);
  const laptop = await start({ self: 'laptop', hosts: { workstation: { web: ws }, studio: { web: studioUrl } }, slack: routes }, null, []);
  const old = await start({ self: 'old', hosts: { workstation: { web: ws } }, slack: { host: 'workstation' } }, null, []);
  const call = async (u, method = 'GET', body) => {
    const res = await fetch(u, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
    return { status: res.status, body: await res.json() };
  };
  const read = (fx) => fx.slack.calls.filter((c) => c.method === 'conversations.replies').length;

  // A studio session: linked by the studio bridge (its own app), not the default one.
  const s = await call(`${laptop}/api/slack/link`, 'POST', { host: 'studio', session: SID, url: URL_, minutes: 30 });
  assert.equal(s.status, 200, JSON.stringify(s.body));
  assert.equal(read(studio), 1);
  assert.equal(read(home), 0);
  const w = await call(`${laptop}/api/slack/link`, 'POST', { host: 'workstation', session: SID, url: URL_, minutes: 30 });
  assert.equal(w.status, 200, JSON.stringify(w.body));
  assert.equal(read(home), 1);

  // A server that still sends everything to slack.host: refused, not linked twice.
  const twice = await call(`${old}/api/slack/link`, 'POST', { host: 'studio', session: SID, url: URL_, minutes: 30 });
  assert.equal(twice.status, 400);
  assert.match(twice.body.error, /linked by the bridge on studio/);
  assert.equal(read(home), 1);

  // Status: both bridges' links, wherever you look; each bridge's own view stays its own.
  const st = await call(`${laptop}/api/slack`);
  assert.equal(st.body.host, 'workstation');
  assert.deepEqual(st.body.links.map((l) => [l.id, l.host]).sort(), [[s.body.id, 'studio'], [w.body.id, 'workstation']].sort());
  assert.deepEqual(st.body.bridges.map((b) => [b.host, b.enabled]), [['workstation', true], ['studio', true]]);
  assert.equal((await call(`${studioUrl}/api/slack`)).body.links.length, 1);

  // Unlink / relink by id: the bridge that has it, from any server.
  assert.equal((await call(`${laptop}/api/slack/links/${s.body.id}/unlink`, 'POST', {})).status, 200);
  assert.equal((await call(`${studioUrl}/api/slack`)).body.ended[0].id, s.body.id);
  const again = await call(`${laptop}/api/slack/links/${s.body.id}/relink`, 'POST', {});
  assert.equal(again.status, 200, JSON.stringify(again.body));
  assert.equal((await call(`${laptop}/api/slack/links/sl-000000/unlink`, 'POST', {})).status, 404);
  // The studio session replies through its own server: its own bridge.
  assert.equal((await call(`${studioUrl}/api/slack/reply`, 'POST', { link: again.body.id, text: 'hi' })).status, 200);
  const posts = (fx) => fx.slack.calls.filter((c) => c.method === 'chat.postMessage' && c.params.text === 'hi').length;
  assert.equal(posts(studio), 1);
  assert.equal(posts(home), 0);
  // A read, the same way: the studio session's own server, its own bridge.
  const h = await call(`${studioUrl}/api/slack/history`, 'POST', { link: again.body.id });
  assert.equal(h.status, 200, JSON.stringify(h.body));
  assert.equal(h.body.messages[1].name, 'Alex');
  assert.equal((await call(`${studioUrl}/api/slack/history`, 'GET')).status, 405);
});

test('a spawn linked to Slack: no prompt at launch, linked once it registers, the prompt ends the intro', async (t) => {
  const fx = fixture(t);
  const NEW = 'cccccccc-0000-0000-0000-000000000003';
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-spawn-slack-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const spawned = [];
  const spawner = { spawn: async (o) => (spawned.push(o), { name: o.name, dir: o.dir, tmuxSession: o.name, command: 'claude', trusted: false }) };
  const start = async (raw, slack) => {
    const config = normalizeConfig({ spawnDirs: [dir], ...raw }, { env: {}, home: '/home/tester' });
    const fleet = { localHost: async () => ({ name: raw.self, ok: true, sessions: fx.sessions }) };
    // The session registers while the spawn waits: first unlisted, then listed without an id, then with one.
    let waits = 0;
    const sleep = async () => {
      waits += 1;
      if (waits === 1) fx.sessions.push({ session_id: null, name: 'job', status: 'idle', handle: '%7', backend: 'tmux', tmux_session: 'job' });
      else fx.sessions.at(-1).session_id = NEW;
    };
    const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner, slack, warmFleet: false, sleep });
    t.after(() => api.stop());
    const server = createHttpServer({ handleApi: api, uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    t.after(() => server.close());
    return `http://127.0.0.1:${server.address().port}`;
  };
  const bridge = await start({ self: 'workstation', slack: { host: 'workstation' } }, fx.bridge);
  const peer = await start({ self: 'laptop', slack: { host: 'workstation' } }, null);
  const post = async (base, host, body) => {
    const res = await fetch(`${base}/api/hosts/${host}/spawn`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    return { status: res.status, body: await res.json() };
  };

  // Refused before anything opens: a host that cannot reach the bridge, a bad link, a bad duration.
  assert.equal((await post(peer, 'laptop', { name: 'job', dir, prompt: 'x', slack: { url: URL_, minutes: 30 } })).status, 400);
  assert.equal((await post(bridge, 'workstation', { name: 'job', dir, prompt: 'x', slack: { url: 'https://x.test', minutes: 30 } })).status, 400);
  assert.equal((await post(bridge, 'workstation', { name: 'job', dir, prompt: 'x', slack: { url: URL_, minutes: 0 } })).status, 400);
  assert.equal(spawned.length, 0);

  assert.equal((await post(bridge, 'workstation', { name: 'job', dir, prompt: 'x', slack: { url: URL_, minutes: 30, from: 'alex' } })).status, 400);
  const r = await post(bridge, 'workstation', { name: 'job', dir, prompt: 'Help Alex with this', slack: { url: URL_, minutes: 60, mode: 'proactive' } });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(spawned[0].prompt, '');
  assert.equal(r.body.slack.session, NEW);
  assert.equal(r.body.slack.mode, 'proactive');

  await fx.bridge.tick();
  const intro = fx.sent.find((s) => s.id === NEW).text;
  assert.match(intro, /^\[slack sl-[0-9a-f]{6}\] You are linked to a Slack thread/);
  assert.match(intro, /How does it work\?[^]*Their request — start on it now[^]*\nHelp Alex with this$/);
});

test('forever: a link with no end never expires and says so in its intro', async (t) => {
  const fx = fixture(t);
  const link = await fx.bridge.link({ session: SID, url: URL_, minutes: 'forever', host: 'h' });
  assert.equal(link.until, null);
  assert.equal(link.minutes, null);
  await fx.bridge.tick();
  assert.match(fx.sent[0].text, /You are linked to a Slack thread until the owner unlinks it/);
  fx.advance(30 * 24 * 60 * 60 * 1000);
  await fx.bridge.tick();
  assert.equal((await fx.bridge.list()).links.length, 1);
  assert.equal((await fx.bridge.reply({ link: link.id, text: 'still here' })).ok, true);
});

test('relink: an ended link comes back on the same thread, with its instructions, mode and duration', async (t) => {
  const fx = fixture(t);
  const a = await fx.bridge.link({ session: SID, url: URL_, minutes: '4h', mode: 'proactive', task: 'demo help', host: 'h' });
  await fx.bridge.unlink(a.id);
  await assert.rejects(fx.bridge.relink('sl-000000'), { status: 404 });
  const reads = fx.slack.calls.length;
  const b = await fx.bridge.relink(a.id);
  assert.notEqual(b.id, a.id);
  assert.equal(b.threadTs, THREAD);
  assert.equal(b.permalink, a.permalink);
  assert.equal(b.task, 'demo help');
  assert.equal(b.mode, 'proactive');
  assert.equal(b.minutes, 240);
  assert.equal(b.until - b.createdAt, 240 * 60 * 1000);
  assert.ok(fx.slack.calls.slice(reads).some((c) => c.method === 'conversations.replies'), 're-reads the thread');
  // Relinking again while it is active renews it; a duration can be given.
  const c = await fx.bridge.relink(a.id, { minutes: 'forever' });
  assert.equal(c.id, b.id);
  assert.equal(c.renewed, true);
  assert.equal(c.until, null);
  assert.equal((await fx.bridge.list()).links.length, 1);
});

const settle = () => new Promise((r) => setTimeout(r, 10));

test('slack.spawn: a mention in a thread no session follows opens a session there, linked, the message its request', async (t) => {
  const spawns = [];
  const fx = fixture(t, {
    settings: { owner: 'UOWNER', spawn: { host: 'laptop', dir: '~/Code/work', model: 'opus', minutes: '1d', from: null } },
    spawn: async (host, body) => (spawns.push({ host, body }), { name: 'job', slack: { id: 'sl-abcdef' } }),
  });
  const mention = (ts, user, text, extra = {}) => ({ type: 'message', channel: 'C0123ABCDEF', ts, user, text, ...extra });

  // Not the owner (default from), no mention, a DM, a bot: nothing opens.
  await fx.bridge.handleEvent(mention('1792778200.000001', 'U1', '<@UBOT> fix it'));
  await fx.bridge.handleEvent(mention('1792778200.000002', 'UOWNER', 'no mention here'));
  await fx.bridge.handleEvent(mention('1792778200.000003', 'UOWNER', '<@UBOT> hi', { channel: 'D0123', channel_type: 'im' }));
  await fx.bridge.handleEvent(mention('1792778200.000004', 'UOWNER', '<@UBOT> hi', { bot_id: 'B9' }));
  await settle();
  assert.equal(spawns.length, 0);

  // The owner, in a reply: the thread's root is linked; the mention is the request; 👀 on it.
  await fx.bridge.handleEvent(mention('1792778200.000005', 'UOWNER', '<@UBOT> why is <https://x.test|this> red?', { thread_ts: THREAD }));
  await fx.bridge.handleEvent(mention('1792778200.000005', 'UOWNER', '<@UBOT> why is <https://x.test|this> red?', { thread_ts: THREAD })); // sent twice
  await settle();
  assert.equal(spawns.length, 1);
  assert.equal(spawns[0].host, 'laptop');
  const b = spawns[0].body;
  assert.equal(b.dir, '~/Code/work');
  assert.equal(b.model, 'opus');
  assert.equal(b.prompt, 'why is this (https://x.test) red?');
  assert.equal(b.slack.url, `${URL_}?thread_ts=${THREAD}`);
  assert.equal(b.slack.minutes, '1d');
  assert.equal(b.slack.from, 'UOWNER');
  assert.match(b.slack.task, /Owner, who started this session from Slack/);
  assert.ok(fx.slack.calls.some((c) => c.method === 'reactions.add' && c.params.timestamp === '1792778200.000005'));
  assert.ok(fx.slack.calls.some((c) => c.method === 'chat.getPermalink' && c.params.message_ts === THREAD));

  // A thread a session already follows: the mention goes to it, nothing opens.
  await fx.bridge.link({ session: SID, url: URL_, minutes: 60, host: 'workstation' });
  await fx.bridge.handleEvent(mention('1792778200.000006', 'UOWNER', '<@UBOT> and now?', { thread_ts: THREAD }));
  await settle();
  assert.equal(spawns.length, 1);
  assert.equal((await fx.bridge.list()).links[0].pending, 1);
  assert.equal((await fx.bridge.status()).spawn.host, 'laptop');
});

test('slack.spawn: from "anyone" or a list; a failed spawn is said in the thread', async (t) => {
  let fail = true;
  const spawns = [];
  const fx = fixture(t, {
    self: 'workstation',
    settings: { owner: 'UOWNER', spawn: { minutes: '2h', from: 'anyone' } },
    spawn: async (host, body) => {
      spawns.push({ host, body });
      if (fail) throw new Error('dir is not a spawn dir');
      return { name: 'job' };
    },
  });
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778300.000001', user: 'U1', text: '<@UBOT> look at this' });
  await settle();
  assert.equal(spawns.length, 1);
  assert.equal(spawns[0].host, 'workstation', 'no host: the bridge\'s own');
  assert.equal(spawns[0].body.dir, undefined);
  const said = fx.slack.calls.find((c) => c.method === 'chat.postMessage');
  assert.equal(said.params.thread_ts, '1792778300.000001', 'a top-level mention: its own thread');
  assert.match(said.params.text, /Could not open a session on workstation: dir is not a spawn dir/);
  // The same thread again after a failure: tried again.
  fail = false;
  await fx.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778300.000002', thread_ts: '1792778300.000001', user: 'U2', text: '<@UBOT> again' });
  await settle();
  assert.equal(spawns.length, 2);

  const listed = fixture(t, { settings: { owner: 'UOWNER', spawn: { from: ['U2'] } }, spawn: async (host, body) => (spawns.push({ host, body }), {}) });
  await listed.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778300.000003', user: 'UOWNER', text: '<@UBOT> x' });
  await listed.bridge.handleEvent({ type: 'message', channel: 'C0123ABCDEF', ts: '1792778300.000004', user: 'U2', text: '<@UBOT> y' });
  await settle();
  assert.deepEqual(spawns.slice(2).map((s) => s.body.slack.from), ['U2']);
});

test('a session started by a colleague\'s mention: its intro says who asked, and that it is not the owner', async (t) => {
  const fx = fixture(t);
  const l = await fx.bridge.link({ session: SID, url: URL_, minutes: '1d', host: 'h', first: 'why is it red?', firstFrom: 'U1' });
  assert.deepEqual(l.startedBy, { user: 'U1', name: 'Alex', owner: false });
  await fx.bridge.tick();
  assert.match(fx.sent[0].text, /Alex \(U1\) started this session from Slack[^\n]*colleague, not the owner[^\n]*\nwhy is it red\?$/);
  assert.doesNotMatch(fx.sent[0].text, /this is the owner, not someone in Slack/);
  await assert.rejects(fx.bridge.link({ session: OTHER, url: URL_, minutes: 5, host: 'h', firstFrom: 'nobody' }), /firstFrom must be a Slack user id/);

  const own = fixture(t);
  await own.bridge.link({ session: SID, url: URL_, minutes: '1d', host: 'h', first: '', firstFrom: 'UOWNER' });
  await own.bridge.tick();
  assert.match(own.sent[0].text, /The owner \(Owner, UOWNER\) started this session from Slack[^]*\(only the mention: read the thread/);
});

test('config slack.spawn: defaults, from, and what it refuses', () => {
  const cfg = (spawn) => normalizeConfig({ self: 'workstation', slack: { host: 'workstation', owner: 'UOWNER', spawn } }, { env: {}, home: '/home/tester' }).slack;
  assert.equal(cfg(undefined).spawn, null);
  assert.deepEqual(cfg({ host: 'laptop', dir: '~/Code/project', model: 'opus' }).spawn, {
    host: 'laptop', dir: '~/Code/project', model: 'opus', harness: null, ghIdentity: null, mode: null, task: null, minutes: '1d', from: null,
  });
  assert.equal(cfg({ from: 'anyone', minutes: 'forever' }).spawn.from, 'anyone');
  assert.deepEqual(cfg({ from: ['U1'] }).spawn.from, ['U1']);
  assert.throws(() => cfg({ from: 'everyone' }), /slack.spawn.from/);
  assert.throws(() => cfg('yes'), /slack.spawn must be an object/);
  assert.throws(() => cfg({ dir: 3 }), /slack.spawn.dir must be a string/);
});
