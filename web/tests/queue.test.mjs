// Prompt queue: one queued prompt per run, only once the session is idle and its run has ended;
// never while it waits on you; a pause holds the queue through the end of the run it was set in.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { createPromptQueue } from '../lib/queue.mjs';

const SID = 'aaaaaaaa-0000-0000-0000-000000000001';

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-queue-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const sent = [];
  let clock = 1_000_000;
  let ids = 0;
  const session = { session_id: SID, status: 'busy', updated_at: clock, backend: 'tmux' };
  const file = path.join(dir, 'prompt-queue.json');
  const make = () =>
    createPromptQueue({
      file,
      listSessions: async () => [session],
      send: async (_s, text) => void sent.push(text),
      now: () => clock,
      randomId: () => (++ids).toString(16).padStart(6, '0'),
    });
  const queue = make();
  // A status change: the session's `updated_at` moves with it, as in `fleet list`.
  const set = (status) => {
    clock += 1000;
    session.status = status;
    session.updated_at = clock;
  };
  return { queue, make, session, sent, set, file, wait: (ms) => (clock += ms) };
}

test('one prompt per run, only when the run has ended (idle), in order', async (t) => {
  const fx = fixture(t);
  await fx.queue.add(fx.session, { text: 'first' });
  await fx.queue.add(fx.session, { text: 'second' });
  await fx.queue.tick();
  assert.deepEqual(fx.sent, [], 'busy: nothing goes in');

  fx.set('idle');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['first']);
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['first'], 'still idle from before the delivery: wait for that run');

  fx.set('busy');
  await fx.queue.tick();
  fx.set('idle');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['first', 'second']);
  assert.deepEqual((await fx.queue.get(fx.session)).items, []);
});

test('a question (waiting) never triggers a send; the answered run ending does', async (t) => {
  const fx = fixture(t);
  await fx.queue.add(fx.session, { text: 'next' });
  fx.set('waiting');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, []);
  fx.set('busy');
  await fx.queue.tick();
  fx.set('idle');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['next']);
});

test('pause: holds through the end of this run, clears itself, sends after the next run', async (t) => {
  const fx = fixture(t);
  await fx.queue.add(fx.session, { text: 'later' });
  let v = await fx.queue.pause(fx.session, { paused: true });
  assert.equal(v.paused, true);

  fx.set('idle');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, []);
  v = await fx.queue.get(fx.session);
  assert.deepEqual([v.paused, v.held], [false, true], 'the pause is over; the queue holds');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, [], 'held through this idle');

  // You follow up by hand: that run ends → the queue moves again.
  fx.set('busy');
  await fx.queue.tick();
  fx.set('idle');
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['later']);
  assert.equal((await fx.queue.get(fx.session)).held, false);
});

test('unpausing a held queue sends at once when idle; send now ignores the run', async (t) => {
  const fx = fixture(t);
  await fx.queue.add(fx.session, { text: 'a' });
  await fx.queue.add(fx.session, { text: 'b' });
  await fx.queue.pause(fx.session, { paused: true });
  fx.set('idle');
  await fx.queue.tick();
  assert.equal((await fx.queue.get(fx.session)).held, true);
  await fx.queue.pause(fx.session, { paused: false });
  await fx.queue.tick();
  assert.deepEqual(fx.sent, ['a']);

  fx.set('busy');
  const { items } = await fx.queue.get(fx.session);
  await fx.queue.sendNow(fx.session, items[0].id);
  assert.deepEqual(fx.sent, ['a', 'b']);
});

test('edit, move, remove; validation; the state survives a restart', async (t) => {
  const fx = fixture(t);
  const { item } = await fx.queue.add(fx.session, { text: 'one' });
  await fx.queue.add(fx.session, { text: 'two' });
  await fx.queue.edit(fx.session, item.id, { text: 'ONE' });
  let v = await fx.queue.move(fx.session, item.id, { to: 1 });
  assert.deepEqual(v.items.map((i) => i.text), ['two', 'ONE']);
  await assert.rejects(fx.queue.move(fx.session, item.id, { to: 5 }), { status: 400 });
  await assert.rejects(fx.queue.add(fx.session, { text: '  ' }), { status: 400 });
  await assert.rejects(fx.queue.edit(fx.session, 'q-ffffff', { text: 'x' }), { status: 404 });
  await assert.rejects(fx.queue.add({ session_id: 'pid-123' }, { text: 'x' }), { status: 409 });

  v = await fx.make().get(fx.session);
  assert.deepEqual(v.items.map((i) => i.text), ['two', 'ONE']);
  assert.equal(fs.statSync(fx.file).mode & 0o777, 0o600);

  v = await fx.queue.remove(fx.session, item.id);
  assert.deepEqual(v.items.map((i) => i.text), ['two']);
});

test('HTTP: …/queue routes, the chat poll carries the queue, 501 without one', async (t) => {
  const fx = fixture(t);
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  const fleet = { localHost: async () => ({ name: 'laptop', ok: true, sessions: [fx.session] }) };
  const transcripts = { messages: async () => ({ messages: [], total: 0, truncated: false, updatedAt: 0 }) };
  const urls = {};
  for (const [k, queue] of [['on', fx.queue], ['off', null]]) {
    const api = createApi({ config, fleet, backend: {}, transcripts, spawner: {}, queue, warmFleet: false });
    t.after(() => api.stop());
    const server = createHttpServer({ handleApi: api, uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    urls[k] = `http://127.0.0.1:${server.address().port}/api/hosts/laptop/sessions/aaaaaaaa`;
    t.after(() => server.close());
  }
  const call = async (u, method = 'GET', body) => {
    const res = await fetch(u, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
    return { status: res.status, body: await res.json() };
  };
  const q = `${urls.on}/queue`;
  const added = await call(q, 'POST', { text: 'queued' });
  assert.equal(added.status, 200);
  assert.equal(added.body.id, SID);
  const qid = added.body.item.id;
  assert.equal((await call(`${q}/${qid}`, 'PUT', { text: 'edited' })).body.items[0].text, 'edited');
  assert.equal((await call(`${q}/pause`, 'POST', { paused: true })).body.paused, true);
  assert.deepEqual((await call(`${urls.on}/messages`)).body.queue.items.map((i) => i.text), ['edited']);
  assert.equal((await call(`${q}/${qid}`, 'DELETE')).body.items.length, 0);
  assert.equal((await call(`${q}/nope`, 'PUT', { text: 'x' })).status, 400);
  assert.equal((await call(`${q}/pause`, 'GET')).status, 405);
  assert.equal((await call(`${urls.off}/queue`)).status, 501);
});
