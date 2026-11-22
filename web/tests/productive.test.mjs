// Productive links: task refs, comment bodies (mentions, HTML ↔ text), changesets, which events
// wake a session and the throttle, the bridge (a mention by an allowed person starts one linked
// session, once; others start none; comments and changes on a linked task are delivered in
// throttled batches; a closed task ends the link; `comment` posts as the account) and the config.
// Productive, the sessions and the fleet CLI are fakes.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { normalizeConfig, productiveSpawn } from '../lib/config.mjs';
import {
  activityEvents,
  changeLines,
  classify,
  createProductiveBridge,
  htmlToText,
  mentionIds,
  notificationTask,
  orgSlug,
  parseTaskRef,
  textToHtml,
  throttleReady,
} from '../lib/productive.mjs';

const ME = '900';
const ALEX = '11';
const STRANGER = '12';
const SID = 'aaaaaaaa-0000-0000-0000-000000000001';
const PEOPLE = { [ME]: ['Bo', 'Bott'], [ALEX]: ['Alex', 'Ant'], [STRANGER]: ['Sam', 'Stranger'] };
const mention = (id, label) => `@[{"type":"person","id":"${id}","label":"${label}","avatar_url":null}]`;

function fakeProductive() {
  const tasks = new Map(); // id → { id, number, title, closed, description, activities: [] }
  let notifications = [];
  let nextActivity = 1000;
  let nextComment = 1;
  let n = 0;
  const posted = [];
  const calls = [];
  const person = (id) => ({ type: 'people', id, attributes: { first_name: PEOPLE[id]?.[0] ?? 'X', last_name: PEOPLE[id]?.[1] ?? null } });
  async function api(method, p, body) {
    calls.push(`${method} ${p}`);
    if (method === 'GET' && p.startsWith('organization_memberships')) {
      return {
        // A person in two organizations: the bridge takes the one of settings.org.
        data: [
          { id: '5', relationships: { person: { data: { type: 'people', id: '5' } }, organization: { data: { type: 'organizations', id: '1' } } } },
          { id: ME, relationships: { person: { data: { type: 'people', id: ME } }, organization: { data: { type: 'organizations', id: '109' } } } },
        ],
        included: [person(ME), { type: 'organizations', id: '109', attributes: { name: 'Productive' } }],
      };
    }
    if (method === 'GET' && p.startsWith('notifications')) return { data: notifications };
    if (method === 'GET' && p.startsWith('people?')) {
      const email = decodeURIComponent(p.split('=')[1]);
      return { data: email === 'alex@example.com' ? [{ id: ALEX }] : [] };
    }
    let m = /^tasks\/(\d+)/.exec(p);
    if (method === 'GET' && m) {
      const t = tasks.get(m[1]);
      if (!t) throw Object.assign(new Error('HTTP 404'), { status: 404 });
      return {
        data: { id: t.id, attributes: { task_number: t.number, title: t.title, closed: t.closed, description: t.description }, relationships: { project: { data: { id: '5' } } } },
        included: [{ type: 'projects', id: '5', attributes: { name: 'Acme' } }],
      };
    }
    m = /^activities\?filter%5Btask_id%5D=(\d+)/.exec(p);
    if (method === 'GET' && m) {
      const t = tasks.get(m[1]);
      const acts = [...(t?.activities ?? [])].reverse(); // newest first, like Productive
      const included = [];
      for (const a of acts) {
        included.push(person(a.author));
        if (a.comment) included.push({ type: 'comments', id: a.comment.id, attributes: { body: a.comment.body } });
      }
      return {
        data: acts.map((a) => ({
          id: String(a.id),
          attributes: { event: a.event, item_type: a.itemType, item_name: a.itemName ?? null, changeset: a.changeset ?? [], created_at: a.at },
          relationships: { creator: { data: { type: 'people', id: a.author } }, comment: a.comment ? { data: { type: 'comments', id: a.comment.id } } : { data: null } },
        })),
        included,
      };
    }
    if (method === 'POST' && p === 'comments') {
      posted.push(body);
      const t = tasks.get(body.data.relationships.task.data.id);
      const id = String(nextComment++);
      t.activities.push({ id: nextActivity++, event: 'create', itemType: 'comment', author: ME, at: new Date().toISOString(), comment: { id, body: body.data.attributes.body } });
      return { data: { id } };
    }
    throw new Error(`unexpected ${method} ${p}`);
  }
  return {
    api,
    calls,
    posted,
    task(id, { number = id, title = `Task ${id}`, description = '<p>Do the <strong>thing</strong>.</p>' } = {}) {
      const t = { id: String(id), number: String(number), title, closed: false, description, activities: [] };
      tasks.set(t.id, t);
      return t;
    },
    comment(id, author, body, at) {
      const t = tasks.get(String(id));
      t.activities.push({ id: nextActivity++, event: 'create', itemType: 'comment', author, at, comment: { id: String(nextComment++), body } });
    },
    change(id, author, changeset, at) {
      tasks.get(String(id)).activities.push({ id: nextActivity++, event: 'update', itemType: 'task', author, at, changeset });
    },
    /** A notification about task `id` (a new `last_action_at`, so it is new). */
    notify(id, { mention: isMention = false } = {}) {
      n += 1;
      notifications = [
        { id: `n${id}`, attributes: { target_type: 'task', target_id: Number(id), parent_type: 'project', parent_id: 5, mention: isMention, last_action_at: `2026-01-01T00:00:${String(n).padStart(2, '0')}Z` } },
        ...notifications.filter((x) => x.id !== `n${id}`),
      ];
    },
    close(id) {
      tasks.get(String(id)).closed = true;
    },
  };
}

