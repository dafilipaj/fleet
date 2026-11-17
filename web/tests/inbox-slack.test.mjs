// The inbox's Slack poller (lib/inbox-slack.mjs): conversations from search, answered ones closed
// without a model, only new messages sorted, the digest. A fake Slack API; no network, no model.
import test from 'node:test';
import assert from 'node:assert/strict';

import {
  classifyPrompt,
  conversationOf,
  conversationState,
  createSlackPoller,
  emptySlackState,
  isHuman,
  permalink,
  plainText,
  readVerdicts,
} from '../lib/inbox-slack.mjs';

const ME = 'UME';
const T0 = Date.UTC(2026, 9, 20, 9);
const ts = (minAgo) => ((T0 - minAgo * 60e3) / 1000).toFixed(6);

test('isHuman: people only', () => {
  assert.equal(isHuman({ user: 'U1', text: 'hi' }), true);
  assert.equal(isHuman({ user: 'U1', bot_id: 'B1' }), false);
  assert.equal(isHuman({ user: 'U1', subtype: 'channel_join' }), false);
  assert.equal(isHuman({ user: 'U1', subtype: 'thread_broadcast' }), true);
});

test('conversationOf: a thread by its root, a DM top level by its channel, a channel message by itself', () => {
  assert.equal(conversationOf({ ts: '2.0', permalink: 'https://x.slack.com/archives/C1/p2?thread_ts=1.0&cid=C1', channel: { id: 'C1', name: 'dev' } }).key, 'slack:C1:1.0');
  assert.equal(conversationOf({ ts: '2.0', permalink: 'https://x.slack.com/archives/D1/p2', channel: { id: 'D1', is_im: true } }).key, 'slack:D1');
  const c = conversationOf({ ts: '2.0', permalink: 'https://x.slack.com/archives/C1/p2', channel: { id: 'C1', name: 'dev' } });
  assert.deepEqual([c.key, c.root, c.label], ['slack:C1:2.0', '2.0', '#dev']);
});

test('conversationState: closed when the user wrote last or reacted; open with what came after', () => {
  assert.equal(conversationState([{ user: 'U1', ts: '1' }, { user: ME, ts: '2' }], ME).open, false);
  assert.equal(conversationState([{ user: 'U1', ts: '1', reactions: [{ name: 'white_check_mark', users: [ME] }] }], ME).reason, 'reacted');
  const st = conversationState([{ user: 'U1', ts: '1' }, { user: ME, ts: '2' }, { user: 'U2', ts: '3' }, { user: 'U1', ts: '4', bot_id: 'B' }], ME);
  assert.equal(st.open, true);
  assert.equal(st.lastOther.ts, '3');
  assert.deepEqual(st.since.map((m) => m.ts), ['3']);
  assert.equal(st.answeredBefore, true);
});

test('plainText: names, @me, links, clipped', () => {
  assert.equal(plainText('<@UME> pogledaj <https://g.com/pr/1|PR #1> i <@U2>', { U2: 'Ana' }, ME), '@me pogledaj PR #1 (https://g.com/pr/1) i @Ana');
  assert.equal(plainText('x'.repeat(20), {}, ME, 10).length, 10);
});

test('permalink: a thread reply carries its root', () => {
  assert.equal(permalink('https://x.slack.com/', 'C1', '2.5', '1.5'), 'https://x.slack.com/archives/C1/p25?thread_ts=1.5&cid=C1');
  assert.equal(permalink('https://x.slack.com', 'D1', '2.5'), 'https://x.slack.com/archives/D1/p25');
});

test('readVerdicts: only asked keys, effort checked', () => {
  const v = readVerdicts([{ key: 'a', needsMe: true, title: 'T', effort: 'huge' }, { key: 'zzz', needsMe: true }], ['a', 'b']);
  assert.deepEqual(Object.keys(v), ['a']);
  assert.equal(v.a.effort, 'medium');
});