function setup({ spawnCfg = { from: ['alex@example.com'], keep: 24 * 60 }, startAt = Date.parse('2026-01-01T00:00:00Z'), person = false } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-productive-'));
  const pr = fakeProductive();
  let t = startAt;
  const sessions = [];
  const sent = [];
  const spawned = [];
  // Another host's sessions (a peer); null: it does not answer.
  const remote = { laptop: [] };
  const on = (host) => (!host || host === 'shed' ? sessions : remote[host]);
  const bridge = createProductiveBridge({
    settings: { org: '109', spawn: spawnCfg, ...(person ? { person: true } : {}) },
    self: 'shed',
    api: pr.api,
    listSessions: async (host) => on(host)?.map((s) => ({ ...s })) ?? null,
    send: async (s, text, host) => {
      sent.push({ session: s.session_id, text, host: host ?? null });
      const live = on(host)?.find((x) => x.session_id === s.session_id);
      if (live) live.status = 'busy';
    },
    spawn: async (body) => {
      spawned.push(body);
      sessions.push({ session_id: SID, tmux_session: body.name, name: body.name, status: 'idle', handle: '%1' });
      return { name: body.name, tmuxSession: body.name };
    },
    sleep: async () => {},
    restore: async () => ({ restored: [] }),
    file: path.join(dir, 'productive-links.json'),
    now: () => t,
  });
  return {
    bridge,
    pr,
    sessions,
    remote,
    sent,
    spawned,
    advance: (ms) => {
      t += ms;
    },
    at: (offsetMs = 0) => new Date(t + offsetMs).toISOString(),
    idle: () => sessions.forEach((s) => (s.status = 'idle')),
    async tick(ms = 61_000) {
      t += ms;
      await bridge.tick();
    },
  };
}

/** A session started by Alex's mention on task 7, its intro delivered, idle again. */
async function linked(opts) {
  const s = setup(opts);
  await s.bridge.tick(); // first run: `since` is now
  s.pr.task(7, { number: 42 });
  s.pr.comment(7, ALEX, `<p>${mention(ME, 'Bo Bott')} can you look at this?</p>`, s.at(1000));
  s.pr.notify(7, { mention: true });
  await s.tick();
  await s.tick();
  s.idle();
  return s;
}

test('parseTaskRef: task URLs (any org, suffix) and bare ids; anything else is null', () => {
  assert.equal(parseTaskRef('https://app.productive.io/109-productive/tasks/21406702'), '21406702');
  assert.equal(parseTaskRef('https://app.productive.io/109-productive/tasks/21406702?filter=x#c'), '21406702');
  assert.equal(parseTaskRef('https://app.productive.io/109-productive/projects/5/tasks/task/77'), '77');
  assert.equal(parseTaskRef('#123'), '123');
  assert.equal(parseTaskRef('123'), '123');
  assert.equal(parseTaskRef('https://example.com/tasks/1'), null);
  assert.equal(parseTaskRef('abc'), null);
  assert.equal(orgSlug('109', 'Productive'), '109-productive');
  assert.equal(orgSlug('7', 'Čvor & Co.'), '7-cvor-co');
});

test('notificationTask: the task a notification is about (its own, or a todo/comment parent)', () => {
  assert.deepEqual(notificationTask({ id: 1, attributes: { target_type: 'task', target_id: 5, parent_type: 'project', parent_id: 9, mention: true, last_action_at: 'x' } }), { id: '1', taskId: '5', mention: true, at: 'x' });
  assert.equal(notificationTask({ id: 2, attributes: { target_type: 'todo', target_id: 3, parent_type: 'task', parent_id: 6 } }).taskId, '6');
  assert.equal(notificationTask({ id: 3, attributes: { target_type: 'deal', target_id: 3, parent_type: 'company', parent_id: 6 } }), null);
});