test('classifyPrompt: keys, the user\'s rules, the digest', () => {
  const p = classifyPrompt({ meName: 'Ivo', rules: 'Ignore #random.', convs: [{ key: 'slack:D1', label: 'DM with Ana', dm: true, digest: [{ at: T0, who: 'Ana', text: 'možeš pogledati?' }] }] });
  assert.match(p, /=== slack:D1 · DM with Ana/);
  assert.match(p, /Ana: možeš pogledati\?/);
  assert.match(p, /Ignore #random\./);
});

/** A tiny Slack: channels with messages; search answers by query prefix. */
function fakeSlack({ search = {}, channels = {} }) {
  const calls = [];
  const api = async (method, p = {}) => {
    calls.push([method, p]);
    if (method === 'auth.test') return { user_id: ME, url: 'https://x.slack.com/' };
    if (method === 'users.info') return { user: { real_name: { U1: 'Ana', U2: 'Bo' }[p.user] ?? p.user } };
    if (method === 'search.messages') {
      const kind = p.query.startsWith('to:me') ? 'to' : p.query.startsWith('from:me') ? 'from' : 'mention';
      return { messages: { matches: search[kind] ?? [], paging: { pages: 1 } } };
    }
    if (method === 'conversations.history') return { messages: [...(channels[p.channel] ?? [])].reverse() };
    if (method === 'conversations.replies') return { messages: (channels[`${p.channel}:${p.ts}`] ?? []), has_more: false };
    throw new Error(`unexpected ${method}`);
  };
  return { api, calls };
}

test('a poll: a DM the user answered closes with no model; a new ask is sorted once; a later answer closes it', async () => {
  const channels = {
    D1: [{ user: 'U1', ts: ts(60), text: 'pogledaj PR?' }, { user: ME, ts: ts(30), text: 'jesam' }],
    D2: [{ user: 'U2', ts: ts(20), text: 'možeš odobriti?' }],
  };
  const search = {
    to: [
      { ts: ts(60), user: 'U1', permalink: 'https://x.slack.com/archives/D1/p1', channel: { id: 'D1', is_im: true } },
      { ts: ts(20), user: 'U2', permalink: 'https://x.slack.com/archives/D2/p1', channel: { id: 'D2', is_im: true } },
    ],
  };
  const { api } = fakeSlack({ search, channels });
  const prompts = [];
  const clock = { t: T0 };
  const state = emptySlackState();
  const poller = createSlackPoller({
    api,
    state,
    now: () => clock.t,
    classify: async ({ prompt }) => {
      prompts.push(prompt);
      return { text: JSON.stringify([{ key: 'slack:D2', needsMe: true, title: 'Odobri', ask: 'Odobri zahtjev', effort: 'quick' }]) };
    },
  });
  let r = await poller.poll();
  assert.deepEqual(r.items.map((i) => [i.id, i.section, i.from]), [['slack:D2', 'quick', 'Bo']]);
  assert.equal(r.items[0].url, `https://x.slack.com/archives/D2/p${ts(20).replace('.', '')}`);
  assert.equal(prompts.length, 1);
  assert.doesNotMatch(prompts[0], /slack:D1/); // answered: never shown to the model

  // Nothing new: no model call.
  clock.t += 2 * 60e3;
  r = await poller.poll();
  assert.equal(prompts.length, 1);
  assert.equal(r.items.length, 1);

  // The user answers in the DM: gone at the next poll, still no model.
  channels.D2.push({ user: ME, ts: ((clock.t - 1000) / 1000).toFixed(6), text: 'done' });
  clock.t += 2 * 60e3;
  r = await poller.poll();
  assert.equal(r.items.length, 0);
  assert.equal(prompts.length, 1);
});

test('a thread the user posted in is watched: a new question there is sorted', async () => {
  const root = ts(100);
  const channels = { [`C1:${root}`]: [{ user: 'U1', ts: root, text: 'plan?' }, { user: ME, ts: ts(90), text: 'evo plana' }] };
  const search = { from: [{ ts: ts(90), user: ME, permalink: `https://x.slack.com/archives/C1/p1?thread_ts=${root}&cid=C1`, channel: { id: 'C1', name: 'dev' } }] };
  const { api } = fakeSlack({ search, channels });
  const clock = { t: T0 };
  let asked = 0;
  const poller = createSlackPoller({
    api,
    state: emptySlackState(),
    now: () => clock.t,
    classify: async () => {
      asked++;
      return { text: JSON.stringify([{ key: `slack:C1:${root}`, needsMe: true, title: 'Rok?', effort: 'medium' }]) };
    },
  });
  assert.equal((await poller.poll()).items.length, 0);
  // Later: Bo asks in the thread (no mention, so no search hit) — the watched thread catches it.
  search.from = [];
  channels[`C1:${root}`].push({ user: 'U2', ts: ts(-5), text: 'a rok?' });
  clock.t += 10 * 60e3;
  const r = await poller.poll();
  assert.equal(asked, 1);
  assert.deepEqual(r.items.map((i) => [i.id, i.section, i.meta.channel]), [[`slack:C1:${root}`, 'waiting', '#dev']]);
});

test('a model that answers no array fails the poll, keeping the last list', async () => {
  const { api } = fakeSlack({ search: { to: [{ ts: ts(5), user: 'U1', permalink: 'https://x.slack.com/archives/D1/p1', channel: { id: 'D1', is_im: true } }] }, channels: { D1: [{ user: 'U1', ts: ts(5), text: '?' }] } });
  const poller = createSlackPoller({ api, state: emptySlackState(), now: () => T0, classify: async () => ({ text: 'sorry' }) });
  await assert.rejects(poller.poll(), /no JSON array/);
});