test('comment bodies: mentions found (raw or &quot;-escaped), HTML to text, text to HTML with real mentions', () => {
  const html = `<div>${mention(ME, 'Bo Bott')} see <a href="https://x.dev/a">the PR</a><br>and &amp; more</div><ul><li>one</li></ul>`;
  assert.deepEqual([...mentionIds(html)], [ME]);
  assert.deepEqual([...mentionIds(html.replace(/"/g, '&quot;'))], [ME]);
  assert.equal(htmlToText(html), '@Bo Bott see the PR (https://x.dev/a)\nand & more\n\n- one');
  const out = textToHtml('Done: **merged** `x<y>`.\n\n- a\n- b\n\nThanks @[Person:11:Alex Ant], see https://x.dev');
  assert.equal(
    out,
    '<p>Done: <strong>merged</strong> <code>x&lt;y&gt;</code>.</p><ul><li>a</li><li>b</li></ul><p>Thanks @[{"type":"person","id":"11","label":"Alex Ant"}], see <a href="https://x.dev">https://x.dev</a></p>',
  );
  assert.deepEqual([...mentionIds(out)], [ALEX]);
});

test('changeLines: field: from → to, values from objects, long text clipped', () => {
  assert.deepEqual(changeLines([{ workflow_status_id: [{ value: 'Open', id: 1 }, { value: 'Review', id: 2 }] }, { assignee: [null, { value: 'Alex Ant', avatar_url: 'u' }] }]), [
    'workflow status: Open → Review',
    'assignee: Alex Ant',
  ]);
  assert.equal(changeLines([{ description: [null, { value: `<p>${'x'.repeat(500)}</p>` }] }])[0].length < 320, true);
});

test('classify and throttle: mentions, allowed comments and changes wake; a burst waits for quiet, deliveries keep a gap', () => {
  const ev = (kind, author, mentions = []) => ({ kind, author: { id: author }, mentions });
  const from = [ALEX];
  assert.equal(classify(ev('comment', STRANGER, [ME]), { me: ME, from }), 'mention');
  assert.equal(classify(ev('comment', ALEX), { me: ME, from }), 'comment');
  assert.equal(classify(ev('comment', STRANGER), { me: ME, from }), 'context');
  assert.equal(classify(ev('change', STRANGER), { me: ME, from }), 'change');
  const now = 1_000_000;
  const comment = [{ type: 'comment' }];
  assert.equal(throttleReady([{ type: 'context' }], { now }), false);
  assert.equal(throttleReady(comment, { lastEventAt: now - 10_000, now }), false); // still busy
  assert.equal(throttleReady(comment, { lastEventAt: now - 50_000, lastDeliveredAt: now - 60_000, now }), false); // gap
  assert.equal(throttleReady(comment, { lastEventAt: now - 50_000, lastDeliveredAt: now - 130_000, now }), true);
  assert.equal(throttleReady([{ type: 'change' }], { lastEventAt: now - 50_000, lastDeliveredAt: now - 130_000, now }), false);
  assert.equal(throttleReady([{ type: 'mention' }], { lastEventAt: now - 25_000, lastDeliveredAt: now - 35_000, now }), true);
});

test('activityEvents: comments with text and mentions, changes, oldest first; comment edits left out', () => {
  const data = {
    data: [
      { id: '3', attributes: { event: 'update', item_type: 'comment', changeset: [{ body: ['a', 'b'] }] }, relationships: { creator: { data: { id: ALEX } } } },
      { id: '2', attributes: { event: 'update', item_type: 'task', changeset: [{ closed: [false, true] }], created_at: 't2' }, relationships: { creator: { data: { id: ALEX } } } },
      { id: '1', attributes: { event: 'create', item_type: 'comment', created_at: 't1' }, relationships: { creator: { data: { id: STRANGER } }, comment: { data: { id: 'c1' } } } },
    ],
    included: [
      { type: 'people', id: ALEX, attributes: { first_name: 'Alex', last_name: 'Ant' } },
      { type: 'comments', id: 'c1', attributes: { body: `<p>hey ${mention(ME, 'Bo')}</p>` } },
    ],
  };
  const ev = activityEvents(data);
  assert.deepEqual(ev.map((e) => [e.id, e.kind]), [[1, 'comment'], [2, 'change']]);
  assert.equal(ev[0].body, 'hey @Bo');
  assert.deepEqual(ev[0].mentions, [ME]);
  assert.equal(ev[0].author.name, null);
  assert.deepEqual(ev[1].changes, ['closed: false → true']);
});

test('a mention by an allowed person starts one linked session; its intro carries the task and the comment', async () => {
  const s = await linked();
  assert.equal(s.spawned.length, 1);
  assert.equal(s.spawned[0].name, 'task-42');
  assert.equal(s.sent.length, 1);
  const intro = s.sent[0].text;
  assert.match(intro, /\[productive pl-[0-9a-f]{6}\] This session is linked to the Productive task #42 "Task 7" \(project Acme\): https:\/\/app\.productive\.io\/109-productive\/tasks\/7/);
  assert.match(intro, /You act on it as Bo Bott \(person 900\)/);
  assert.match(intro, /Alex Ant \(person 11\) mentioned you on this task:\n {4}@Bo Bott can you look at this\?/);
  assert.match(intro, /The task description:\n {4}Do the thing\./);
  assert.match(intro, /fleet productive reply/);
  const st = await s.bridge.status();
  assert.equal(st.links.length, 1);
  assert.equal(st.links[0].startedBy.id, ALEX);
  // The same notification again: no second session, nothing new delivered.
  s.pr.notify(7, { mention: true });
  await s.tick();
  await s.tick(5 * 60_000);
  assert.equal(s.spawned.length, 1);
  assert.equal(s.sent.length, 1);
});

test('a mention by someone not in spawn.from, or from before the bridge ran, starts nothing', async () => {
  const s = setup();
  s.pr.task(3);
  s.pr.comment(3, ALEX, `<p>${mention(ME, 'Bo')} old</p>`, s.at(-3600_000));
  s.pr.notify(3, { mention: true });
  await s.bridge.tick();
  s.pr.task(4);
  s.pr.comment(4, STRANGER, `<p>${mention(ME, 'Bo')} please</p>`, s.at(1000));
  s.pr.notify(4, { mention: true });
  await s.tick();
  assert.equal(s.spawned.length, 0);
  assert.equal((await s.bridge.status()).links.length, 0);
});

test('on a linked task: comments come in one throttled batch, a stranger only as context, its own never; a change alone waits longer', async () => {
  const s = await linked();
  // A stranger's comment alone wakes nothing.
  s.pr.comment(7, STRANGER, '<p>fyi, unrelated</p>', s.at());
  s.pr.notify(7);
  await s.tick();
  await s.tick(5 * 60_000);
  assert.equal(s.sent.length, 1);
  // Alex writes twice: delivered together, once the task has been quiet ~45s, with the stranger's as context.
  s.pr.comment(7, ALEX, '<p>first</p>', s.at());
  s.pr.comment(7, ALEX, '<p>second</p>', s.at());
  s.pr.comment(7, ME, '<p>my own</p>', s.at());
  s.pr.notify(7);
  await s.tick(); // read; not quiet yet
  assert.equal(s.sent.length, 1);
  await s.tick(10_000);
  assert.equal(s.sent.length, 1);
  await s.tick(40_000);
  assert.equal(s.sent.length, 2);
  const text = s.sent[1].text;
  assert.match(text, /^\[productive pl-[0-9a-f]{6}\] task #42 \(https:\/\/app\.productive\.io\/109-productive\/tasks\/7\): new on the task\./);
  assert.match(text, /Sam Stranger \(person 12\) commented[^\n]*:\n {4}fyi, unrelated/);
  assert.match(text, /Alex Ant \(person 11\) commented[^\n]*:\n {4}first[\s\S]*second/);
  assert.doesNotMatch(text, /my own/);
  // A status change alone: not before 10 min since the last delivery.
  s.idle();
  s.pr.change(7, STRANGER, [{ workflow_status_id: [{ value: 'Open' }, { value: 'Review' }] }], s.at());
  s.pr.notify(7);
  await s.tick();
  await s.tick(2 * 60_000);
  assert.equal(s.sent.length, 2);
  await s.tick(8 * 60_000);
  assert.equal(s.sent.length, 3);
  assert.match(s.sent[2].text, /Sam Stranger changed the task: workflow status: Open → Review/);
});

test('a closed task ends its link', async () => {
  const s = await linked();
  s.pr.close(7);
  s.pr.notify(7);
  await s.tick();
  const st = await s.bridge.status();
  assert.equal(st.links.length, 0);
  assert.equal(st.ended[0].endReason, 'closed');
});

test('comment: posts on the session\'s linked task as the account; an unlinked session is refused', async () => {
  const s = await linked();
  const r = await s.bridge.comment({ pane: '%1', body: 'On it, @[Person:11:Alex Ant].' });
  assert.equal(r.taskId, '7');
  assert.equal(s.pr.posted.length, 1);
  assert.equal(s.pr.posted[0].data.relationships.task.data.id, '7');
  assert.equal(s.pr.posted[0].data.attributes.body, '<p>On it, @[{"type":"person","id":"11","label":"Alex Ant"}].</p>');
  // Its own comment comes back in the feed and wakes nothing.
  s.pr.notify(7);
  await s.tick();
  await s.tick(5 * 60_000);
  assert.equal(s.sent.length, 1);
  s.sessions.push({ session_id: 'other', handle: '%9', status: 'idle' });
  await assert.rejects(s.bridge.comment({ pane: '%9', body: 'x' }), /not linked to a Productive task/);
  await assert.rejects(s.bridge.comment({ pane: '%1', body: '  ' }), /body is required/);
});

test('link: an open task, a live session; a second session on the same task is refused', async () => {
  const s = setup({ spawnCfg: null });
  s.pr.task(8);
  s.sessions.push({ session_id: SID, handle: '%1', status: 'idle', name: 'mine' });
  s.sessions.push({ session_id: 'bbbbbbbb-0000-0000-0000-000000000002', handle: '%2', status: 'idle', name: 'other' });
  const l = await s.bridge.link({ url: 'https://app.productive.io/109-productive/tasks/8', pane: '%1', keep: '12h' });
  assert.equal(l.taskId, '8');
  assert.equal(l.keep, 12 * 60);
  await assert.rejects(s.bridge.link({ url: '8', pane: '%2' }), /already followed by mine/);
  const again = await s.bridge.link({ url: '8', pane: '%1', task: 'review it' });
  assert.equal(again.renewed, true);
  s.pr.task(9).closed = true;
  await assert.rejects(s.bridge.link({ url: '9', pane: '%2' }), /is closed/);
  await assert.rejects(s.bridge.link({ url: 'nope', pane: '%2' }), /must be a Productive task/);
});

test('a session on another host: linked through its host, typed into there, left alone while that host does not answer', async () => {
  const s = setup({ spawnCfg: null });
  await s.bridge.tick();
  s.pr.task(8);
  s.remote.laptop.push({ session_id: SID, handle: '%3', status: 'idle', name: 'far' });
  await assert.rejects(s.bridge.link({ url: '8', pane: '%3' }), /no live session %3 on shed/);
  const l = await s.bridge.link({ url: '8', pane: '%3', host: 'laptop' });
  assert.equal(l.host, 'laptop');
  await s.tick();
  assert.equal(s.sent.length, 1);
  assert.equal(s.sent[0].host, 'laptop');
  s.remote.laptop[0].status = 'idle';
  s.pr.comment(8, ME, '<p>ping</p>', s.at(1000));
  s.remote.laptop = null; // the peer is down: nothing is woken or started
  s.pr.notify(8);
  await s.tick();
  await s.tick(5 * 60_000);
  assert.equal(s.sent.length, 1);
  assert.equal(s.spawned.length, 0);
  // The session comments from its own host.
  s.remote.laptop = [{ session_id: SID, handle: '%3', status: 'idle', name: 'far' }];
  const r = await s.bridge.comment({ pane: '%3', host: 'laptop', body: 'done' });
  assert.equal(r.taskId, '8');
});

test('person: an account that is also a person — its own comments wake the session, only what the bridge posted does not', async () => {
  const s = setup({ spawnCfg: null, person: true });
  await s.bridge.tick();
  s.pr.task(8);
  s.sessions.push({ session_id: SID, handle: '%1', status: 'idle', name: 'mine' });
  await s.bridge.link({ url: '8', pane: '%1' });
  await s.tick();
  s.idle();
  await s.bridge.comment({ pane: '%1', body: 'Posted by the session.' });
  s.pr.comment(8, ME, '<p>Written by the person, in Productive.</p>', s.at(1000));
  s.pr.notify(8);
  await s.tick();
  await s.tick(5 * 60_000);
  assert.equal(s.sent.length, 2);
  assert.match(s.sent[1].text, /Written by the person/);
  assert.doesNotMatch(s.sent[1].text, /Posted by the session/);
});

test('config: productive.host enables the bridge on that host; org required there; from ids or emails', () => {
  const base = { self: 'shed' };
  const off = normalizeConfig({ ...base }, { env: {} });
  assert.equal(off.productive.enabled, false);
  const on = normalizeConfig({ ...base, productive: { host: 'shed', org: 109, spawn: { from: ['Alex@Example.com', 11], keep: '12h' } } }, { env: {} });
  assert.equal(on.productive.enabled, true);
  assert.equal(on.productive.org, '109');
  assert.equal(on.productive.keychain, 'PRODUCTIVE_AUTH_TOKEN');
  assert.deepEqual(on.productive.spawn.from, ['alex@example.com', '11']);
  assert.equal(on.productive.spawn.keep, 12 * 60);
  assert.equal(normalizeConfig({ ...base, productive: { host: 'other', org: 1 } }, { env: {} }).productive.enabled, false);
  assert.throws(() => normalizeConfig({ ...base, productive: { host: 'shed' } }, { env: {} }), /org is required/);
  assert.throws(() => productiveSpawn({ from: ['not an id'] }), /person ids or emails/);
  assert.deepEqual(productiveSpawn({ from: 'anyone' }).from, ['anyone']);
  assert.equal(on.productive.person, false);
  assert.equal(normalizeConfig({ ...base, productive: { host: 'shed', org: 1, person: true, tokenFile: '~/t' } }, { env: {} }).productive.person, true);
  assert.throws(() => normalizeConfig({ ...base, productive: { host: 'shed', org: 1, person: 'yes' } }, { env: {} }), /person must be/);
});
